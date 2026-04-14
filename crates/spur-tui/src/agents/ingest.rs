//! Runtime impl of ingest hooks. `run_ingest_hook(binding, params)`
//! dispatches on the binding's parser + item_schema enums and returns the
//! decoded items (or None on parse failure).

use serde_json::Value;
use spur_acp::{AvailableCommand, IngestBinding, IngestParserKind, ItemSchemaKind};

/// Decode a vendor-ext notification's params payload into AvailableCommands
/// according to `binding`. Returns `None` if the expected path is missing or
/// the payload doesn't match the item schema — the caller should log and
/// move on rather than treat this as fatal.
pub fn run_ingest_hook(binding: &IngestBinding, params: &Value) -> Option<Vec<AvailableCommand>> {
    match binding.parser {
        IngestParserKind::JsonPathList => {
            let list = lookup_dotted_path(params, &binding.path)?;
            match binding.item_schema {
                ItemSchemaKind::AcpAvailableCommand => {
                    serde_json::from_value::<Vec<AvailableCommand>>(list).ok()
                }
            }
        }
    }
}

/// Walk a dotted path through a JSON value. Does not support array indexing
/// — `binding.path` is expected to be a field-name path like
/// `"availableCommands"` or `"result.items"`.
fn lookup_dotted_path(root: &Value, path: &str) -> Option<Value> {
    // Empty path ("") yields a single empty segment from `.split('.')`;
    // `.get("")` on any JSON object returns None, so an empty path
    // returns None (not the root). Treat empty path as a config error.
    let mut current = root;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_path_list_decodes_available_commands() {
        let binding = IngestBinding {
            method: "_kiro.dev/commands/available".into(),
            parser: IngestParserKind::JsonPathList,
            path: "availableCommands".into(),
            item_schema: ItemSchemaKind::AcpAvailableCommand,
        };
        let params = serde_json::json!({
            "availableCommands": [
                { "name": "context", "description": "manage context" }
            ]
        });
        let out = run_ingest_hook(&binding, &params).expect("decoded");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "context");
    }

    #[test]
    fn missing_path_returns_none() {
        let binding = IngestBinding {
            method: "x".into(),
            parser: IngestParserKind::JsonPathList,
            path: "nope".into(),
            item_schema: ItemSchemaKind::AcpAvailableCommand,
        };
        let params = serde_json::json!({ "something_else": [] });
        assert!(run_ingest_hook(&binding, &params).is_none());
    }

    #[test]
    fn dotted_path_traverses_nested() {
        let binding = IngestBinding {
            method: "x".into(),
            parser: IngestParserKind::JsonPathList,
            path: "result.items".into(),
            item_schema: ItemSchemaKind::AcpAvailableCommand,
        };
        let params = serde_json::json!({
            "result": { "items": [{ "name": "a", "description": "b" }] }
        });
        let out = run_ingest_hook(&binding, &params).expect("decoded");
        assert_eq!(out.len(), 1);
    }
}
