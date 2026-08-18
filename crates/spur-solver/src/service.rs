//! Shared solver service, concurrency budget, and typed model decoding.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZeroUsize,
    path::PathBuf,
    str,
    sync::Arc,
    time::Duration,
};

use serde::Serialize;
use thiserror::Error;
use tokio::{
    sync::Semaphore,
    time::{self, Instant},
};

use crate::{
    cache::{fingerprint_request, SolveCache},
    encode::{encode_solve_constraints, EncodeError, SMT_IDENTIFIER_PREFIX},
    persist::{
        validate_solve_id, ArtifactStore, GetSolveResultResponse, PersistError, SolveArtifact,
    },
    process::{
        z3_version_is_supported, ProcessError, ProcessOutcome, ProcessOutput, ProcessRequest,
        ProcessRunner, Z3Process,
    },
    session::{SessionApply, SessionError, SessionStore},
    smt_gate::{validate_smt_script, SmtGateError},
    types::{
        ModelValue, ObjectiveBound, ObjectivePriority, ObjectiveResult, OptimizationResult,
        OptimizationSolution, OptimizationTermination, SessionOp, SoftConstraintResult,
        SoftGroupResult, SolveConstraintsRequest, SolveConstraintsResponse, SolveModel,
        SolveSmtRequest, SolveStatus, Variable, MAX_TIMEOUT_MS,
    },
};

/// Default maximum number of concurrent Z3 children in one shared service.
pub const DEFAULT_MAX_CONCURRENT_SOLVES: usize = 4;

/// Validation failure shared by typed and raw solver requests.
#[derive(Debug, Error)]
pub enum InvalidRequestError {
    /// B′ validation or generated SMT serialization failed.
    #[error(transparent)]
    Constraints(#[from] EncodeError),
    /// The raw SMT-LIB2 gate rejected the complete script.
    #[error(transparent)]
    RawSmt(#[from] SmtGateError),
    /// A raw solve requested more than the process-wide timeout cap.
    #[error("timeout {timeout_ms} ms exceeds maximum {max_timeout_ms} ms")]
    TimeoutTooLarge {
        /// Requested wall-clock budget.
        timeout_ms: u64,
        /// Maximum accepted wall-clock budget.
        max_timeout_ms: u64,
    },
}

/// Transport-facing failure that prevents a solve result envelope.
#[derive(Debug, Error)]
pub enum SolverServiceError {
    /// Request validation or generated-script limits rejected the request.
    #[error("invalid solver request: {source}")]
    InvalidParams {
        /// Typed or raw request validation failure.
        #[source]
        source: InvalidRequestError,
    },
    /// Operator configuration did not resolve a runnable Z3 binary.
    #[error("solver unavailable: {message}")]
    SolverUnavailable {
        /// Installation or discovery diagnostic.
        message: String,
    },
    /// Persistence was requested before the hosting repository root was set.
    #[error("solver persistence requires an explicit repository root")]
    RepoRootNotConfigured,
    /// Persisting or retrieving a repository-local solve artifact failed.
    #[error(transparent)]
    Persistence(#[from] PersistError),
    /// Incremental session operation failed.
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// Process-wide owner of solver concurrency and subprocess execution.
///
/// Construct one service and inject the same instance into brain and worker
/// MCP modules. Clones share both the runner and semaphore.
#[derive(Clone, Debug)]
pub struct SolverService {
    runner: Arc<dyn ProcessRunner>,
    semaphore: Arc<Semaphore>,
    max_concurrent_solves: usize,
    artifacts: Option<ArtifactStore>,
    cache: Arc<SolveCache>,
    sessions: Arc<SessionStore>,
}

impl SolverService {
    /// Creates a lazily-discovered Z3 service with four concurrent permits.
    ///
    /// Construction does not require Z3 to be installed. The first solve
    /// discovers `SPUR_Z3_BIN` and then `PATH`, returning
    /// [`SolverServiceError::SolverUnavailable`] when neither resolves.
    ///
    /// # Examples
    ///
    /// ```
    /// use spur_solver::service::SolverService;
    ///
    /// let service = SolverService::new();
    /// let clone = service.clone();
    /// assert_eq!(service.max_concurrent_solves(), clone.max_concurrent_solves());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::with_runner(Arc::new(Z3Process::new()))
    }

    /// Creates a four-permit service around an injected process runner.
    #[must_use]
    pub fn with_runner(runner: Arc<dyn ProcessRunner>) -> Self {
        Self {
            runner,
            semaphore: Arc::new(Semaphore::new(DEFAULT_MAX_CONCURRENT_SOLVES)),
            max_concurrent_solves: DEFAULT_MAX_CONCURRENT_SOLVES,
            artifacts: None,
            cache: Arc::new(SolveCache::new()),
            sessions: Arc::new(SessionStore::new()),
        }
    }

    /// Creates a service with an explicit non-zero concurrency limit.
    #[must_use]
    pub fn with_runner_and_concurrency(
        runner: Arc<dyn ProcessRunner>,
        max_concurrent_solves: NonZeroUsize,
    ) -> Self {
        Self {
            runner,
            semaphore: Arc::new(Semaphore::new(max_concurrent_solves.get())),
            max_concurrent_solves: max_concurrent_solves.get(),
            artifacts: None,
            cache: Arc::new(SolveCache::new()),
            sessions: Arc::new(SessionStore::new()),
        }
    }

    /// Configures the repository root used for `.spur/solver/` artifacts.
    ///
    /// Clones created after this call share the same repository-local store and
    /// quota lock. Existing clones retain their original store.
    #[must_use]
    pub fn with_repo_root(mut self, repo_root: impl Into<PathBuf>) -> Self {
        self.artifacts = Some(ArtifactStore::for_repo_root(repo_root));
        self
    }

    /// Returns the configured maximum number of concurrent solver children.
    #[must_use]
    pub const fn max_concurrent_solves(&self) -> usize {
        self.max_concurrent_solves
    }

    /// Persists a solve response as a schema-v1 handoff artifact.
    ///
    /// This cache does not replace Beads as the collaboration source of truth.
    /// The returned artifact contains the generated traversal-safe `solve_id`.
    ///
    /// # Errors
    ///
    /// Returns [`SolverServiceError::Persistence`] if serialization, quota
    /// enforcement, or the atomic write fails.
    pub fn persist<T: Serialize>(
        &self,
        request: &T,
        response: &SolveConstraintsResponse,
    ) -> Result<SolveArtifact, SolverServiceError> {
        let z3_version = self.runner.solver_version();
        self.artifacts
            .as_ref()
            .ok_or(SolverServiceError::RepoRootNotConfigured)?
            .persist(request, response, &z3_version)
            .map_err(SolverServiceError::from)
    }

    /// Returns the operator Z3 version string known to the process runner.
    #[must_use]
    pub fn z3_version(&self) -> String {
        self.runner.solver_version()
    }

    /// Loads a persisted solve artifact by its pinned identifier.
    ///
    /// The identifier is validated before it is joined to the configured
    /// repository root.
    ///
    /// # Errors
    ///
    /// Returns [`SolverServiceError::Persistence`] for malformed identifiers,
    /// missing artifacts, invalid payloads, or filesystem failures.
    pub fn get_solve_result(
        &self,
        solve_id: &str,
    ) -> Result<GetSolveResultResponse, SolverServiceError> {
        validate_solve_id(solve_id)?;
        self.artifacts
            .as_ref()
            .ok_or(SolverServiceError::RepoRootNotConfigured)?
            .get(solve_id)
            .map_err(SolverServiceError::from)
    }

    /// Encodes and solves one typed B′ constraint request.
    ///
    /// The request's single wall-clock budget starts before encoding and also
    /// includes time spent waiting for a semaphore permit. `unknown` and
    /// `timeout` remain distinct successful result statuses.
    ///
    /// # Errors
    ///
    /// Returns [`SolverServiceError::InvalidParams`] for validation/encoding
    /// failures, or [`SolverServiceError::SolverUnavailable`] when operator
    /// discovery cannot launch Z3. Process, output, and parse failures are
    /// represented by [`SolveStatus::Error`] result envelopes.
    pub async fn solve_constraints(
        &self,
        mut request: SolveConstraintsRequest,
    ) -> Result<SolveConstraintsResponse, SolverServiceError> {
        let started = Instant::now();
        let mut response_session_id = None;
        match self.sessions.apply(&request)? {
            SessionApply::Stateless => {}
            SessionApply::Ended { session_id } => {
                // Protocol: `ended` is not a feasible model. Agents must not
                // treat it as sat or invent constants from an empty model.
                let mut response = response(
                    SolveStatus::Ended,
                    None,
                    started,
                    Some("session ended".to_owned()),
                    None,
                );
                response.session_id = Some(session_id);
                return self.finish_response(
                    &request,
                    request.persist,
                    request.include_smt,
                    None,
                    response,
                );
            }
            SessionApply::Solve {
                session_id,
                constraints,
                vars,
            } => {
                response_session_id = session_id;
                request.vars = vars;
                request.constraints = constraints;
            }
        }

        if is_optimization_request(&request) {
            let version = self.runner.solver_version();
            if !z3_version_is_supported(&version) {
                return Err(SolverServiceError::SolverUnavailable {
                    message: format!(
                        "{version} is unsupported for optimization; install Z3 4.8.12 or newer"
                    ),
                });
            }
        }

        let cacheable = request.use_cache
            && matches!(request.session_op, SessionOp::None)
            && request.session_id.is_none();
        if cacheable {
            if let Ok(key) = fingerprint_request(&request) {
                if let Some(mut cached) = self.cache.get(&key) {
                    cached.cached = true;
                    cached.duration_ms = 0;
                    cached.smt = None;
                    cached.solve_id = None;
                    return self.finish_response(
                        &request,
                        request.persist,
                        request.include_smt,
                        None,
                        cached,
                    );
                }
            }
        }

        let smt = encode_solve_constraints(&request).map_err(|source| {
            SolverServiceError::InvalidParams {
                source: source.into(),
            }
        })?;
        let deadline = started + Duration::from_millis(request.timeout_ms);
        let include_smt = request.include_smt;
        let smt_for_echo = if include_smt { Some(smt.clone()) } else { None };

        let mut response = match self.run_script(smt, deadline).await? {
            ServiceRunOutcome::TimedOut => timeout_response(started),
            ServiceRunOutcome::Completed(output) => response_from_output(&request, output, started),
            ServiceRunOutcome::Error(message) => error_response(started, message),
        };
        response.session_id = response_session_id;

        // Cache only decisive outcomes. Never cache top-level or optimization
        // incompleteness because it must not sticky-replay across later solves.
        let optimization_is_decisive = response.optimization.as_ref().is_none_or(|optimization| {
            optimization.termination != OptimizationTermination::Unknown
        });
        if cacheable
            && matches!(response.status, SolveStatus::Sat | SolveStatus::Unsat)
            && optimization_is_decisive
        {
            if let Ok(key) = fingerprint_request(&request) {
                let mut to_store = response.clone();
                to_store.cached = false;
                to_store.smt = None;
                to_store.solve_id = None;
                to_store.session_id = None;
                self.cache.insert(key, to_store);
            }
        }

        self.finish_response(
            &request,
            request.persist,
            include_smt,
            smt_for_echo,
            response,
        )
    }

    /// Gates and solves one raw SMT-LIB2 request without B′ validation.
    ///
    /// Accepted scripts are passed byte-for-byte to the same process-wide
    /// semaphore, deadline, and fixed-argv runner used by
    /// [`Self::solve_constraints`].
    ///
    /// # Errors
    ///
    /// Returns [`SolverServiceError::InvalidParams`] when the timeout exceeds
    /// the shared cap or the reject-only raw SMT gate rejects the script.
    /// Returns [`SolverServiceError::SolverUnavailable`] when operator binary
    /// discovery cannot launch Z3. Process, output, and parse failures are
    /// represented by [`SolveStatus::Error`] result envelopes.
    pub async fn solve_smt(
        &self,
        request: SolveSmtRequest,
    ) -> Result<SolveConstraintsResponse, SolverServiceError> {
        let started = Instant::now();
        if request.timeout_ms > MAX_TIMEOUT_MS {
            return Err(SolverServiceError::InvalidParams {
                source: InvalidRequestError::TimeoutTooLarge {
                    timeout_ms: request.timeout_ms,
                    max_timeout_ms: MAX_TIMEOUT_MS,
                },
            });
        }
        validate_smt_script(&request.smt_lib).map_err(|source| {
            SolverServiceError::InvalidParams {
                source: source.into(),
            }
        })?;

        let deadline = started + Duration::from_millis(request.timeout_ms);
        let include_smt = request.include_smt;
        let smt_for_echo = if include_smt {
            Some(request.smt_lib.clone())
        } else {
            None
        };
        let response = match self.run_script(request.smt_lib.clone(), deadline).await? {
            ServiceRunOutcome::TimedOut => timeout_response(started),
            ServiceRunOutcome::Completed(output) => response_from_raw_output(output, started),
            ServiceRunOutcome::Error(message) => error_response(started, message),
        };
        self.finish_response(
            &request,
            request.persist,
            include_smt,
            smt_for_echo,
            response,
        )
    }

    async fn run_script(
        &self,
        smt: String,
        deadline: Instant,
    ) -> Result<ServiceRunOutcome, SolverServiceError> {
        if Instant::now() >= deadline {
            return Ok(ServiceRunOutcome::TimedOut);
        }

        let permit = match time::timeout_at(deadline, self.semaphore.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_closed)) => {
                return Ok(ServiceRunOutcome::Error(
                    "solver concurrency semaphore is closed".to_owned(),
                ));
            }
            Err(_elapsed) => return Ok(ServiceRunOutcome::TimedOut),
        };

        let outcome = self.runner.run(ProcessRequest::new(smt, deadline)).await;
        drop(permit);

        match outcome {
            Ok(ProcessOutcome::TimedOut) => Ok(ServiceRunOutcome::TimedOut),
            Ok(ProcessOutcome::Completed(output)) => Ok(ServiceRunOutcome::Completed(output)),
            Err(ProcessError::SolverUnavailable { message }) => {
                Err(SolverServiceError::SolverUnavailable { message })
            }
            Err(error) => Ok(ServiceRunOutcome::Error(error.to_string())),
        }
    }

    fn finish_response<T: Serialize>(
        &self,
        request: &T,
        persist: bool,
        include_smt: bool,
        smt: Option<String>,
        mut response: SolveConstraintsResponse,
    ) -> Result<SolveConstraintsResponse, SolverServiceError> {
        let version = self.runner.solver_version();
        response.solver_version =
            (version != crate::persist::UNKNOWN_Z3_VERSION).then_some(version);
        if include_smt {
            response.smt = smt;
        }
        if persist {
            let artifact = self.persist(request, &response)?;
            response.solve_id = Some(artifact.solve_id);
        }
        Ok(response)
    }
}

#[derive(Debug)]
enum ServiceRunOutcome {
    TimedOut,
    Completed(ProcessOutput),
    Error(String),
}

impl Default for SolverService {
    fn default() -> Self {
        Self::new()
    }
}

fn response_from_output(
    request: &SolveConstraintsRequest,
    output: crate::process::ProcessOutput,
    started: Instant,
) -> SolveConstraintsResponse {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = match str::from_utf8(&output.stdout) {
        Ok(stdout) => stdout,
        Err(error) => {
            return error_response(
                started,
                with_optional_stderr(format!("Z3 stdout was not UTF-8: {error}"), stderr.as_ref()),
            );
        }
    };
    let parsed = parse_typed_solver_output(stdout, request);
    let expected_get_value_failure = output.exit_code == Some(1)
        && parsed
            .as_ref()
            .is_ok_and(|parsed| parsed.expected_get_value_failure);

    // Prefer a successfully parsed status over stderr noise. Hard-fail only
    // when the process failed without a known post-unsat get-value error and
    // the stdout parse also failed.
    if !output.success && !expected_get_value_failure && parsed.is_err() {
        let exit = output
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| format!("exit code {code}"));
        return error_response(
            started,
            with_optional_stderr(
                format!("Z3 exited unsuccessfully ({exit})"),
                stderr.as_ref(),
            ),
        );
    }

