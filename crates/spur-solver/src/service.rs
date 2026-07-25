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
    encode::{encode_solve_constraints, EncodeError, SMT_IDENTIFIER_PREFIX},
    persist::{
        validate_solve_id, ArtifactStore, GetSolveResultResponse, PersistError, SolveArtifact,
        UNKNOWN_Z3_VERSION,
    },
    process::{ProcessError, ProcessOutcome, ProcessRequest, ProcessRunner, Z3Process},
    types::{
        ModelValue, SolveConstraintsRequest, SolveConstraintsResponse, SolveModel, SolveStatus,
        Variable,
    },
};

/// Default maximum number of concurrent Z3 children in one shared service.
pub const DEFAULT_MAX_CONCURRENT_SOLVES: usize = 4;

/// Transport-facing failure that prevents a solve result envelope.
#[derive(Debug, Error)]
pub enum SolverServiceError {
    /// Typed validation or generated-script limits rejected the request.
    #[error("invalid solver request: {source}")]
    InvalidParams {
        /// Encoder or request validation failure.
        #[source]
        source: EncodeError,
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
        self.artifacts
            .as_ref()
            .ok_or(SolverServiceError::RepoRootNotConfigured)?
            .persist(request, response, UNKNOWN_Z3_VERSION)
            .map_err(SolverServiceError::from)
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
        request: SolveConstraintsRequest,
    ) -> Result<SolveConstraintsResponse, SolverServiceError> {
        let started = Instant::now();
        let smt = encode_solve_constraints(&request)
            .map_err(|source| SolverServiceError::InvalidParams { source })?;
        let deadline = started + Duration::from_millis(request.timeout_ms);

        if Instant::now() >= deadline {
            return self.finish_response(&request, timeout_response(started));
        }

        let permit = match time::timeout_at(deadline, self.semaphore.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_closed)) => {
                return self.finish_response(
                    &request,
                    error_response(started, "solver concurrency semaphore is closed".to_owned()),
                );
            }
            Err(_elapsed) => {
                return self.finish_response(&request, timeout_response(started));
            }
        };

        let outcome = self.runner.run(ProcessRequest::new(smt, deadline)).await;
        drop(permit);

        let response = match outcome {
            Ok(ProcessOutcome::TimedOut) => timeout_response(started),
            Ok(ProcessOutcome::Completed(output)) => {
                response_from_output(&request, output, started)
            }
            Err(ProcessError::SolverUnavailable { message }) => {
                return Err(SolverServiceError::SolverUnavailable { message });
            }
            Err(error) => error_response(started, error.to_string()),
        };
        self.finish_response(&request, response)
    }

    fn finish_response(
        &self,
        request: &SolveConstraintsRequest,
        mut response: SolveConstraintsResponse,
    ) -> Result<SolveConstraintsResponse, SolverServiceError> {
        if request.persist {
            let artifact = self.persist(request, &response)?;
            response.solve_id = Some(artifact.solve_id);
        }
        Ok(response)
    }
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
    if !stderr.trim().is_empty() {
        return error_response(
            started,
            format!("Z3 wrote to stderr: {}", diagnostic_text(&stderr)),
        );
    }

    let stdout = match str::from_utf8(&output.stdout) {
        Ok(stdout) => stdout,
        Err(error) => {
            return error_response(started, format!("Z3 stdout was not UTF-8: {error}"));
        }
    };
    let parsed = parse_solver_output(stdout, &request.vars);
    let expected_get_value_failure = output.exit_code == Some(1)
        && parsed
            .as_ref()
            .is_ok_and(|parsed| parsed.expected_get_value_failure);
    if !output.success && !expected_get_value_failure {
        let exit = output
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| format!("exit code {code}"));
        return error_response(started, format!("Z3 exited unsuccessfully ({exit})"));
    }

    match parsed.map(|parsed| parsed.solve) {
        Ok(ParsedSolve::Sat(model)) => response(SolveStatus::Sat, Some(model), started, None),
        Ok(ParsedSolve::Unsat) => response(SolveStatus::Unsat, None, started, None),
        Ok(ParsedSolve::Unknown) => response(
            SolveStatus::Unknown,
            None,
            started,
            Some("Z3 returned unknown".to_owned()),
        ),
        Err(error) => error_response(started, format!("failed to parse Z3 output: {error}")),
    }
}

fn response(
    status: SolveStatus,
    model: Option<SolveModel>,
    started: Instant,
    reason: Option<String>,
) -> SolveConstraintsResponse {
    SolveConstraintsResponse {
        status,
        model,
        duration_ms: elapsed_millis(started),
        solve_id: None,
        reason,
        smt: None,
    }
}

fn timeout_response(started: Instant) -> SolveConstraintsResponse {
    response(
        SolveStatus::Timeout,
        None,
        started,
        Some("wall-clock solve budget exhausted".to_owned()),
    )
}

