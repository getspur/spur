//! Reject-only allowlist gate for raw SMT-LIB2 scripts.

use thiserror::Error;

/// Maximum raw SMT-LIB2 script size accepted by [`validate_smt_script`].
pub const MAX_RAW_SMT_BYTES: usize = 256 * 1024;

/// Failure reported while gating a raw SMT-LIB2 script.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum SmtGateError {
    /// The script exceeded [`MAX_RAW_SMT_BYTES`].
    #[error("raw SMT-LIB2 script size {actual_bytes} exceeds maximum {max_bytes} bytes")]
    ScriptTooLarge {
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
        /// Maximum accepted UTF-8 byte length.
        max_bytes: usize,
    },
    /// The input was not a complete sequence of top-level S-expressions.
    #[error("malformed raw SMT-LIB2 at byte {offset}: {message}")]
    Malformed {
        /// Byte offset at or immediately after the malformed token.
        offset: usize,
        /// Stable parse diagnostic.
        message: &'static str,
    },
    /// A top-level command was outside the fixed allowlist.
    #[error("raw SMT-LIB2 command `{command}` at byte {offset} is not allowed")]
    DisallowedCommand {
        /// Rejected top-level command name.
        command: String,
        /// Byte offset of the command name.
        offset: usize,
    },
    /// `set-option` named a key outside the fixed safe subset.
    #[error("raw SMT-LIB2 option `{option}` at byte {offset} is not allowed")]
    DisallowedOption {
        /// Rejected option key.
        option: String,
        /// Byte offset of the option key.
        offset: usize,
    },
    /// `set-option` did not have the required key/value shape.
    #[error("invalid raw SMT-LIB2 set-option at byte {offset}: {message}")]
    InvalidSetOption {
        /// Byte offset at or immediately after the invalid token.
        offset: usize,
        /// Stable validation diagnostic.
        message: &'static str,
    },
}

/// Validates a complete raw SMT-LIB2 script against the v1 command allowlist.
///
/// The scanner understands top-level lists, comments, strings, and quoted
/// symbols. It checks only command capability, not SMT sorts or assertions:
/// nested expressions remain entirely solver-owned. Accepted scripts are not
/// rewritten, stripped, or normalized.
///
/// `set-option` is restricted to a fixed Boolean subset:
/// `:produce-models` and `:produce-unsat-cores`.
/// Commands beginning with `declare-` are accepted alongside `set-logic`,
/// `assert`, `assert-soft`, `check-sat`, `get-model`, `get-value`,
/// `get-objectives`, `get-unsat-core`, `maximize`, `minimize`, `push`, and
/// `pop`.
///
/// # Errors
///
/// Returns [`SmtGateError`] for oversized, malformed, or disallowed scripts.
///
/// # Examples
///
/// ```
/// use spur_solver::smt_gate::validate_smt_script;
///
/// let script = "
///     (set-logic QF_LIA)
///     (declare-const answer Int)
///     (assert (= answer 42))
///     (check-sat)
///     (get-value (answer))
/// ";
///
/// assert!(validate_smt_script(script).is_ok());
/// assert!(validate_smt_script("(exit)").is_err());
/// assert!(validate_smt_script("(maximize answer)").is_ok());
/// assert!(validate_smt_script("(assert-soft true :weight 1)").is_ok());
/// ```
pub fn validate_smt_script(script: &str) -> Result<(), SmtGateError> {
    validate_smt_script_with_responses(script).map(|_responses| ())
}

/// One response-producing command in an already validated raw SMT script.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RawResponseCommand {
    /// `check-sat` produces a status atom.
    CheckSat,
    /// `get-model` produces a model or an error form.
    GetModel,
    /// `get-value` produces bindings or an error form.
    GetValue,
    /// `get-objectives` produces an objectives form.
    GetObjectives,
    /// `get-unsat-core` produces assertion names or an error form.
    GetUnsatCore,
}

