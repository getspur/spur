//! Snapshot CAS pattern types. See spec section "Snapshot re-validation pattern".

use std::fmt;

/// A snapshot captured by `read_snapshot`, used as a CAS token by
/// `validate_and_commit`.
#[derive(Debug, Clone)]
pub struct Snapshot<S> {
    pub value: S,
    /// SQLite `PRAGMA data_version` at read time. Cheap monotonic counter
    /// that bumps whenever any other connection commits a write.
    pub data_version: i64,
}

#[derive(Debug, thiserror::Error)]
#[error("snapshot CAS conflict: state changed between read and validate")]
pub struct Conflict {
    pub data_version_expected: i64,
    pub data_version_actual: i64,
    pub detail: Option<String>,
}

impl Conflict {
    pub fn data_version(expected: i64, actual: i64) -> Self {
        Self {
            data_version_expected: expected,
            data_version_actual: actual,
            detail: None,
        }
    }

    pub fn with_detail(mut self, msg: impl fmt::Display) -> Self {
        self.detail = Some(msg.to_string());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_carries_value_and_version() {
        let s = Snapshot {
            value: 42_u32,
            data_version: 7,
        };
        assert_eq!(s.value, 42);
        assert_eq!(s.data_version, 7);
    }

    #[test]
    fn conflict_default_no_detail() {
        let c = Conflict::data_version(3, 5);
        assert!(c.detail.is_none());
        assert_eq!(c.data_version_expected, 3);
        assert_eq!(c.data_version_actual, 5);
    }

    #[test]
    fn conflict_with_detail() {
        let c = Conflict::data_version(3, 5).with_detail("issue bd-x changed");
        assert_eq!(c.detail.as_deref(), Some("issue bd-x changed"));
    }
}