    match parsed {
        Ok(ParsedSolverOutput {
            solve: ParsedSolve::Sat(model),
            optimization,
            ..
        }) => {
            let mut response = response(SolveStatus::Sat, Some(model), started, None, None);
            response.optimization = optimization;
            response
        }
        Ok(ParsedSolverOutput {
            solve: ParsedSolve::Unsat { unsat_core },
            ..
        }) => response(SolveStatus::Unsat, None, started, None, unsat_core),
        Ok(ParsedSolverOutput {
            solve: ParsedSolve::Unknown,
            ..
        }) => response(
            SolveStatus::Unknown,
            None,
            started,
            Some("Z3 returned unknown".to_owned()),
            None,
        ),
        Err(error) => error_response(
            started,
            with_optional_stderr(
                format!("failed to parse Z3 output: {error}"),
                stderr.as_ref(),
            ),
        ),
    }
}

fn response_from_raw_output(output: ProcessOutput, started: Instant) -> SolveConstraintsResponse {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = match str::from_utf8(&output.stdout) {
        Ok(stdout) => stdout,
        Err(error) => {
            return error_response(
                started,
                with_optional_stderr(format!("Z3 stdout was not UTF-8: {error}"), stderr.as_ref()),
            );
        }
    };
    let parsed = parse_raw_solver_output(stdout);
    let expected_model_failure = output.exit_code == Some(1)
        && parsed
            .as_ref()
            .is_ok_and(|parsed| parsed.expected_get_value_failure);
    if !output.success && !expected_model_failure && parsed.is_err() {
        let exit = output
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| format!("exit code {code}"));
        return error_response(
            started,
            with_optional_stderr(
                format!("Z3 exited unsuccessfully ({exit})"),
                stderr.as_ref(),
            ),
        );
    }

    match parsed {
        Ok(ParsedSolverOutput {
            solve: ParsedSolve::Sat(model),
            optimization,
            ..
        }) => {
            let mut response = response(SolveStatus::Sat, Some(model), started, None, None);
            response.optimization = optimization;
            response
        }
        Ok(ParsedSolverOutput {
            solve: ParsedSolve::Unsat { unsat_core },
            ..
        }) => response(SolveStatus::Unsat, None, started, None, unsat_core),
        Ok(ParsedSolverOutput {
            solve: ParsedSolve::Unknown,
            ..
        }) => response(
            SolveStatus::Unknown,
            None,
            started,
            Some("Z3 returned unknown".to_owned()),
            None,
        ),
        Err(error) => error_response(
            started,
            with_optional_stderr(
                format!("failed to parse Z3 output: {error}"),
                stderr.as_ref(),
            ),
        ),
    }
}

