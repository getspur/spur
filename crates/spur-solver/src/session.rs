//! Accumulation-based incremental solve sessions.
//!
//! Each solve re-encodes the full stacked constraint set into a fresh Z3
//! subprocess (no long-lived solver process). This preserves crash isolation
//! while letting agents push/pop constraint frames across MCP calls.

use std::{collections::HashMap, sync::Mutex, time::Instant};

use uuid::Uuid;

use crate::types::{
    ConstraintItem, SessionOp, SolveConstraintsRequest, Variable, MAX_SESSION_FRAMES,
    MAX_SOLVE_SESSIONS,
};

/// Prefix for traversal-safe session identifiers.
pub const SESSION_ID_PREFIX: &str = "sess_";

/// In-memory session table shared by cloned solver services.
#[derive(Debug, Default)]
pub struct SessionStore {
    inner: Mutex<HashMap<String, Session>>,
}

#[derive(Clone, Debug)]
struct Session {
    vars: Vec<Variable>,
    frames: Vec<Vec<ConstraintItem>>,
    _created: Instant,
}

/// Outcome of applying a session op before the solve.
#[derive(Debug)]
pub enum SessionApply {
    /// Stateless solve using the request as-is.
    Stateless,
    /// Solve with merged constraints; optional session id for the response.
    Solve {
        session_id: Option<String>,
        constraints: Vec<ConstraintItem>,
        vars: Vec<Variable>,
    },
    /// Session ended; no solver run required when nothing remains to check.
    Ended { session_id: String },
}

/// Session apply failure.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Unknown session id.
    #[error("session_id `{session_id}` was not found")]
    NotFound { session_id: String },
    /// Session table is full.
    #[error("solve session quota exceeded (max {limit})")]
    QuotaExceeded { limit: usize },
    /// Push depth exceeded.
    #[error("session frame depth exceeds maximum {limit}")]
    FrameOverflow { limit: usize },
    /// Pop with empty stack.
    #[error("session has no frames to pop")]
    EmptyPop,
    /// Begin/push supplied variables that conflict with the session.
    #[error("session variables must match the original begin request")]
    VariableMismatch,
}

impl SessionStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `session_op` and returns constraints/vars for the upcoming solve.
    pub fn apply(&self, request: &SolveConstraintsRequest) -> Result<SessionApply, SessionError> {
        match request.session_op {
            SessionOp::None => {
                if let Some(session_id) = request.session_id.as_deref() {
                    let guard = self.inner.lock().expect("session lock");
                    let session = guard
                        .get(session_id)
                        .ok_or_else(|| SessionError::NotFound {
                            session_id: session_id.to_owned(),
                        })?;
                    let mut constraints = flatten_frames(&session.frames);
                    constraints.extend(request.constraints.iter().cloned());
                    Ok(SessionApply::Solve {
                        session_id: Some(session_id.to_owned()),
                        constraints,
                        vars: session.vars.clone(),
                    })
                } else {
                    Ok(SessionApply::Stateless)
                }
            }
            SessionOp::Begin => {
                let mut guard = self.inner.lock().expect("session lock");
                if guard.len() >= MAX_SOLVE_SESSIONS {
                    return Err(SessionError::QuotaExceeded {
                        limit: MAX_SOLVE_SESSIONS,
                    });
                }
                let session_id = allocate_session_id(&guard)?;
                guard.insert(
                    session_id.clone(),
                    Session {
                        vars: request.vars.clone(),
                        frames: vec![request.constraints.clone()],
                        _created: Instant::now(),
                    },
                );
                Ok(SessionApply::Solve {
                    session_id: Some(session_id),
                    constraints: request.constraints.clone(),
                    vars: request.vars.clone(),
                })
            }
            SessionOp::Push => {
                let session_id = request
                    .session_id
                    .clone()
                    .expect("validated session_id for push");
                let mut guard = self.inner.lock().expect("session lock");
                let session = guard
                    .get_mut(&session_id)
                    .ok_or_else(|| SessionError::NotFound {
                        session_id: session_id.clone(),
                    })?;
                if !request.vars.is_empty() && request.vars != session.vars {
                    return Err(SessionError::VariableMismatch);
                }
                if session.frames.len() >= MAX_SESSION_FRAMES {
                    return Err(SessionError::FrameOverflow {
                        limit: MAX_SESSION_FRAMES,
                    });
                }
                session.frames.push(request.constraints.clone());
                let constraints = flatten_frames(&session.frames);
                let vars = session.vars.clone();
                Ok(SessionApply::Solve {
                    session_id: Some(session_id),
                    constraints,
                    vars,
                })
            }
            SessionOp::Pop => {
                let session_id = request
                    .session_id
                    .clone()
                    .expect("validated session_id for pop");
                let mut guard = self.inner.lock().expect("session lock");
                let session = guard
                    .get_mut(&session_id)
                    .ok_or_else(|| SessionError::NotFound {
                        session_id: session_id.clone(),
                    })?;
                if session.frames.is_empty() {
                    return Err(SessionError::EmptyPop);
                }
                session.frames.pop();
                let constraints = flatten_frames(&session.frames);
                let vars = session.vars.clone();
                Ok(SessionApply::Solve {
                    session_id: Some(session_id),
                    constraints,
                    vars,
                })
            }
            SessionOp::End => {
                let session_id = request
                    .session_id
                    .clone()
                    .expect("validated session_id for end");
                let mut guard = self.inner.lock().expect("session lock");
                if guard.remove(&session_id).is_none() {
                    return Err(SessionError::NotFound {
                        session_id: session_id.clone(),
                    });
                }
                Ok(SessionApply::Ended { session_id })
            }
        }
    }
}

