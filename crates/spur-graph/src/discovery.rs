use std::path::{Path, PathBuf};

use anyhow::Context;
use ignore::{DirEntry, Error as IgnoreError, WalkBuilder};

pub fn discover_files(root: &Path, allowed_extensions: &[&str]) -> anyhow::Result<Vec<PathBuf>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", root.display()))?;
    let mut files = Vec::new();

    for entry in WalkBuilder::new(&root)
        .standard_filters(true)
        .filter_entry(|entry| should_descend(entry))
        .build()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                let path = walk_error_path(&err)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_string());
                tracing::warn!(
                    root = %root.display(),
                    path = %path,
                    error = %err,
                    "spur-graph: skipping entry (walk failed)"
                );
                continue;
            }
        };
        if entry
            .file_type()
            .map(|file_type| file_type.is_file())
            .unwrap_or(false)
            && entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    allowed_extensions
                        .iter()
                        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
                })
        {
            files.push(entry.into_path());
        }
    }

    files.sort();
    Ok(files)
}

fn walk_error_path(err: &IgnoreError) -> Option<&Path> {
    match err {
        IgnoreError::Partial(errs) => errs.iter().find_map(walk_error_path),
        IgnoreError::WithLineNumber { err, .. } => walk_error_path(err),
        IgnoreError::WithPath { path, .. } => Some(path.as_path()),
        IgnoreError::WithDepth { err, .. } => walk_error_path(err),
        IgnoreError::Loop { child, .. } => Some(child.as_path()),
        _ => None,
    }
}

fn should_descend(entry: &DirEntry) -> bool {
    let Some(file_name) = entry.file_name().to_str() else {
        return true;
    };
    if file_name == "target" || file_name == ".git" || file_name == "node_modules" {
        return false;
    }
    if entry.depth() > 0 && file_name.starts_with('.') {
        return false;
    }
    true
}