fn response(
    status: SolveStatus,
    model: Option<SolveModel>,
    started: Instant,
    reason: Option<String>,
    unsat_core: Option<Vec<String>>,
) -> SolveConstraintsResponse {
    SolveConstraintsResponse {
        status,
        model,
        duration_ms: elapsed_millis(started),
        solve_id: None,
        reason,
        smt: None,
        unsat_core,
        cached: false,
        session_id: None,
        optimization: None,
        solver_version: None,
    }
}

fn with_optional_stderr(message: String, stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        message
    } else {
        format!("{message}; stderr: {}", diagnostic_text(trimmed))
    }
}

fn timeout_response(started: Instant) -> SolveConstraintsResponse {
    response(
        SolveStatus::Timeout,
        None,
        started,
        Some("wall-clock solve budget exhausted".to_owned()),
        None,
    )
}

fn error_response(started: Instant, reason: String) -> SolveConstraintsResponse {
    response(SolveStatus::Error, None, started, Some(reason), None)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn diagnostic_text(text: &str) -> String {
    const MAX_DIAGNOSTIC_CHARS: usize = 4_096;

    let trimmed = text.trim();
    let mut characters = trimmed.chars();
    let diagnostic: String = characters.by_ref().take(MAX_DIAGNOSTIC_CHARS).collect();
    if characters.next().is_some() {
        format!("{diagnostic}…")
    } else {
        diagnostic
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ParsedSolve {
    Sat(SolveModel),
    Unsat { unsat_core: Option<Vec<String>> },
    Unknown,
}

#[derive(Debug, Eq, PartialEq)]
struct ParsedSolverOutput {
    solve: ParsedSolve,
    optimization: Option<OptimizationResult>,
    expected_get_value_failure: bool,
}

fn is_optimization_request(request: &SolveConstraintsRequest) -> bool {
    request.has_soft_constraints() || request.has_objectives()
}

fn parse_typed_solver_output(
    stdout: &str,
    request: &SolveConstraintsRequest,
) -> Result<ParsedSolverOutput, ParseError> {
    if is_optimization_request(request) {
        parse_optimization_output(stdout, request)
    } else {
        parse_solver_output(stdout, &request.vars, request.wants_unsat_cores())
    }
}

fn parse_solver_output(
    stdout: &str,
    variables: &[Variable],
    want_cores: bool,
) -> Result<ParsedSolverOutput, ParseError> {
    let forms = SExpressionParser::new(stdout).parse_all()?;
    let status = forms
        .first()
        .and_then(SExpression::as_atom)
        .ok_or_else(|| ParseError::new("expected status atom as first output form"))?;

    match status {
        "sat" => Ok(ParsedSolverOutput {
            solve: parse_sat_output(&forms, variables, want_cores)?,
            optimization: None,
            expected_get_value_failure: false,
        }),
        "unsat" => {
            let (expected_get_value_failure, unsat_core) =
                parse_unsat_output(&forms, variables, want_cores)?;
            Ok(ParsedSolverOutput {
                solve: ParsedSolve::Unsat { unsat_core },
                optimization: None,
                expected_get_value_failure,
            })
        }
        "unknown" => {
            let expected_get_value_failure = require_non_sat_output(&forms, variables, "unknown")?;
            Ok(ParsedSolverOutput {
                solve: ParsedSolve::Unknown,
                optimization: None,
                expected_get_value_failure,
            })
        }
        other => Err(ParseError::new(format!(
            "unexpected solver status `{other}`"
        ))),
    }
}

fn parse_optimization_output(
    stdout: &str,
    request: &SolveConstraintsRequest,
) -> Result<ParsedSolverOutput, ParseError> {
    let forms = SExpressionParser::new(stdout).parse_all()?;
    let cycle_count = optimization_cycle_count(request);
    let objective_count = optimization_objective_count(request);
    let mut cursor = 0;
    let mut solutions = Vec::new();
    let mut semantic_termination = None;
    let mut expected_get_value_failure = false;

    for cycle_index in 0..cycle_count {
        if cursor >= forms.len() {
            if semantic_termination.is_some() {
                break;
            }
            return Err(ParseError::new(format!(
                "missing status for optimization cycle {}",
                cycle_index + 1
            )));
        }
        let status = optimization_status_at(&forms, cursor, "optimization cycle")?;
        cursor += 1;

        if semantic_termination.is_none() {
            match status {
                "sat" => {
                    let objectives = forms.get(cursor).ok_or_else(|| {
                        ParseError::new("sat optimization cycle missing objectives response")
                    })?;
                    cursor += 1;
                    let values = forms.get(cursor).ok_or_else(|| {
                        ParseError::new(
                            "sat optimization cycle missing combined get-value response",
                        )
                    })?;
                    cursor += 1;
                    solutions.push(parse_optimization_solution(
                        request,
                        objectives,
                        values,
                        objective_count,
                    )?);
                    continue;
                }
                "unsat" => semantic_termination = Some(OptimizationTermination::Complete),
                "unknown" => semantic_termination = Some(OptimizationTermination::Unknown),
                other => {
                    return Err(ParseError::new(format!(
                        "unexpected solver status `{other}` in optimization cycle"
                    )))
                }
            }
        }

        // Every `sat` status denotes a complete optimization cycle, even after
        // an earlier non-sat status has already established termination.
        if status == "sat" || cursor < forms.len() && forms[cursor].as_atom().is_none() {
            consume_ignored_optimization_payload(
                &forms,
                &mut cursor,
                status,
                request,
                objective_count,
                &mut expected_get_value_failure,
            )?;
        }
    }

    let mut termination = semantic_termination;
    if cursor < forms.len() {
        let terminal = optimization_status_at(&forms, cursor, "optimization terminal probe")?;
        cursor += 1;
        if termination.is_none() {
            termination = Some(match (request.objective_priority, terminal) {
                (ObjectivePriority::Pareto, "sat") => OptimizationTermination::SolutionLimit,
                (ObjectivePriority::Pareto | ObjectivePriority::Box, "unsat") => {
                    OptimizationTermination::Complete
                }
                (ObjectivePriority::Pareto | ObjectivePriority::Box, "unknown") => {
                    OptimizationTermination::Unknown
                }
                (ObjectivePriority::Box, "sat") => {
                    return Err(ParseError::new(
                        "box terminal probe unexpectedly returned sat",
                    ))
                }
                (ObjectivePriority::Lex, _) => {
                    return Err(ParseError::new(
                        "unexpected terminal status after lex cycle",
                    ))
                }
                (_, other) => {
                    return Err(ParseError::new(format!(
                        "unexpected optimization terminal status `{other}`"
                    )))
                }
            });
        }
    }

    if cursor != forms.len() {
        return Err(ParseError::new("unexpected extra optimization output form"));
    }

    if solutions.is_empty() {
        return match termination {
            Some(OptimizationTermination::Unknown) => Ok(ParsedSolverOutput {
                solve: ParsedSolve::Unknown,
                optimization: None,
                expected_get_value_failure,
            }),
            Some(OptimizationTermination::Complete) => Ok(ParsedSolverOutput {
                solve: ParsedSolve::Unsat { unsat_core: None },
                optimization: None,
                expected_get_value_failure,
            }),
            Some(OptimizationTermination::SolutionLimit) | None => Err(ParseError::new(
                "optimization completed without a satisfiable solution",
            )),
        };
    }

    let termination = termination.unwrap_or(OptimizationTermination::Complete);
    let model = solutions[0].model.clone();
    Ok(ParsedSolverOutput {
        solve: ParsedSolve::Sat(model),
        optimization: Some(OptimizationResult {
            priority: Some(request.objective_priority),
            solutions,
            termination,
        }),
        expected_get_value_failure,
    })
}

fn status_at(forms: &[SExpression], cursor: usize) -> Result<&str, ParseError> {
    forms
        .get(cursor)
        .and_then(SExpression::as_atom)
        .ok_or_else(|| ParseError::new(format!("expected status atom at output form {cursor}")))
}

fn optimization_status_at<'a>(
    forms: &'a [SExpression],
    cursor: usize,
    context: &str,
) -> Result<&'a str, ParseError> {
    let status = status_at(forms, cursor)?;
    if matches!(status, "sat" | "unsat" | "unknown") {
        Ok(status)
    } else {
        Err(ParseError::new(format!(
            "unexpected solver status `{status}` in {context}"
        )))
    }
}

fn consume_ignored_optimization_payload(
    forms: &[SExpression],
    cursor: &mut usize,
    status: &str,
    request: &SolveConstraintsRequest,
    objective_count: usize,
    expected_get_value_failure: &mut bool,
) -> Result<(), ParseError> {
    let objectives = forms
        .get(*cursor)
        .ok_or_else(|| ParseError::new("missing ignored objectives response"))?;
    objective_entries(objectives, Some(objective_count))?;
    *cursor += 1;

    let values = forms
        .get(*cursor)
        .ok_or_else(|| ParseError::new("missing ignored combined get-value response"))?;
    if values.is_model_unavailable_error() {
        if status == "sat" {
            return Err(ParseError::new(
                "sat optimization cycle returned a model-unavailable error",
            ));
        }
        *expected_get_value_failure = true;
    } else {
        if status != "sat" {
            return Err(ParseError::new(format!(
                "{status} optimization cycle unexpectedly returned model values"
            )));
        }
        let expected_values = request.vars.len()
            + request
                .constraints
                .iter()
                .filter(|constraint| constraint.is_soft())
                .count()
            + request.objectives.len();
        combined_value_bindings(values, expected_values)?;
    }
    *cursor += 1;
    Ok(())
}

fn optimization_cycle_count(request: &SolveConstraintsRequest) -> usize {
    match request.objective_priority {
        ObjectivePriority::Lex => 1,
        ObjectivePriority::Pareto => request.max_solutions,
        ObjectivePriority::Box => optimization_objective_count(request),
    }
}

fn optimization_objective_count(request: &SolveConstraintsRequest) -> usize {
    soft_group_order(request).len() + request.objectives.len()
}

fn soft_group_order(request: &SolveConstraintsRequest) -> Vec<Option<&str>> {
    let mut groups = Vec::new();
    for constraint in request
        .constraints
        .iter()
        .filter(|constraint| constraint.is_soft())
    {
        let group = constraint.group();
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups
}

fn parse_optimization_solution(
    request: &SolveConstraintsRequest,
    objectives_form: &SExpression,
    values_form: &SExpression,
    objective_count: usize,
) -> Result<OptimizationSolution, ParseError> {
    let entries = objective_entries(objectives_form, Some(objective_count))?;
    let soft_groups = soft_group_order(request);
    validate_soft_objective_entries(entries, &soft_groups)?;

    let soft_constraints: Vec<_> = request
        .constraints
        .iter()
        .enumerate()
        .filter(|(_, constraint)| constraint.is_soft())
        .collect();
    let expected_values = request.vars.len() + soft_constraints.len() + request.objectives.len();
    let bindings = combined_value_bindings(values_form, expected_values)?;

    let mut model = BTreeMap::new();
    for (variable, binding) in request.vars.iter().zip(bindings.iter()) {
        let symbol = binding[0]
            .as_atom()
            .ok_or_else(|| ParseError::new("declared-variable binding must use a symbol"))?;
        let expected = mangled_symbol(variable.name());
        if symbol != expected {
            return Err(ParseError::new(format!(
                "expected declared-variable symbol `{expected}`, found `{symbol}`"
            )));
        }
        if model
            .insert(
                variable.name().to_owned(),
                parse_model_value(variable, &binding[1])?,
            )
            .is_some()
        {
            return Err(ParseError::new(format!(
                "duplicate declared-variable binding `{symbol}`"
            )));
        }
    }

    let soft_start = request.vars.len();
    let mut soft_results = Vec::with_capacity(soft_constraints.len());
    for ((index, constraint), binding) in soft_constraints.iter().zip(bindings[soft_start..].iter())
    {
        let satisfied = match binding[1].as_atom() {
            Some("true") => true,
            Some("false") => false,
            _ => {
                return Err(ParseError::new(format!(
                    "soft constraint {index} returned a non-Boolean value"
                )))
            }
        };
        soft_results.push(SoftConstraintResult {
            index: *index,
            id: constraint.id().map(str::to_owned),
            group: constraint.group().map(str::to_owned),
            weight: constraint.soft_weight().ok_or_else(|| {
                ParseError::new(format!("soft constraint {index} is missing a weight"))
            })?,
            satisfied,
        });
    }

    let objective_start = soft_start + soft_constraints.len();
    let explicit_entries = &entries[soft_groups.len()..];
    let mut objective_results = Vec::with_capacity(request.objectives.len());
    for ((objective, entry), binding) in request
        .objectives
        .iter()
        .zip(explicit_entries)
        .zip(bindings[objective_start..].iter())
    {
        let fields = entry
            .as_list()
            .ok_or_else(|| ParseError::new("explicit objective entry must be a list"))?;
        if fields.len() != 2 {
            return Err(ParseError::new(
                "explicit objective entry must contain expression and bound",
            ));
        }
        if fields[0] != binding[0] {
            return Err(ParseError::new(
                "objective response expression does not match get-value expression",
            ));
        }
        objective_results.push(ObjectiveResult {
            op: Some(objective.op),
            value: parse_raw_model_value(&binding[1]),
            bound: classify_bound(&fields[1]),
        });
    }

    Ok(OptimizationSolution {
        model,
        objectives: objective_results,
        soft_constraints: soft_results.clone(),
        groups: aggregate_soft_groups(&soft_results)?,
    })
}

fn objective_entries(
    form: &SExpression,
    expected_count: Option<usize>,
) -> Result<&[SExpression], ParseError> {
    let values = form
        .as_list()
        .ok_or_else(|| ParseError::new("objectives response must be a list"))?;
    if values.first().and_then(SExpression::as_atom) != Some("objectives") {
        return Err(ParseError::new(
            "expected objectives response before combined get-value response",
        ));
    }
    let entries = &values[1..];
    if let Some(expected_count) = expected_count {
        if entries.len() != expected_count {
            return Err(ParseError::new(format!(
                "objectives response returned {} entries, expected {expected_count}",
                entries.len()
            )));
        }
    }
    Ok(entries)
}

fn is_objectives_form(form: &SExpression) -> bool {
    form.as_list()
        .and_then(|values| values.first())
        .and_then(SExpression::as_atom)
        == Some("objectives")
}

fn validate_soft_objective_entries(
    entries: &[SExpression],
    groups: &[Option<&str>],
) -> Result<(), ParseError> {
    for (entry, group) in entries.iter().zip(groups) {
        let fields = entry
            .as_list()
            .ok_or_else(|| ParseError::new("soft objective entry must be a list"))?;
        match group {
            None if fields.len() == 1 => {}
            Some(expected) if fields.len() == 2 && fields[0].as_atom() == Some(*expected) => {}
            None => {
                return Err(ParseError::new(
                    "anonymous soft objective entry must contain exactly one bound",
                ))
            }
            Some(expected) => {
                return Err(ParseError::new(format!(
                    "expected soft objective group `{expected}`"
                )))
            }
        }
    }
    Ok(())
}

fn combined_value_bindings(
    form: &SExpression,
    expected_count: usize,
) -> Result<Vec<&[SExpression]>, ParseError> {
    let values = form
        .as_list()
        .ok_or_else(|| ParseError::new("combined get-value response must be a list"))?;
    if values.len() != expected_count {
        return Err(ParseError::new(format!(
            "combined get-value returned {} bindings, expected {expected_count}",
            values.len()
        )));
    }
    values
        .iter()
        .map(|binding| {
            let pair = binding
                .as_list()
                .ok_or_else(|| ParseError::new("combined get-value binding must be a pair"))?;
            if pair.len() != 2 {
                return Err(ParseError::new(
                    "combined get-value binding must contain expression and value",
                ));
            }
            Ok(pair)
        })
        .collect()
}

fn aggregate_soft_groups(
    soft_constraints: &[SoftConstraintResult],
) -> Result<Vec<SoftGroupResult>, ParseError> {
    let mut groups: Vec<SoftGroupResult> = Vec::new();
    for constraint in soft_constraints {
        let position = groups
            .iter()
            .position(|group| group.group == constraint.group)
            .unwrap_or_else(|| {
                groups.push(SoftGroupResult {
                    group: constraint.group.clone(),
                    satisfied_weight: 0,
                    violated_weight: 0,
                });
                groups.len() - 1
            });
        let total = if constraint.satisfied {
            &mut groups[position].satisfied_weight
        } else {
            &mut groups[position].violated_weight
        };
        *total = total.checked_add(constraint.weight).ok_or_else(|| {
            ParseError::new("soft group weight total exceeds signed 64-bit range")
        })?;
    }
    Ok(groups)
}

fn classify_bound(expression: &SExpression) -> ObjectiveBound {
    let exact = render_s_expression(expression);
    if expression.contains_atom("oo") {
        ObjectiveBound::Infinite { exact }
    } else if expression.contains_atom("epsilon") {
        ObjectiveBound::Strict { exact }
    } else {
        ObjectiveBound::Finite { exact }
    }
}

fn parse_raw_solver_output(stdout: &str) -> Result<ParsedSolverOutput, ParseError> {
    let forms = SExpressionParser::new(stdout).parse_all()?;
    let status = forms
        .first()
        .and_then(SExpression::as_atom)
        .ok_or_else(|| ParseError::new("expected status atom as first output form"))?;

    match status {
        "sat" => {
            let (solve, optimization) = parse_raw_sat_output(&forms)?;
            Ok(ParsedSolverOutput {
                solve,
                optimization,
                expected_get_value_failure: false,
            })
        }
        "unsat" => Ok(ParsedSolverOutput {
            solve: ParsedSolve::Unsat {
                unsat_core: extract_raw_unsat_core(&forms),
            },
            optimization: None,
            expected_get_value_failure: require_raw_non_sat_output(&forms, "unsat")?,
        }),
        "unknown" => Ok(ParsedSolverOutput {
            solve: ParsedSolve::Unknown,
            optimization: None,
            expected_get_value_failure: require_raw_non_sat_output(&forms, "unknown")?,
        }),
        other => Err(ParseError::new(format!(
            "unexpected solver status `{other}`"
        ))),
    }
}

fn extract_raw_unsat_core(forms: &[SExpression]) -> Option<Vec<String>> {
    forms.iter().skip(1).find_map(|form| {
        if is_objectives_form(form) {
            return None;
        }
        let values = form.as_list()?;
        // A core is a list of atoms (assertion names). Skip error forms and models.
        if values.iter().all(|item| item.as_atom().is_some()) {
            Some(
                values
                    .iter()
                    .filter_map(SExpression::as_atom)
                    .map(str::to_owned)
                    .collect(),
            )
        } else {
            None
        }
    })
}

fn parse_raw_sat_output(
    forms: &[SExpression],
) -> Result<(ParsedSolve, Option<OptimizationResult>), ParseError> {
    let mut model_form = None;
    let mut objectives_form = None;
    for form in forms.iter().skip(1) {
        if is_objectives_form(form) {
            if objectives_form.replace(form).is_some() {
                return Err(ParseError::new(
                    "raw sat output must contain at most one objectives response",
                ));
            }
        } else if model_form.replace(form).is_some() {
            return Err(ParseError::new(
                "raw sat output must contain at most one model or get-value response",
            ));
        }
    }

    let model = model_form
        .map(parse_raw_model)
        .transpose()?
        .unwrap_or_default();
    let optimization = if let Some(objectives_form) = objectives_form {
        let objectives = raw_objective_results(objectives_form)?;
        let solution = OptimizationSolution {
            model: model.clone(),
            objectives,
            soft_constraints: Vec::new(),
            groups: Vec::new(),
        };
        Some(OptimizationResult {
            priority: None,
            solutions: vec![solution],
            termination: OptimizationTermination::Complete,
        })
    } else {
        None
    };

    Ok((ParsedSolve::Sat(model), optimization))
}

fn raw_objective_results(form: &SExpression) -> Result<Vec<ObjectiveResult>, ParseError> {
    objective_entries(form, None)?
        .iter()
        .map(|entry| {
            let values = entry
                .as_list()
                .ok_or_else(|| ParseError::new("objective entry must be a list"))?;
            let bound = values
                .last()
                .ok_or_else(|| ParseError::new("objective entry must contain a bound"))?;
            Ok(ObjectiveResult {
                op: None,
                value: parse_raw_model_value(bound),
                bound: classify_bound(bound),
            })
        })
        .collect()
}

fn parse_raw_model(form: &SExpression) -> Result<SolveModel, ParseError> {
    let values = form
        .as_list()
        .ok_or_else(|| ParseError::new("raw model response must be a list"))?;
    if values.is_empty() {
        return Ok(BTreeMap::new());
    }

    if values.first().and_then(SExpression::as_atom) == Some("model") {
        return parse_raw_definitions(&values[1..]);
    }
    if values.iter().all(is_define_fun) {
        return parse_raw_definitions(values);
    }
    parse_raw_get_value_bindings(values)
}

fn is_define_fun(expression: &SExpression) -> bool {
    expression
        .as_list()
        .and_then(|values| values.first())
        .and_then(SExpression::as_atom)
        == Some("define-fun")
}

fn parse_raw_definitions(definitions: &[SExpression]) -> Result<SolveModel, ParseError> {
    let mut model = BTreeMap::new();
    for definition in definitions {
        let fields = definition
            .as_list()
            .ok_or_else(|| ParseError::new("model definition must be a list"))?;
        if fields.len() != 5 || fields[0].as_atom() != Some("define-fun") {
            return Err(ParseError::new(
                "model definition must be a five-field define-fun",
            ));
        }
        let name = fields[1]
            .as_atom()
            .ok_or_else(|| ParseError::new("model definition name must be an atom"))?;
        let arguments = fields[2]
            .as_list()
            .ok_or_else(|| ParseError::new("model definition arguments must be a list"))?;
        if !arguments.is_empty() {
            continue;
        }
        insert_raw_binding(&mut model, name.to_owned(), &fields[4])?;
    }
    Ok(model)
}

fn parse_raw_get_value_bindings(bindings: &[SExpression]) -> Result<SolveModel, ParseError> {
    let mut model = BTreeMap::new();
    for binding in bindings {
        let pair = binding
            .as_list()
            .ok_or_else(|| ParseError::new("get-value binding must be a pair"))?;
        if pair.len() != 2 {
            return Err(ParseError::new(
                "get-value binding must contain expression and value",
            ));
        }
        let key = pair[0]
            .as_atom()
            .map_or_else(|| render_s_expression(&pair[0]), str::to_owned);
        insert_raw_binding(&mut model, key, &pair[1])?;
    }
    Ok(model)
}

fn insert_raw_binding(
    model: &mut SolveModel,
    key: String,
    value: &SExpression,
) -> Result<(), ParseError> {
    if model
        .insert(key.clone(), parse_raw_model_value(value))
        .is_some()
    {
        return Err(ParseError::new(format!(
            "raw model returned duplicate binding `{key}`"
        )));
    }
    Ok(())
}

fn parse_raw_model_value(value: &SExpression) -> ModelValue {
    match value.as_atom() {
        Some("true") => return ModelValue::Bool(true),
        Some("false") => return ModelValue::Bool(false),
        _ => {}
    }
    if let Ok(integer) = parse_integer(value) {
        return ModelValue::Int(integer);
    }
    match value {
        SExpression::String(value) => ModelValue::Enum(value.clone()),
        SExpression::Atom(_) | SExpression::List(_) => ModelValue::Enum(render_s_expression(value)),
    }
}

fn render_s_expression(expression: &SExpression) -> String {
    match expression {
        SExpression::Atom(value) => value.clone(),
        SExpression::String(value) => {
            format!("\"{}\"", value.replace('"', "\"\""))
        }
        SExpression::List(values) => {
            let mut rendered = String::from("(");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    rendered.push(' ');
                }
                rendered.push_str(&render_s_expression(value));
            }
            rendered.push(')');
            rendered
        }
    }
}