fn flatten_frames(frames: &[Vec<ConstraintItem>]) -> Vec<ConstraintItem> {
    frames.iter().flatten().cloned().collect()
}

fn allocate_session_id(existing: &HashMap<String, Session>) -> Result<String, SessionError> {
    for _ in 0..16 {
        let id = format!(
            "{SESSION_ID_PREFIX}{}",
            &Uuid::new_v4().simple().to_string()[..16]
        );
        if !existing.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(SessionError::QuotaExceeded {
        limit: MAX_SOLVE_SESSIONS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ConstraintExpr, ConstraintOp, ObjectivePriority, Variable, DEFAULT_MAX_SOLUTIONS,
        DEFAULT_TIMEOUT_MS,
    };

    fn request(op: SessionOp, session_id: Option<String>) -> SolveConstraintsRequest {
        SolveConstraintsRequest {
            vars: vec![Variable::IntRange {
                name: "x".to_owned(),
                min: 0,
                max: 10,
            }],
            constraints: vec![ConstraintExpr::Op {
                op: ConstraintOp::Ge,
                args: vec![
                    ConstraintExpr::Var {
                        name: "x".to_owned(),
                    },
                    ConstraintExpr::Int { value: 1 },
                ],
            }
            .into()],
            objectives: vec![],
            objective_priority: ObjectivePriority::Lex,
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
            use_cache: false,
            session_id,
            session_op: op,
        }
    }

    #[test]
    fn begin_push_pop_end_round_trip() {
        let store = SessionStore::new();
        let SessionApply::Solve {
            session_id: Some(id),
            constraints,
            ..
        } = store.apply(&request(SessionOp::Begin, None)).unwrap()
        else {
            panic!("begin should solve");
        };
        assert_eq!(constraints.len(), 1);

        let mut push = request(SessionOp::Push, Some(id.clone()));
        push.constraints = vec![ConstraintExpr::Op {
            op: ConstraintOp::Le,
            args: vec![
                ConstraintExpr::Var {
                    name: "x".to_owned(),
                },
                ConstraintExpr::Int { value: 5 },
            ],
        }
        .into()];
        let SessionApply::Solve { constraints, .. } = store.apply(&push).unwrap() else {
            panic!("push should solve");
        };
        assert_eq!(constraints.len(), 2);

        let SessionApply::Solve { constraints, .. } = store
            .apply(&request(SessionOp::Pop, Some(id.clone())))
            .unwrap()
        else {
            panic!("pop should solve");
        };
        assert_eq!(constraints.len(), 1);

        let SessionApply::Ended { session_id } =
            store.apply(&request(SessionOp::End, Some(id))).unwrap()
        else {
            panic!("end should drop");
        };
        assert!(session_id.starts_with(SESSION_ID_PREFIX));
    }

    use crate::{
        process::{ProcessFuture, ProcessOutcome, ProcessOutput, ProcessRequest, ProcessRunner},
        service::SolverService,
        types::SolveStatus,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Debug)]
    struct CountingRunner {
        calls: AtomicUsize,
        stdout: Vec<u8>,
    }

    impl ProcessRunner for CountingRunner {
        fn run(&self, _request: ProcessRequest) -> ProcessFuture<'_> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let stdout = self.stdout.clone();
            Box::pin(async move {
                Ok(ProcessOutcome::Completed(ProcessOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout,
                    stderr: Vec::new(),
                }))
            })
        }
    }

    #[tokio::test]
    async fn session_end_returns_ended_without_model() {
        let service = SolverService::with_runner(Arc::new(CountingRunner {
            calls: AtomicUsize::new(0),
            stdout: b"sat\n((v_x 1))\n".to_vec(),
        }));
        let begin = service
            .solve_constraints(request(SessionOp::Begin, None))
            .await
            .expect("begin");
        let session_id = begin.session_id.expect("begin returns session_id");

        let ended = service
            .solve_constraints(request(SessionOp::End, Some(session_id.clone())))
            .await
            .expect("end");
        assert_eq!(ended.status, SolveStatus::Ended);
        assert!(ended.model.is_none());
        assert_eq!(ended.reason.as_deref(), Some("session ended"));
        assert_eq!(ended.session_id.as_deref(), Some(session_id.as_str()));
        ended.validate().expect("ended envelope is valid");
    }

    #[tokio::test]
    async fn unknown_outcomes_are_not_cached() {
        let runner = Arc::new(CountingRunner {
            calls: AtomicUsize::new(0),
            stdout: b"unknown\n".to_vec(),
        });
        let service = SolverService::with_runner(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
        let mut req = request(SessionOp::None, None);
        req.use_cache = true;
        req.constraints = vec![ConstraintExpr::Bool { value: true }.into()];

        let first = service.solve_constraints(req.clone()).await.expect("first");
        assert_eq!(first.status, SolveStatus::Unknown);
        assert!(!first.cached);

        let second = service.solve_constraints(req).await.expect("second");
        assert_eq!(second.status, SolveStatus::Unknown);
        assert!(!second.cached);
        // Both calls must hit the runner — unknown must not sticky-cache.
        assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn sat_outcomes_are_cached_on_second_call() {
        let runner = Arc::new(CountingRunner {
            calls: AtomicUsize::new(0),
            stdout: b"sat\n((v_x 1))\n".to_vec(),
        });
        let service = SolverService::with_runner(Arc::clone(&runner) as Arc<dyn ProcessRunner>);
        let mut req = request(SessionOp::None, None);
        req.use_cache = true;

        let first = service.solve_constraints(req.clone()).await.expect("first");
        assert_eq!(first.status, SolveStatus::Sat);
        assert!(!first.cached);

        let second = service.solve_constraints(req).await.expect("second");
        assert_eq!(second.status, SolveStatus::Sat);
        assert!(second.cached);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    }
}
