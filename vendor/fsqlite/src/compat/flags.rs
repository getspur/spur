//! Connection open flags, analogous to `rusqlite::OpenFlags`.

use std::path::Path;

use fsqlite_error::FrankenError;
use fsqlite_types::flags::VfsOpenFlags;

use crate::Connection;

/// Subset of SQLite open flags that cass uses, mirroring `rusqlite::OpenFlags`.
///
/// Under the hood these map to `VfsOpenFlags`.
#[derive(Debug, Clone, Copy)]
pub struct OpenFlags(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenDisposition {
    ReadOnly,
    WriteExisting,
    WriteCreate,
}

impl OpenFlags {
    /// Open the database in read-only mode.
    pub const SQLITE_OPEN_READ_ONLY: Self = Self(0x01);

    /// Open the database for reading and writing.
    pub const SQLITE_OPEN_READ_WRITE: Self = Self(0x02);

    /// Create the database if it does not exist (combined with READ_WRITE).
    pub const SQLITE_OPEN_CREATE: Self = Self(0x04);

    /// Default flags: READ_WRITE | CREATE.
    pub fn default_flags() -> Self {
        Self(Self::SQLITE_OPEN_READ_WRITE.0 | Self::SQLITE_OPEN_CREATE.0)
    }

    /// Combine two flag sets with bitwise OR.
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Check if a flag is set.
    pub fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    /// Convert to `VfsOpenFlags`.
    pub fn to_vfs_flags(self) -> VfsOpenFlags {
        let mut flags = VfsOpenFlags::MAIN_DB;
        if self.contains(Self::SQLITE_OPEN_READ_ONLY) {
            flags |= VfsOpenFlags::READONLY;
        } else if self.contains(Self::SQLITE_OPEN_READ_WRITE) {
            flags |= VfsOpenFlags::READWRITE;
        }
        if self.contains(Self::SQLITE_OPEN_CREATE) {
            flags |= VfsOpenFlags::CREATE;
        }
        flags
    }
}

fn classify_access_mode(flags: OpenFlags) -> Result<OpenDisposition, FrankenError> {
    let read_only = flags.contains(OpenFlags::SQLITE_OPEN_READ_ONLY);
    let read_write = flags.contains(OpenFlags::SQLITE_OPEN_READ_WRITE);
    let create = flags.contains(OpenFlags::SQLITE_OPEN_CREATE);

    match (read_only, read_write, create) {
        (true, false, false) => Ok(OpenDisposition::ReadOnly),
        (false, true, false) => Ok(OpenDisposition::WriteExisting),
        (false, true, true) => Ok(OpenDisposition::WriteCreate),
        _ => Err(FrankenError::TypeMismatch {
            expected:
                "one of SQLITE_OPEN_READ_ONLY, SQLITE_OPEN_READ_WRITE, or SQLITE_OPEN_READ_WRITE | SQLITE_OPEN_CREATE"
                    .into(),
            actual: format!("open flags 0x{:x}", flags.0),
        }),
    }
}

fn open_read_only_connection(path: &str) -> Result<Connection, FrankenError> {
    if path == ":memory:" {
        return Err(FrankenError::NotImplemented(
            "read-only :memory: connections are not supported".to_owned(),
        ));
    }
    Connection::open_schema_only(path)
}

impl std::ops::BitOr for OpenFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

/// Open a connection with the given flags.
///
/// When `SQLITE_OPEN_READ_ONLY` is set, the connection is opened in
/// schema-only mode: table/index/view/trigger definitions are loaded
/// but no row data is read into the in-memory `MemDatabase`. Queries
/// are served through pager-backed B-tree cursors, which read directly
/// from the on-disk pages. This makes opening even multi-gigabyte
/// databases near-instantaneous.
///
/// # Examples
///
/// ```ignore
/// use fsqlite::compat::{OpenFlags, open_with_flags};
///
/// let conn = open_with_flags("my.db", OpenFlags::SQLITE_OPEN_READ_ONLY)?;
/// ```
pub fn open_with_flags(path: &str, flags: OpenFlags) -> Result<Connection, FrankenError> {
    match classify_access_mode(flags)? {
        OpenDisposition::ReadOnly => open_read_only_connection(path),
        OpenDisposition::WriteExisting => {
            if path != ":memory:" && !Path::new(path).exists() {
                return Err(FrankenError::CannotOpen { path: path.into() });
            }
            Connection::open(path)
        }
        OpenDisposition::WriteCreate => Connection::open(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_flags_contain_rw_and_create() {
        let flags = OpenFlags::default_flags();
        assert!(flags.contains(OpenFlags::SQLITE_OPEN_READ_WRITE));
        assert!(flags.contains(OpenFlags::SQLITE_OPEN_CREATE));
    }

    #[test]
    fn bitor_combines_flags() {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
        assert!(flags.contains(OpenFlags::SQLITE_OPEN_READ_WRITE));
        assert!(flags.contains(OpenFlags::SQLITE_OPEN_CREATE));
    }

    #[test]
    fn open_with_flags_in_memory() {
        let conn = open_with_flags(":memory:", OpenFlags::default_flags()).unwrap();
        assert_eq!(conn.path(), ":memory:");
    }

    #[test]
    fn vfs_flags_conversion() {
        let flags = OpenFlags::default_flags();
        let vfs = flags.to_vfs_flags();
        assert!(vfs.contains(VfsOpenFlags::READWRITE));
        assert!(vfs.contains(VfsOpenFlags::CREATE));
        assert!(vfs.contains(VfsOpenFlags::MAIN_DB));
    }

    #[test]
    fn vfs_flags_conversion_preserves_read_only() {
        let vfs = OpenFlags::SQLITE_OPEN_READ_ONLY.to_vfs_flags();
        assert!(vfs.contains(VfsOpenFlags::READONLY));
        assert!(!vfs.contains(VfsOpenFlags::READWRITE));
        assert!(vfs.contains(VfsOpenFlags::MAIN_DB));
    }

    #[test]
    fn vfs_flags_conversion_prefers_read_only_when_both_are_present() {
        let vfs =
            (OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_READ_WRITE).to_vfs_flags();
        assert!(vfs.contains(VfsOpenFlags::READONLY));
        assert!(!vfs.contains(VfsOpenFlags::READWRITE));
    }

    #[test]
    fn open_with_flags_read_write_without_create_missing_db_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("missing.db");
        let error = open_with_flags(path.to_str().unwrap(), OpenFlags::SQLITE_OPEN_READ_WRITE)
            .expect_err("READ_WRITE without CREATE should not create a missing database");
        assert!(matches!(error, FrankenError::CannotOpen { .. }));
        assert!(!path.exists());
    }

    #[test]
    fn classify_access_mode_rejects_create_without_read_write() {
        let error = classify_access_mode(OpenFlags::SQLITE_OPEN_CREATE)
            .expect_err("CREATE alone is not a valid sqlite3_open_v2 access mode");
        assert!(matches!(error, FrankenError::TypeMismatch { .. }));
    }

    #[test]
    fn classify_access_mode_rejects_read_only_create_combo() {
        let error =
            classify_access_mode(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_CREATE)
                .expect_err("READ_ONLY | CREATE is not a valid sqlite3_open_v2 access mode");
        assert!(matches!(error, FrankenError::TypeMismatch { .. }));
    }

    #[test]
    fn open_with_flags_read_only_in_memory_is_rejected() {
        let error = open_with_flags(":memory:", OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect_err("compat open must not return a writable connection for READ_ONLY");
        assert!(matches!(error, FrankenError::NotImplemented(_)));
    }
}
