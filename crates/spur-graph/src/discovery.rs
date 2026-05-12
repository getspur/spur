use std::path::{Path, PathBuf};

use anyhow::Context;
use ignore::{DirEntry, WalkBuilder};

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
        let entry = entry.with_context(|| format!("failed to walk `{}`", root.display()))?;
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