fn require_raw_non_sat_output(forms: &[SExpression], status: &str) -> Result<bool, ParseError> {
    if forms.len() == 1 {
        return Ok(false);
    }
    // Accept model-unavailable errors and optional unsat-core atom lists.
    let mut saw_model_error = false;
    let mut saw_objectives = false;
    for form in forms.iter().skip(1) {
        if form.is_model_unavailable_error() || form.is_unsat_core_unavailable_error() {
            saw_model_error = true;
            continue;
        }
        if is_objectives_form(form) {
            if saw_objectives {
                return Err(ParseError::new(format!(
                    "multiple objectives responses after `{status}` status"
                )));
            }
            raw_objective_results(form)?;
            saw_objectives = true;
            continue;
        }
        if form
            .as_list()
            .is_some_and(|values| values.iter().all(|item| item.as_atom().is_some()))
        {
            // bare unsat core list (optional on raw scripts)
            continue;
        }
        return Err(ParseError::new(format!(
            "unexpected output after `{status}` status"
        )));
    }
    Ok(saw_model_error)
}

fn require_non_sat_output(
    forms: &[SExpression],
    variables: &[Variable],
    status: &str,
) -> Result<bool, ParseError> {
    if forms.len() == 1 {
        return Ok(false);
    }
    let mut saw_model_error = false;
    for form in forms.iter().skip(1) {
        if form.is_model_unavailable_error() || form.is_unsat_core_unavailable_error() {
            saw_model_error = true;
            continue;
        }
        if form
            .as_list()
            .is_some_and(|values| values.iter().all(|item| item.as_atom().is_some()))
        {
            continue;
        }
        let _ = variables;
        return Err(ParseError::new(format!(
            "unexpected output after `{status}` status"
        )));
    }
    Ok(saw_model_error)
}

