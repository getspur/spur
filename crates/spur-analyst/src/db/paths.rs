use std::path::{Path, PathBuf};

use spur_graph::resolve_worktree_root_from;

use crate::mcp::McpHandlerError;

pub(crate) fn analyst_db_path() -> Result<PathBuf, McpHandlerError> {
    let root = current_repo_root()?;
    Ok(select_analyst_db_path(&root))
}

pub(crate) fn current_repo_root() -> Result<PathBuf, McpHandlerError> {
    if let Some(worktree) = spur_graph::mcp::scoped_worktree_root() {
        return Ok(worktree);
    }
    let current_dir = std::env::current_dir().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read current directory: {error}"))
    })?;
    Ok(resolve_worktree_root_from(current_dir))
}

pub(crate) fn select_analyst_db_path(root: &Path) -> PathBuf {
    let local_db = root.join(".spur").join("analyst.duckdb");
    if local_db.exists() {
        return local_db;
    }
    parent_spur_worktree_analyst_db(root).unwrap_or(local_db)
}

fn parent_spur_worktree_analyst_db(root: &Path) -> Option<PathBuf> {
    for ancestor in root.ancestors() {
        if ancestor.file_name().is_some_and(|name| name == "worktrees") {
            let spur_dir = ancestor.parent()?;
            if spur_dir.file_name().is_some_and(|name| name == ".spur") {
                let db_path = spur_dir.join("analyst.duckdb");
                if db_path.exists() {
                    return Some(db_path);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn db_path_selection_falls_back_to_parent_spur_db_for_worker_worktree() {
        let repo_dir = tempfile::tempdir().expect("repo tempdir");
        let repo_spur = repo_dir.path().join(".spur");
        let worker_dir = repo_spur.join("worktrees").join("worker-1");
        std::fs::create_dir_all(&worker_dir).expect("create worker dir");
        std::fs::write(repo_spur.join("analyst.duckdb"), b"db").expect("write repo analyst db");

        let selected = super::select_analyst_db_path(&worker_dir);

        assert_eq!(selected, repo_spur.join("analyst.duckdb"));
    }
}
