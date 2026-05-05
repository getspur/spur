//! Init-time guards for the beads crate adapter.
//!
//! Each function is a precondition that must hold before `BeadsCrateAdapter`
//! is allowed to open the writer connection.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("non-local filesystem detected at {path}: fs_type = {fs_type}. \
             flock semantics are not portable here. \
             Set allow_non_local_fs=true in config to bypass.")]
    NonLocalFilesystem { path: String, fs_type: String },
}

/// Returns Ok(()) for local filesystems; Err for known network mounts (NFS, SMB, etc.).
/// Best-effort: on platforms where we cannot determine the FS type, returns Ok.
pub fn detect_local_fs(beads_dir: &Path) -> Result<(), InitError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(beads_dir.as_os_str().as_bytes()).unwrap();
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
        if rc != 0 { return Ok(()); } // can't determine; allow
        // Magic numbers from <linux/magic.h>
        const NFS_SUPER_MAGIC: i64 = 0x6969;
        const SMB_SUPER_MAGIC: i64 = 0x517B;
        const CIFS_MAGIC_NUMBER: i64 = 0xFF534D42_u64 as i64;
        let ty = buf.f_type as i64;
        if ty == NFS_SUPER_MAGIC || ty == SMB_SUPER_MAGIC || ty == CIFS_MAGIC_NUMBER {
            return Err(InitError::NonLocalFilesystem {
                path: beads_dir.display().to_string(),
                fs_type: format!("0x{:x}", ty),
            });
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(beads_dir.as_os_str().as_bytes()).unwrap();
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
        if rc != 0 { return Ok(()); }
        let fs_name = unsafe {
            let raw = buf.f_fstypename.as_ptr();
            std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned()
        };
        if matches!(fs_name.as_str(), "nfs" | "smbfs" | "cifs" | "afpfs") {
            return Err(InitError::NonLocalFilesystem {
                path: beads_dir.display().to_string(),
                fs_type: fs_name,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn local_tempdir_is_local() {
        let dir = TempDir::new().unwrap();
        // tmpfs / APFS local — should pass
        assert!(detect_local_fs(dir.path()).is_ok());
    }
}