fn parse_unsat_output(
    forms: &[SExpression],
    variables: &[Variable],
    want_cores: bool,
) -> Result<(bool, Option<Vec<String>>), ParseError> {
    let expected_get_value_failure = require_non_sat_output(forms, variables, "unsat")?;
    let unsat_core = if want_cores {
        forms.iter().skip(1).find_map(|form| {
            let values = form.as_list()?;
            if values.is_empty() {
                return Some(Vec::new());
            }
            if values.iter().all(|item| item.as_atom().is_some())
                && !form.is_model_unavailable_error()
                && !form.is_unsat_core_unavailable_error()
            {
                Some(
                    values
                        .iter()
                        .filter_map(SExpression::as_atom)
                        .map(str::to_owned)
                        .collect(),
                )
            } else {
                None
            }
        })
    } else {
        None
    };
    Ok((expected_get_value_failure, unsat_core))
}

fn require_status_only(forms: &[SExpression], status: &str) -> Result<(), ParseError> {
    if forms.len() == 1 {
        Ok(())
    } else {
        // Allow ignored core-unavailable errors when cores were requested on a sat path.
        for form in forms.iter().skip(1) {
            if form.is_unsat_core_unavailable_error() {
                continue;
            }
            return Err(ParseError::new(format!(
                "unexpected output after `{status}` status"
            )));
        }
        Ok(())
    }
}