/// Validates a raw script and records its response-producing commands in order.
pub(crate) fn validate_smt_script_with_responses(
    script: &str,
) -> Result<Vec<RawResponseCommand>, SmtGateError> {
    let actual_bytes = script.len();
    if actual_bytes > MAX_RAW_SMT_BYTES {
        return Err(SmtGateError::ScriptTooLarge {
            actual_bytes,
            max_bytes: MAX_RAW_SMT_BYTES,
        });
    }

    let mut lexer = Lexer::new(script);
    let mut form_count = 0_usize;
    let mut responses = Vec::new();
    while let Some(token) = lexer.next_token()? {
        let open_offset = match token {
            Token::Open { offset } => offset,
            other => {
                return Err(SmtGateError::Malformed {
                    offset: other.offset(),
                    message: "top-level input must be an S-expression list",
                });
            }
        };

        let (command, command_offset) = match lexer.next_token()? {
            Some(Token::Atom { value, offset }) => (value, offset),
            Some(other) => {
                return Err(SmtGateError::Malformed {
                    offset: other.offset(),
                    message: "top-level command must be an unquoted symbol",
                });
            }
            None => {
                return Err(SmtGateError::Malformed {
                    offset: open_offset,
                    message: "unterminated top-level form",
                });
            }
        };

        if command == "set-option" {
            validate_set_option(&mut lexer)?;
        } else if is_allowed_command(command) {
            skip_form_tail(&mut lexer, open_offset)?;
        } else {
            return Err(SmtGateError::DisallowedCommand {
                command: command.to_owned(),
                offset: command_offset,
            });
        }
        if let Some(response) = match command {
            "check-sat" => Some(RawResponseCommand::CheckSat),
            "get-model" => Some(RawResponseCommand::GetModel),
            "get-value" => Some(RawResponseCommand::GetValue),
            "get-objectives" => Some(RawResponseCommand::GetObjectives),
            "get-unsat-core" => Some(RawResponseCommand::GetUnsatCore),
            _ => None,
        } {
            responses.push(response);
        }
        form_count += 1;
    }

    if form_count == 0 {
        return Err(SmtGateError::Malformed {
            offset: 0,
            message: "script contains no top-level commands",
        });
    }

    Ok(responses)
}

fn is_allowed_command(command: &str) -> bool {
    matches!(
        command,
        "set-logic"
            | "assert"
            | "assert-soft"
            | "check-sat"
            | "get-model"
            | "get-value"
            | "get-objectives"
            | "get-unsat-core"
            | "maximize"
            | "minimize"
            | "push"
            | "pop"
    ) || command
        .strip_prefix("declare-")
        .is_some_and(|suffix| !suffix.is_empty())
}

fn is_allowed_set_option(option: &str) -> bool {
    matches!(
        option,
        ":produce-models" | ":produce-unsat-cores" | ":opt.priority"
    )
}

fn validate_set_option(lexer: &mut Lexer<'_>) -> Result<(), SmtGateError> {
    let (option, option_offset) = match lexer.next_token()? {
        Some(Token::Atom { value, offset }) => (value, offset),
        Some(other) => {
            return Err(SmtGateError::InvalidSetOption {
                offset: other.offset(),
                message: "expected an option keyword",
            });
        }
        None => {
            return Err(SmtGateError::InvalidSetOption {
                offset: lexer.cursor(),
                message: "missing option keyword",
            });
        }
    };
    if !is_allowed_set_option(option) {
        return Err(SmtGateError::DisallowedOption {
            option: option.to_owned(),
            offset: option_offset,
        });
    }

    if option == ":opt.priority" {
        match lexer.next_token()? {
            Some(Token::Atom {
                value: "lex" | "pareto" | "box",
                ..
            }) => {}
            Some(other) => {
                return Err(SmtGateError::InvalidSetOption {
                    offset: other.offset(),
                    message: ":opt.priority requires lex, pareto, or box",
                });
            }
            None => {
                return Err(SmtGateError::InvalidSetOption {
                    offset: lexer.cursor(),
                    message: "missing :opt.priority value",
                });
            }
        }
    } else {
        match lexer.next_token()? {
            Some(Token::Atom {
                value: "true" | "false",
                ..
            }) => {}
            Some(other) => {
                return Err(SmtGateError::InvalidSetOption {
                    offset: other.offset(),
                    message: "allowed set-option keys require a Boolean value",
                });
            }
            None => {
                return Err(SmtGateError::InvalidSetOption {
                    offset: lexer.cursor(),
                    message: "missing set-option value",
                });
            }
        }
    }

    match lexer.next_token()? {
        Some(Token::Close { .. }) => Ok(()),
        Some(other) => Err(SmtGateError::InvalidSetOption {
            offset: other.offset(),
            message: "set-option accepts exactly one key and one value",
        }),
        None => Err(SmtGateError::InvalidSetOption {
            offset: lexer.cursor(),
            message: "unterminated set-option form",
        }),
    }
}

