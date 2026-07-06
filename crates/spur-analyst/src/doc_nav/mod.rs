mod artifact;
mod projection;
mod query;

use globset::Glob;
use serde_json::{json, Value};
use spur_graph::CODE_SYMBOL_URI_PREFIX;

use crate::mcp::McpHandlerError;

use artifact::{current_worktree, open_doc_artifact_for_request, resolve_root_for_as_of};
use query::{child_hits, fts_hits, open_sections_table};

const DEFAULT_K: usize = 20;
const MAX_K: usize = 100;

pub(crate) async fn doc_navigate(args: &Value) -> Result<Value, McpHandlerError> {
    let request = DocNavigateRequest::parse(args)?;
    let worktree = current_worktree()?;
    let source = open_doc_artifact_for_request(&worktree).await?;
    let table = open_sections_table(source.artifact_dir()).await?;

    let mut hits = if let Some(root) = request.root.as_deref() {
        let root = resolve_root_for_as_of(
            source.artifact_dir(),
            &worktree,
            root,
            request.as_of.as_deref(),
            source.artifact(),
        )?;
        child_hits(&table, &root).await?
    } else {
        fts_hits(&table, &request).await?
    };

    if let Some(glob) = &request.file_glob {
        hits.retain(|hit| glob.is_match(&hit.file_path));
    }
    if hits.len() > request.k {
        hits.truncate(request.k);
    }

    Ok(json!({
        "hits": hits
            .into_iter()
            .map(|hit| hit.into_value(request.include_lede))
            .collect::<Vec<_>>()
    }))
}

struct DocNavigateRequest {
    query: Option<String>,
    root: Option<String>,
    k: usize,
    file_glob: Option<globset::GlobMatcher>,
    as_of: Option<String>,
    include_lede: bool,
}

impl DocNavigateRequest {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let root = optional_string(args, "root")?.map(strip_symbol_uri);
        let query = optional_string(args, "query")?;
        if root.is_none() && query.as_deref().is_none_or(str::is_empty) {
            return Err(McpHandlerError::InvalidParams(
                "field 'query' is required when 'root' is not set".to_owned(),
            ));
        }

        let requested_k = args
            .get("k")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_K as u64);
        let k = requested_k.clamp(1, MAX_K as u64) as usize;
        let file_glob = optional_string(args, "file_glob")?
            .map(|pattern| {
                Glob::new(&pattern)
                    .map(|glob| glob.compile_matcher())
                    .map_err(|error| {
                        McpHandlerError::InvalidParams(format!(
                            "invalid file_glob `{pattern}`: {error}"
                        ))
                    })
            })
            .transpose()?;
        let as_of = optional_string(args, "as_of")?;
        if as_of.as_deref().is_some_and(str::is_empty) {
            return Err(McpHandlerError::InvalidParams(
                "field 'as_of' must not be empty".to_owned(),
            ));
        }
        let include_lede = args
            .get("include_lede")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        Ok(Self {
            query,
            root,
            k,
            file_glob,
            as_of,
            include_lede,
        })
    }
}

fn optional_string(args: &Value, field: &str) -> Result<Option<String>, McpHandlerError> {
    match args.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be a string"
        ))),
    }
}

fn strip_symbol_uri(value: String) -> String {
    value
        .strip_prefix(CODE_SYMBOL_URI_PREFIX)
        .unwrap_or(value.as_str())
        .to_owned()
}