fn parse_sat_output(
    forms: &[SExpression],
    variables: &[Variable],
    want_cores: bool,
) -> Result<ParsedSolve, ParseError> {
    if variables.is_empty() {
        require_status_only(forms, "sat")?;
        return Ok(ParsedSolve::Sat(BTreeMap::new()));
    }

    // When cores are requested the script emits get-unsat-core before get-value.
    // On sat, that produces an error form we skip.
    let pairs_form = forms.iter().skip(1).find(|form| {
        if form.is_model_unavailable_error() || form.is_unsat_core_unavailable_error() {
            return false;
        }
        form.as_list().is_some_and(|values| {
            !values.is_empty()
                && values.iter().all(|item| {
                    item.as_list()
                        .is_some_and(|pair| pair.len() == 2 && pair[0].as_atom().is_some())
                })
        })
    });
    let pairs = pairs_form
        .and_then(SExpression::as_list)
        .ok_or_else(|| ParseError::new("sat output must contain one get-value response"))?;
    let _ = want_cores;
    if pairs.len() != variables.len() {
        return Err(ParseError::new(format!(
            "get-value returned {} bindings for {} variables",
            pairs.len(),
            variables.len()
        )));
    }

    let expected_symbols: HashSet<String> = variables
        .iter()
        .map(|variable| mangled_symbol(variable.name()))
        .collect();
    let mut bindings = HashMap::with_capacity(pairs.len());
    for pair in pairs {
        let pair = pair
            .as_list()
            .ok_or_else(|| ParseError::new("get-value binding must be a pair"))?;
        if pair.len() != 2 {
            return Err(ParseError::new(
                "get-value binding must contain symbol and value",
            ));
        }
        let symbol = pair[0]
            .as_atom()
            .ok_or_else(|| ParseError::new("get-value binding symbol must be an atom"))?;
        if !expected_symbols.contains(symbol) {
            return Err(ParseError::new(format!(
                "get-value returned unexpected symbol `{symbol}`"
            )));
        }
        if bindings.insert(symbol, &pair[1]).is_some() {
            return Err(ParseError::new(format!(
                "get-value returned duplicate symbol `{symbol}`"
            )));
        }
    }

    let mut model = BTreeMap::new();
    for variable in variables {
        let symbol = mangled_symbol(variable.name());
        let value = bindings
            .get(symbol.as_str())
            .ok_or_else(|| ParseError::new(format!("missing value for `{symbol}`")))?;
        model.insert(
            variable.name().to_owned(),
            parse_model_value(variable, value)?,
        );
    }
    Ok(ParsedSolve::Sat(model))
}

fn mangled_symbol(surface_name: &str) -> String {
    format!("{SMT_IDENTIFIER_PREFIX}{surface_name}")
}

fn parse_model_value(
    variable: &Variable,
    expression: &SExpression,
) -> Result<ModelValue, ParseError> {
    match variable {
        Variable::Bool { name } => match expression.as_atom() {
            Some("true") => Ok(ModelValue::Bool(true)),
            Some("false") => Ok(ModelValue::Bool(false)),
            _ => Err(ParseError::new(format!(
                "Boolean variable `{name}` had a non-Boolean value"
            ))),
        },
        Variable::Int { name } | Variable::IntRange { name, .. } => parse_integer(expression)
            .map(ModelValue::Int)
            .map_err(|error| error.with_context(format!("integer variable `{name}`"))),
        Variable::Real { name } => {
            let _ = name;
            Ok(ModelValue::Enum(render_s_expression(expression)))
        }
        Variable::BitVec { name, .. } => {
            let _ = name;
            Ok(ModelValue::Enum(render_s_expression(expression)))
        }
        Variable::Enum { name, values } => {
            let index = parse_integer(expression)
                .map_err(|error| error.with_context(format!("enum variable `{name}`")))?;
            let index = usize::try_from(index).map_err(|_negative_index| {
                ParseError::new(format!(
                    "enum variable `{name}` returned negative index {index}"
                ))
            })?;
            let mut labels: Vec<&str> = values.iter().map(String::as_str).collect();
            labels.sort_unstable();
            labels
                .get(index)
                .map(|label| ModelValue::Enum((*label).to_owned()))
                .ok_or_else(|| {
                    ParseError::new(format!(
                        "enum variable `{name}` returned out-of-range index {index}"
                    ))
                })
        }
    }
}

fn parse_integer(expression: &SExpression) -> Result<i64, ParseError> {
    if let Some(atom) = expression.as_atom() {
        return atom.parse::<i64>().map_err(|_invalid_integer| {
            ParseError::new(format!("`{atom}` is not a signed 64-bit integer"))
        });
    }

    let list = expression
        .as_list()
        .ok_or_else(|| ParseError::new("integer value must be an atom or unary minus"))?;
    let [operator, magnitude] = list else {
        return Err(ParseError::new(
            "integer list value must be unary minus with one operand",
        ));
    };
    if operator.as_atom() != Some("-") {
        return Err(ParseError::new("integer list value must use unary minus"));
    }
    let magnitude = magnitude
        .as_atom()
        .ok_or_else(|| ParseError::new("unary-minus magnitude must be an atom"))?
        .parse::<u64>()
        .map_err(|_invalid_magnitude| {
            ParseError::new("unary-minus magnitude must be an unsigned integer")
        })?;
    if magnitude == i64::MIN.unsigned_abs() {
        return Ok(i64::MIN);
    }
    let magnitude = i64::try_from(magnitude)
        .map_err(|_out_of_range| ParseError::new("negative integer exceeds signed 64-bit range"))?;
    Ok(-magnitude)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SExpression {
    Atom(String),
    String(String),
    List(Vec<Self>),
}

impl SExpression {
    fn as_atom(&self) -> Option<&str> {
        match self {
            Self::Atom(atom) => Some(atom),
            Self::String(_) | Self::List(_) => None,
        }
    }

    fn as_list(&self) -> Option<&[Self]> {
        match self {
            Self::Atom(_) | Self::String(_) => None,
            Self::List(values) => Some(values),
        }
    }

    fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Atom(_) | Self::List(_) => None,
        }
    }

    fn contains_atom(&self, needle: &str) -> bool {
        match self {
            Self::Atom(atom) => atom == needle,
            Self::String(_) => false,
            Self::List(values) => values.iter().any(|value| value.contains_atom(needle)),
        }
    }

    fn is_model_unavailable_error(&self) -> bool {
        self.is_error_containing("model is not available")
    }

    fn is_unsat_core_unavailable_error(&self) -> bool {
        self.is_error_containing("unsat core is not available")
            || self.is_error_containing("unsat cores are not available")
    }

    fn is_error_containing(&self, needle: &str) -> bool {
        let Some([head, message]) = self.as_list() else {
            return false;
        };
        head.as_atom() == Some("error")
            && message
                .as_string()
                .is_some_and(|message| message.to_ascii_lowercase().contains(needle))
    }
}

struct SExpressionParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

const MAX_SOLVER_OUTPUT_NESTING: usize = 256;

