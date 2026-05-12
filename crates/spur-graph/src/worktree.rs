use std::path::PathBuf;

pub fn resolve_worktree_root() -> PathBuf {
    match std::env::current_dir() {
        Ok(current_dir) => resolve_worktree_root_from(current_dir),
        Err(err) => {
            tracing::warn!(error = %err, "failed to read current directory; using relative cwd");
            PathBuf::from(".")
        }
    }
}

pub fn resolve_worktree_root_from(start: impl Into<PathBuf>) -> PathBuf {
    let start = start.into();
    let mut candidate = start.as_path();

    loop {
        if candidate.join(".git").exists() {
            return candidate.to_path_buf();
        }

        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => {
                tracing::warn!(
                    cwd = %start.display(),
                    "no .git marker found while resolving worktree root; using current directory"
                );
                return start;
            }
        }
    }
}