fn error_response(started: Instant, reason: String) -> SolveConstraintsResponse {
    response(SolveStatus::Error, None, started, Some(reason))
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
    Unsat,
    Unknown,
}

#[derive(Debug, Eq, PartialEq)]
struct ParsedSolverOutput {
    solve: ParsedSolve,
    expected_get_value_failure: bool,
}

fn parse_solver_output(
    stdout: &str,
    variables: &[Variable],
) -> Result<ParsedSolverOutput, ParseError> {
    let forms = SExpressionParser::new(stdout).parse_all()?;
    let status = forms
        .first()
        .and_then(SExpression::as_atom)
        .ok_or_else(|| ParseError::new("expected status atom as first output form"))?;

    match status {
        "sat" => Ok(ParsedSolverOutput {
            solve: parse_sat_output(&forms, variables)?,
            expected_get_value_failure: false,
        }),
        "unsat" => {
            let expected_get_value_failure = require_non_sat_output(&forms, variables, "unsat")?;
            Ok(ParsedSolverOutput {
                solve: ParsedSolve::Unsat,
                expected_get_value_failure,
            })
        }
        "unknown" => {
            let expected_get_value_failure = require_non_sat_output(&forms, variables, "unknown")?;
            Ok(ParsedSolverOutput {
                solve: ParsedSolve::Unknown,
                expected_get_value_failure,
            })
        }
        other => Err(ParseError::new(format!(
            "unexpected solver status `{other}`"
        ))),
    }
}

fn require_non_sat_output(
    forms: &[SExpression],
    variables: &[Variable],
    status: &str,
) -> Result<bool, ParseError> {
    if forms.len() == 1 {
        return Ok(false);
    }
    if !variables.is_empty() && forms.len() == 2 && forms[1].is_model_unavailable_error() {
        return Ok(true);
    }
    Err(ParseError::new(format!(
        "unexpected output after `{status}` status"
    )))
}

fn require_status_only(forms: &[SExpression], status: &str) -> Result<(), ParseError> {
    if forms.len() == 1 {
        Ok(())
    } else {
        Err(ParseError::new(format!(
            "unexpected output after `{status}` status"
        )))
    }
}

fn parse_sat_output(
    forms: &[SExpression],
    variables: &[Variable],
) -> Result<ParsedSolve, ParseError> {
    if variables.is_empty() {
        require_status_only(forms, "sat")?;
        return Ok(ParsedSolve::Sat(BTreeMap::new()));
    }
    if forms.len() != 2 {
        return Err(ParseError::new(
            "sat output must contain exactly one get-value response",
        ));
    }

    let pairs = forms[1]
        .as_list()
        .ok_or_else(|| ParseError::new("get-value response must be a list"))?;
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

    fn is_model_unavailable_error(&self) -> bool {
        let Some([head, message]) = self.as_list() else {
            return false;
        };
        head.as_atom() == Some("error")
            && message
                .as_string()
                .is_some_and(|message| message.contains("model is not available"))
    }
}

struct SExpressionParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

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
            forms.push(self.parse_expression()?);
            self.skip_whitespace();
        }
        if forms.is_empty() {
            return Err(ParseError::new("solver produced empty stdout"));
        }
        Ok(forms)
    }

    fn parse_expression(&mut self) -> Result<SExpression, ParseError> {
        self.skip_whitespace();
        match self.bytes.get(self.cursor).copied() {
            Some(b'(') => self.parse_list(),
            Some(b')') => Err(ParseError::new("unexpected closing parenthesis")),
            Some(b'"') => self.parse_string(),
            Some(_) => self.parse_atom(),
            None => Err(ParseError::new("unexpected end of output")),
        }
    }

    fn parse_list(&mut self) -> Result<SExpression, ParseError> {
        self.cursor += 1;
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            match self.bytes.get(self.cursor).copied() {
                Some(b')') => {
                    self.cursor += 1;
                    return Ok(SExpression::List(values));
                }
                Some(_) => values.push(self.parse_expression()?),
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
    use super::{parse_solver_output, ParsedSolve};
    use crate::types::{ModelValue, Variable};

    #[test]
    fn parses_minimum_integer_from_z3_unary_minus_form() {
        let parsed = parse_solver_output(
            "sat\n((v_floor (- 9223372036854775808)))\n",
            &[Variable::Int {
                name: "floor".to_owned(),
            }],
        )
        .expect("minimum integer should parse");

        let ParsedSolve::Sat(model) = parsed.solve else {
            panic!("expected sat model");
        };
        assert_eq!(model.get("floor"), Some(&ModelValue::Int(i64::MIN)));
    }

    #[test]
    fn rejects_extra_output_after_unsat() {
        let error = parse_solver_output("unsat\nsat\n", &[])
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
        )
        .expect_err("duplicate binding must fail");

        assert!(error.to_string().contains("duplicate"));
    }
}