impl<'a> SExpressionParser<'a> {
    const fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            cursor: 0,
        }
    }

    fn parse_all(mut self) -> Result<Vec<SExpression>, ParseError> {
        let mut forms = Vec::new();
        self.skip_whitespace();
        while self.cursor < self.bytes.len() {
            forms.push(self.parse_expression(0)?);
            self.skip_whitespace();
        }
        if forms.is_empty() {
            return Err(ParseError::new("solver produced empty stdout"));
        }
        Ok(forms)
    }

    fn parse_expression(&mut self, nesting: usize) -> Result<SExpression, ParseError> {
        self.skip_whitespace();
        match self.bytes.get(self.cursor).copied() {
            Some(b'(') if nesting == MAX_SOLVER_OUTPUT_NESTING => Err(ParseError::new(format!(
                "solver output expression nesting exceeds maximum {MAX_SOLVER_OUTPUT_NESTING}"
            ))),
            Some(b'(') => self.parse_list(nesting + 1),
            Some(b')') => Err(ParseError::new("unexpected closing parenthesis")),
            Some(b'"') => self.parse_string(),
            Some(b'|') => self.parse_quoted_symbol(),
            Some(_) => self.parse_atom(),
            None => Err(ParseError::new("unexpected end of output")),
        }
    }

    fn parse_list(&mut self, nesting: usize) -> Result<SExpression, ParseError> {
        self.cursor += 1;
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            match self.bytes.get(self.cursor).copied() {
                Some(b')') => {
                    self.cursor += 1;
                    return Ok(SExpression::List(values));
                }
                Some(_) => values.push(self.parse_expression(nesting)?),
                None => return Err(ParseError::new("unterminated list")),
            }
        }
    }

    fn parse_atom(&mut self) -> Result<SExpression, ParseError> {
        let start = self.cursor;
        while let Some(byte) = self.bytes.get(self.cursor).copied() {
            if byte.is_ascii_whitespace() || matches!(byte, b'(' | b')') {
                break;
            }
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(ParseError::new("expected atom"));
        }
        let atom = str::from_utf8(&self.bytes[start..self.cursor])
            .map_err(|error| ParseError::new(format!("invalid UTF-8 atom: {error}")))?;
        Ok(SExpression::Atom(atom.to_owned()))
    }

    fn parse_quoted_symbol(&mut self) -> Result<SExpression, ParseError> {
        let start = self.cursor;
        self.cursor += 1;
        while let Some(byte) = self.bytes.get(self.cursor).copied() {
            self.cursor += 1;
            if byte == b'|' {
                let atom = str::from_utf8(&self.bytes[start..self.cursor]).map_err(|error| {
                    ParseError::new(format!("invalid UTF-8 quoted symbol: {error}"))
                })?;
                return Ok(SExpression::Atom(atom.to_owned()));
            }
        }
        Err(ParseError::new("unterminated quoted symbol"))
    }

    fn parse_string(&mut self) -> Result<SExpression, ParseError> {
        self.cursor += 1;
        let mut value = Vec::new();
        loop {
            match self.bytes.get(self.cursor).copied() {
                Some(b'"') if self.bytes.get(self.cursor + 1) == Some(&b'"') => {
                    value.push(b'"');
                    self.cursor += 2;
                }
                Some(b'"') => {
                    self.cursor += 1;
                    let value = String::from_utf8(value).map_err(|error| {
                        ParseError::new(format!("invalid UTF-8 string: {error}"))
                    })?;
                    return Ok(SExpression::String(value));
                }
                Some(byte) => {
                    value.push(byte);
                    self.cursor += 1;
                }
                None => return Err(ParseError::new("unterminated string")),
            }
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.cursor += 1;
        }
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
struct ParseError {
    message: String,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn with_context(self, context: impl AsRef<str>) -> Self {
        Self::new(format!("{}: {}", context.as_ref(), self.message))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use super::{
        classify_bound, parse_solver_output, response_from_output, response_from_raw_output,
        ParsedSolve, SExpressionParser, SolverService, SolverServiceError,
    };
    use crate::{
        persist::UNKNOWN_Z3_VERSION,
        process::{ProcessFuture, ProcessOutcome, ProcessOutput, ProcessRequest, ProcessRunner},
        types::{
            ModelValue, ObjectiveBound, OptimizationTermination, SolveConstraintsRequest,
            SolveStatus, Variable,
        },
    };
    use tokio::time::Instant;

    fn request(value: serde_json::Value) -> SolveConstraintsRequest {
        serde_json::from_value(value).expect("request fixture must deserialize")
    }

    fn completed(stdout: &str) -> ProcessOutput {
        ProcessOutput {
            success: true,
            exit_code: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn parses_minimum_integer_from_z3_unary_minus_form() {
        let parsed = parse_solver_output(
            "sat\n((v_floor (- 9223372036854775808)))\n",
            &[Variable::Int {
                name: "floor".to_owned(),
            }],
            false,
        )
        .expect("minimum integer should parse");

        let ParsedSolve::Sat(model) = parsed.solve else {
            panic!("expected sat model");
        };
        assert_eq!(model.get("floor"), Some(&ModelValue::Int(i64::MIN)));
    }

    #[test]
    fn rejects_extra_output_after_unsat() {
        let error = parse_solver_output("unsat\nsat\n", &[], false)
            .expect_err("extra status can spoof output and must fail");

        assert!(error.to_string().contains("unexpected output"));
    }

    #[test]
    fn rejects_duplicate_get_value_bindings() {
        let error = parse_solver_output(
            "sat\n((v_count 1) (v_count 2))\n",
            &[
                Variable::Int {
                    name: "count".to_owned(),
                },
                Variable::Int {
                    name: "other".to_owned(),
                },
            ],
            false,
        )
        .expect_err("duplicate binding must fail");

        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn parses_named_unsat_core_after_get_value_error() {
        let parsed = parse_solver_output(
            "unsat\n(lower upper)\n(error \"model is not available\")\n",
            &[Variable::Int {
                name: "x".to_owned(),
            }],
            true,
        )
        .expect("unsat core should parse");

        let ParsedSolve::Unsat { unsat_core } = parsed.solve else {
            panic!("expected unsat");
        };
        assert!(parsed.expected_get_value_failure);
        assert_eq!(
            unsat_core.as_deref(),
            Some(["lower".to_owned(), "upper".to_owned()].as_slice())
        );
    }

    #[test]
    fn rejects_solver_output_with_excessive_expression_nesting() {
        let nesting = 257;
        let output = format!("{}value{}", "(".repeat(nesting), ")".repeat(nesting));

        let error = SExpressionParser::new(&output)
            .parse_all()
            .expect_err("deep solver output must fail before exhausting the stack");

        assert!(error.to_string().contains("nesting"));
    }

    #[test]
    fn classifies_finite_infinite_negative_infinite_and_strict_bounds_losslessly() {
        let forms = SExpressionParser::new("3 oo (* (- 1) oo) (+ 1.0 (* (- 1.0) epsilon))")
            .parse_all()
            .expect("bound fixtures must parse");

        assert_eq!(
            classify_bound(&forms[0]),
            ObjectiveBound::Finite {
                exact: "3".to_owned()
            }
        );
        assert_eq!(
            classify_bound(&forms[1]),
            ObjectiveBound::Infinite {
                exact: "oo".to_owned()
            }
        );
        assert_eq!(
            classify_bound(&forms[2]),
            ObjectiveBound::Infinite {
                exact: "(* (- 1) oo)".to_owned()
            }
        );
        assert_eq!(
            classify_bound(&forms[3]),
            ObjectiveBound::Strict {
                exact: "(+ 1.0 (* (- 1.0) epsilon))".to_owned()
            }
        );
    }

    #[test]
    fn parses_typed_lex_cycle_and_derives_soft_group_diagnostics() {
        let request = request(serde_json::json!({
            "vars": [
                {"type":"int","name":"x"},
                {"type":"bool","name":"a"},
                {"type":"bool","name":"b"}
            ],
            "constraints": [
                {"kind":"bool","value":true},
                {"id":"prefer_a","group":"preferences","soft":true,"weight":2,
                 "expr":{"kind":"var","name":"a"}},
                {"id":"prefer_b","group":"preferences","soft":true,"weight":3,
                 "expr":{"kind":"var","name":"b"}}
            ],
            "objectives": [{"op":"maximize","expr":{"kind":"var","name":"x"}}],
            "use_cache": false
        }));
        let response = response_from_output(
            &request,
            completed(
                "sat\n(objectives (preferences 3) (v_x (+ 4 epsilon)))\n\
                 ((v_x 4) (v_a true) (v_b false) (v_a true) (v_b false) (v_x 4))\n",
            ),
            Instant::now(),
        );

        assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
        let optimization = response.optimization.expect("optimization payload");
        assert_eq!(optimization.termination, OptimizationTermination::Complete);
        let solution = &optimization.solutions[0];
        assert_eq!(solution.objectives[0].value, ModelValue::Int(4));
        assert!(matches!(
            &solution.objectives[0].bound,
            ObjectiveBound::Strict { exact } if exact == "(+ 4 epsilon)"
        ));
        assert_eq!(solution.soft_constraints[0].index, 1);
        assert!(solution.soft_constraints[0].satisfied);
        assert_eq!(solution.soft_constraints[1].index, 2);
        assert!(!solution.soft_constraints[1].satisfied);
        assert_eq!(solution.groups[0].group.as_deref(), Some("preferences"));
        assert_eq!(solution.groups[0].satisfied_weight, 2);
        assert_eq!(solution.groups[0].violated_weight, 3);
    }

    #[test]
    fn classifies_pareto_terminal_statuses_after_solutions() {
        let complete_request = request(serde_json::json!({
            "vars": [{"type":"int","name":"x"}],
            "constraints": [],
            "objectives": [{"op":"maximize","expr":{"kind":"var","name":"x"}}],
            "objective_priority": "pareto",
            "max_solutions": 2,
            "use_cache": false
        }));
        let one_solution = "sat\n(objectives (v_x 1))\n((v_x 1) (v_x 1))\n";
        let complete = response_from_output(
            &complete_request,
            completed(&format!("{one_solution}unsat\n")),
            Instant::now(),
        );
        assert_eq!(
            complete.optimization.unwrap().termination,
            OptimizationTermination::Complete
        );

        let limited = response_from_output(
            &complete_request,
            completed(&format!(
                "{one_solution}sat\n(objectives (v_x 2))\n((v_x 2) (v_x 2))\nsat\n"
            )),
            Instant::now(),
        );
        assert_eq!(
            limited.optimization.unwrap().termination,
            OptimizationTermination::SolutionLimit
        );

        let partial = response_from_output(
            &complete_request,
            completed(&format!("{one_solution}unknown\n")),
            Instant::now(),
        );
        assert_eq!(partial.status, SolveStatus::Sat);
        assert_eq!(
            partial.optimization.unwrap().termination,
            OptimizationTermination::Unknown
        );

        let initial_unknown =
            response_from_output(&complete_request, completed("unknown\n"), Instant::now());
        assert_eq!(initial_unknown.status, SolveStatus::Unknown);
        assert!(initial_unknown.optimization.is_none());
    }

    #[test]
    fn rejects_invalid_status_atoms_after_semantic_termination() {
        let one_solution = "sat\n(objectives (v_x 1))\n((v_x 1) (v_x 1))\n";
        for max_solutions in [2, 3] {
            let request = request(serde_json::json!({
                "vars": [{"type":"int","name":"x"}],
                "constraints": [],
                "objectives": [{"op":"maximize","expr":{"kind":"var","name":"x"}}],
                "objective_priority": "pareto",
                "max_solutions": max_solutions,
                "use_cache": false
            }));
            let response = response_from_output(
                &request,
                completed(&format!("{one_solution}unsat\nbogus\n")),
                Instant::now(),
            );

            assert_eq!(response.status, SolveStatus::Error, "{response:?}");
            assert!(response
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("bogus")));
        }
    }

    #[test]
    fn rejects_payloadless_sat_cycle_after_semantic_termination() {
        let request = request(serde_json::json!({
            "vars": [{"type":"int","name":"x"}],
            "constraints": [],
            "objectives": [{"op":"maximize","expr":{"kind":"var","name":"x"}}],
            "objective_priority": "pareto",
            "max_solutions": 3,
            "use_cache": false
        }));
        let response = response_from_output(
            &request,
            completed(
                "sat\n(objectives (v_x 1))\n((v_x 1) (v_x 1))\n\
                 unsat\nsat\nunsat\n",
            ),
            Instant::now(),
        );

        assert_eq!(response.status, SolveStatus::Error, "{response:?}");
    }

    #[test]
    fn parses_box_cycles_positionally_and_rejects_missing_payload() {
        let request = request(serde_json::json!({
            "vars": [{"type":"int","name":"x"},{"type":"int","name":"y"}],
            "constraints": [],
            "objectives": [
                {"op":"maximize","expr":{"kind":"var","name":"x"}},
                {"op":"maximize","expr":{"kind":"var","name":"y"}}
            ],
            "objective_priority": "box",
            "use_cache": false
        }));
        let response = response_from_output(
            &request,
            completed(
                "sat\n(objectives (v_x 3) (v_y 3))\n\
                 ((v_x 3) (v_y 0) (v_x 3) (v_y 0))\n\
                 sat\n(objectives (v_x 3) (v_y 3))\n\
                 ((v_x 0) (v_y 3) (v_x 0) (v_y 3))\nunsat\n",
            ),
            Instant::now(),
        );
        assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
        assert_eq!(response.optimization.unwrap().solutions.len(), 2);

        let malformed = response_from_output(
            &request,
            completed("sat\n(objectives (v_x 3) (v_y 3))\n"),
            Instant::now(),
        );
        assert_eq!(malformed.status, SolveStatus::Error);
        assert!(malformed.reason.unwrap().contains("combined get-value"));
    }

    #[test]
    fn raw_objective_output_is_exposed_with_unknown_metadata() {
        let response = response_from_raw_output(
            completed("sat\n(objectives (v_x (* (- 1) oo)))\n"),
            Instant::now(),
        );

        assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
        let optimization = response.optimization.expect("raw objective payload");
        assert_eq!(optimization.priority, None);
        assert_eq!(optimization.solutions[0].objectives[0].op, None);
        assert_eq!(
            optimization.solutions[0].objectives[0].value,
            ModelValue::Enum("(* (- 1) oo)".to_owned())
        );
        assert!(matches!(
            optimization.solutions[0].objectives[0].bound,
            ObjectiveBound::Infinite { .. }
        ));
    }

    #[test]
    fn raw_sat_objectives_and_model_are_order_independent() {
        for stdout in [
            "sat\n((x 0))\n(objectives (x (* (- 1) oo)))\n",
            "sat\n(objectives (x (* (- 1) oo)))\n((x 0))\n",
        ] {
            let response = response_from_raw_output(completed(stdout), Instant::now());

            assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
            assert_eq!(
                response.model.as_ref().and_then(|model| model.get("x")),
                Some(&ModelValue::Int(0))
            );
            assert!(response.optimization.is_some());
        }

        for stdout in [
            "sat\n((x 0))\n((x 1))\n",
            "sat\n(objectives (x 0))\n(objectives (x 1))\n",
        ] {
            let response = response_from_raw_output(completed(stdout), Instant::now());

            assert_eq!(response.status, SolveStatus::Error, "{response:?}");
        }
    }

    #[test]
    fn raw_non_sat_objectives_are_validated_and_ignored() {
        for (stdout, expected_status) in [
            ("unsat\n(objectives (x 0))\n", SolveStatus::Unsat),
            ("unknown\n(objectives (x 0))\n", SolveStatus::Unknown),
        ] {
            let response = response_from_raw_output(completed(stdout), Instant::now());

            assert_eq!(response.status, expected_status, "{response:?}");
            assert!(response.optimization.is_none());
        }

        let malformed =
            response_from_raw_output(completed("unsat\n(objectives malformed)\n"), Instant::now());
        assert_eq!(malformed.status, SolveStatus::Error, "{malformed:?}");
    }

    #[test]
    fn raw_empty_objectives_after_unsat_are_not_an_unsat_core() {
        let response =
            response_from_raw_output(completed("unsat\n(objectives\n)\n"), Instant::now());

        assert_eq!(response.status, SolveStatus::Unsat, "{response:?}");
        assert!(response.optimization.is_none());
        assert!(response.unsat_core.is_none());
    }

    #[derive(Debug)]
    struct VersionRunner {
        version: &'static str,
        runs: AtomicUsize,
    }

    impl VersionRunner {
        const fn new(version: &'static str) -> Self {
            Self {
                version,
                runs: AtomicUsize::new(0),
            }
        }
    }

    impl ProcessRunner for VersionRunner {
        fn run(&self, _request: ProcessRequest) -> ProcessFuture<'_> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Box::pin(future::ready(Ok(ProcessOutcome::Completed(completed(
                "sat\n(objectives (v_x 1))\n((v_x 1) (v_x 1))\n",
            )))))
        }

        fn solver_version(&self) -> String {
            self.version.to_owned()
        }
    }

    #[derive(Debug, Default)]
    struct SequentialOptimizationRunner {
        runs: AtomicUsize,
    }

    impl ProcessRunner for SequentialOptimizationRunner {
        fn run(&self, _request: ProcessRequest) -> ProcessFuture<'_> {
            let run = self.runs.fetch_add(1, Ordering::SeqCst);
            let terminal = if run == 0 { "unknown" } else { "unsat" };
            Box::pin(future::ready(Ok(ProcessOutcome::Completed(completed(
                &format!("sat\n(objectives (v_x 1))\n((v_x 1) (v_x 1))\n{terminal}\n"),
            )))))
        }

        fn solver_version(&self) -> String {
            UNKNOWN_Z3_VERSION.to_owned()
        }
    }

    #[tokio::test]
    async fn partial_unknown_optimization_results_are_not_cached() {
        let runner = Arc::new(SequentialOptimizationRunner::default());
        let service_runner: Arc<dyn ProcessRunner> = Arc::clone(&runner) as Arc<dyn ProcessRunner>;
        let service = SolverService::with_runner(service_runner);
        let request = request(serde_json::json!({
            "vars": [{"type":"int","name":"x"}],
            "constraints": [],
            "objectives": [{"op":"maximize","expr":{"kind":"var","name":"x"}}],
            "objective_priority": "pareto",
            "max_solutions": 1,
            "use_cache": true
        }));

        let first = service
            .solve_constraints(request.clone())
            .await
            .expect("partial solve must succeed");
        assert_eq!(
            first.optimization.as_ref().unwrap().termination,
            OptimizationTermination::Unknown
        );

        let second = service
            .solve_constraints(request)
            .await
            .expect("later solve must execute again");
        assert_eq!(
            second.optimization.as_ref().unwrap().termination,
            OptimizationTermination::Complete
        );
        assert_eq!(runner.runs.load(Ordering::SeqCst), 2);
        assert!(!second.cached);
    }

    fn version_request() -> SolveConstraintsRequest {
        request(serde_json::json!({
            "vars": [{"type":"int","name":"x"}],
            "constraints": [],
            "objectives": [{"op":"maximize","expr":{"kind":"var","name":"x"}}],
            "use_cache": false
        }))
    }

    #[tokio::test]
    async fn optimization_rejects_old_z3_before_execution_and_exposes_accepted_version() {
        let old_runner = Arc::new(VersionRunner::new("Z3 version 4.8.11 - 64 bit"));
        let service_runner: Arc<dyn ProcessRunner> = Arc::<VersionRunner>::clone(&old_runner);
        let error = SolverService::with_runner(service_runner)
            .solve_constraints(version_request())
            .await
            .expect_err("old production Z3 must be rejected");
        assert!(matches!(
            error,
            SolverServiceError::SolverUnavailable { .. }
        ));
        assert_eq!(old_runner.runs.load(Ordering::SeqCst), 0);

        let current_runner = Arc::new(VersionRunner::new("Z3 version 4.16.0 - 64 bit"));
        let response = SolverService::with_runner(current_runner)
            .solve_constraints(version_request())
            .await
            .expect("supported Z3 must run");
        assert_eq!(
            response.solver_version.as_deref(),
            Some("Z3 version 4.16.0 - 64 bit")
        );

        let unknown_runner = Arc::new(VersionRunner::new(UNKNOWN_Z3_VERSION));
        let response = SolverService::with_runner(unknown_runner)
            .solve_constraints(version_request())
            .await
            .expect("unknown injected runner versions stay allowed");
        assert_eq!(response.status, SolveStatus::Sat);
    }
}