fn skip_form_tail(lexer: &mut Lexer<'_>, open_offset: usize) -> Result<(), SmtGateError> {
    let mut depth = 1_usize;
    loop {
        match lexer.next_token()? {
            Some(Token::Open { .. }) => depth += 1,
            Some(Token::Close { .. }) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Some(Token::Atom { .. } | Token::QuotedSymbol { .. } | Token::String { .. }) => {}
            None => {
                return Err(SmtGateError::Malformed {
                    offset: open_offset,
                    message: "unterminated top-level form",
                });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Token<'a> {
    Open { offset: usize },
    Close { offset: usize },
    Atom { value: &'a str, offset: usize },
    QuotedSymbol { offset: usize },
    String { offset: usize },
}

impl Token<'_> {
    const fn offset(self) -> usize {
        match self {
            Self::Open { offset }
            | Self::Close { offset }
            | Self::Atom { offset, .. }
            | Self::QuotedSymbol { offset }
            | Self::String { offset } => offset,
        }
    }
}

#[derive(Debug)]
struct Lexer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            cursor: 0,
        }
    }

    const fn cursor(&self) -> usize {
        self.cursor
    }

    fn next_token(&mut self) -> Result<Option<Token<'a>>, SmtGateError> {
        self.skip_trivia();
        let Some(byte) = self.bytes.get(self.cursor).copied() else {
            return Ok(None);
        };
        let offset = self.cursor;
        match byte {
            b'(' => {
                self.cursor += 1;
                Ok(Some(Token::Open { offset }))
            }
            b')' => {
                self.cursor += 1;
                Ok(Some(Token::Close { offset }))
            }
            b'"' => {
                self.scan_string(offset)?;
                Ok(Some(Token::String { offset }))
            }
            b'|' => {
                self.scan_quoted_symbol(offset)?;
                Ok(Some(Token::QuotedSymbol { offset }))
            }
            _ => Ok(Some(self.scan_atom(offset))),
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            while self
                .bytes
                .get(self.cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.cursor += 1;
            }
            if self.bytes.get(self.cursor) != Some(&b';') {
                return;
            }
            while self
                .bytes
                .get(self.cursor)
                .is_some_and(|byte| *byte != b'\n')
            {
                self.cursor += 1;
            }
        }
    }

    fn scan_atom(&mut self, offset: usize) -> Token<'a> {
        while self.bytes.get(self.cursor).is_some_and(|byte| {
            !byte.is_ascii_whitespace() && !matches!(byte, b'(' | b')' | b';' | b'"' | b'|')
        }) {
            self.cursor += 1;
        }
        Token::Atom {
            value: &self.input[offset..self.cursor],
            offset,
        }
    }

    fn scan_string(&mut self, offset: usize) -> Result<(), SmtGateError> {
        self.cursor += 1;
        loop {
            match self.bytes.get(self.cursor).copied() {
                Some(b'"') if self.bytes.get(self.cursor + 1) == Some(&b'"') => {
                    self.cursor += 2;
                }
                Some(b'"') => {
                    self.cursor += 1;
                    return Ok(());
                }
                Some(_) => self.cursor += 1,
                None => {
                    return Err(SmtGateError::Malformed {
                        offset,
                        message: "unterminated string literal",
                    });
                }
            }
        }
    }

    fn scan_quoted_symbol(&mut self, offset: usize) -> Result<(), SmtGateError> {
        self.cursor += 1;
        while let Some(byte) = self.bytes.get(self.cursor).copied() {
            self.cursor += 1;
            if byte == b'|' {
                return Ok(());
            }
        }
        Err(SmtGateError::Malformed {
            offset,
            message: "unterminated quoted symbol",
        })
    }
}
