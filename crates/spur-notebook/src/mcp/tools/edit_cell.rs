use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::{bridge::BridgeRequester, ServerDeps};

pub const METHOD: &str = "notebook.edit_cell";
const LAST_EDITED_BY: &str = "brain";

#[derive(Debug, Deserialize, Clone)]
pub struct CellEdit {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Deserialize)]
struct EditCellParams {
    id: String,
    edits: Vec<CellEdit>,
    expected_version: Option<u64>,
}

#[derive(Debug)]
pub struct AppliedEdits {
    pub source: String,
    pub replacements: usize,
    /// Byte offset in the resulting source of the start of the last replacement.
    pub last_edit_offset: usize,
}

#[derive(Debug)]
pub enum EditError {
    NotFound {
        index: usize,
        id: String,
    },
    Ambiguous {
        index: usize,
        id: String,
        count: usize,
    },
}

/// Apply a sequence of string-replacement edits sequentially to `source`.
///
/// Edits are applied in order against the evolving source. Each edit must match
/// exactly once (non-overlapping, exact byte match) unless `replace_all` is true.
///
/// Returns `Err(EditError)` if any edit fails. On error the call is aborted
/// before any write — edits are atomic as a set.
pub fn apply_edits(source: &str, edits: &[CellEdit]) -> Result<AppliedEdits, EditError> {
    let mut current = source.to_string();
    let mut total_replacements: usize = 0;
    let mut last_edit_offset: usize = 0;

    for (index, edit) in edits.iter().enumerate() {
        let count = current.matches(edit.old_string.as_str()).count();
        if count == 0 {
            return Err(EditError::NotFound {
                index,
                id: String::new(),
            });
        }
        if count > 1 && !edit.replace_all {
            return Err(EditError::Ambiguous {
                index,
                id: String::new(),
                count,
            });
        }

        last_edit_offset = current.find(edit.old_string.as_str()).unwrap_or(0);
        if edit.replace_all {
            current = current.replace(edit.old_string.as_str(), edit.new_string.as_str());
            total_replacements += count;
        } else {
            current = current.replacen(edit.old_string.as_str(), edit.new_string.as_str(), 1);
            total_replacements += 1;
        }
    }

    Ok(AppliedEdits {
        source: current,
        replacements: total_replacements,
        last_edit_offset,
    })
}

/// Extract up to ~10 lines of context centered on the given byte offset.
fn snippet_around(source: &str, offset: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return source.to_string();
    }
    let mut cumulative: usize = 0;
    let mut target_line: usize = 0;
    for (i, line) in lines.iter().enumerate() {
        let next = cumulative + line.len() + 1; // +1 for newline
        if offset < next {
            target_line = i;
            break;
        }
        cumulative = next;
        target_line = i;
    }
    let half = 5usize;
    let start = target_line.saturating_sub(half);
    let end = (target_line + half + 1).min(lines.len());
    lines[start..end].join("\n")
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Apply targeted string-replacement edits to one cell without rewriting its full \
         source. Each `old_string` must match the current cell source exactly once (include \
         surrounding context to disambiguate) unless `replace_all` is true. Edits apply in \
         order. Prefer this over `notebook.write_cell` for small changes to large cells; \
         prefer `notebook.write_cell` for short cells or full rewrites.",
        rmcp_object(json!({
            "type": "object",
            "required": ["id", "edits"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["old_string", "new_string"],
                        "properties": {
                            "old_string": { "type": "string", "minLength": 1 },
                            "new_string": { "type": "string" },
                            "replace_all": { "type": "boolean", "default": false }
                        },
                        "additionalProperties": false
                    }
                },
                "expected_version": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

fn validate_params(params: &EditCellParams) -> Result<(), McpError> {
    if params.id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.edit_cell id must not be empty",
            None,
        ));
    }
    if params.edits.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.edit_cell edits must not be empty",
            None,
        ));
    }
    for (i, edit) in params.edits.iter().enumerate() {
        if edit.old_string.is_empty() {
            return Err(McpError::invalid_params(
                format!("notebook.edit_cell edit {i}: old_string must not be empty"),
                Some(json!({ "edit_index": i })),
            ));
        }
        if edit.old_string == edit.new_string {
            return Err(McpError::invalid_params(
                format!("notebook.edit_cell edit {i}: old_string and new_string must differ"),
                Some(json!({ "edit_index": i })),
            ));
        }
    }
    Ok(())
}

fn is_stale_version_error(err: &McpError) -> bool {
    if let Some(data) = &err.data {
        if let Some(code) = data.get("code") {
            return code.as_str() == Some("stale_version");
        }
    }
    false
}

/// Single read → apply → write attempt.
async fn run_edit_attempt(
    bridge: &dyn BridgeRequester,
    id: &str,
    edits: &[CellEdit],
    pinned_version: Option<u64>,
) -> Result<(u64, usize, bool, String), McpError> {
    // 1. Read current source + version.
    let read_value = bridge
        .request("notebook.read_cell", json!({ "id": id }), BRIDGE_TIMEOUT)
        .await
        .map_err(|e| e.into_mcp_error())?;

    let current_version: u64 = read_value["version"].as_u64().ok_or_else(|| {
        McpError::internal_error(
            "invalid notebook.read_cell bridge response: missing version",
            None,
        )
    })?;
    let current_source: String = read_value["source"]
        .as_str()
        .ok_or_else(|| {
            McpError::internal_error(
                "invalid notebook.read_cell bridge response: missing source",
                None,
            )
        })?
        .to_string();

    // 2. Version-pin check (before applying edits).
    if let Some(pinned) = pinned_version {
        if current_version != pinned {
            return Err(McpError::invalid_params(
                format!(
                    "notebook.edit_cell stale_version: expected_version {pinned} but cell \
                     {id} is at version {current_version}"
                ),
                Some(json!({
                    "code": "stale_version",
                    "expected_version": pinned,
                    "current_version": current_version
                })),
            ));
        }
    }

    // 3. Apply edits in Rust.
    let applied = apply_edits(&current_source, edits).map_err(|err| match err {
        EditError::NotFound { index, .. } => McpError::invalid_params(
            format!("notebook.edit_cell edit {index}: old_string not found in cell {id}"),
            Some(json!({ "edit_index": index, "cell_id": id })),
        ),
        EditError::Ambiguous { index, count, .. } => McpError::invalid_params(
            format!(
                "notebook.edit_cell edit {index}: old_string matched {count} times in \
                 cell {id}; include more surrounding context to make it unique, or set replace_all"
            ),
            Some(json!({ "edit_index": index, "cell_id": id, "match_count": count })),
        ),
    })?;

    // 4. No-change short-circuit.
    if applied.source == current_source {
        return Ok((current_version, applied.replacements, false, String::new()));
    }

    // 5. Compute snippet centered on last edit.
    let snippet = snippet_around(&applied.source, applied.last_edit_offset);

    // 6. Write via bridge.
    let write_result = bridge
        .request(
            "notebook.write_cell",
            json!({
                "id": id,
                "source": applied.source,
                "expected_version": current_version,
                "last_edited_by": LAST_EDITED_BY
            }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|e| e.into_mcp_error())?;

    let new_version: u64 = write_result["version"].as_u64().ok_or_else(|| {
        McpError::internal_error(
            "invalid notebook.write_cell bridge response: missing version",
            None,
        )
    })?;

    Ok((new_version, applied.replacements, true, snippet))
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let bridge = deps.bridge.as_ref();

    let params: EditCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.edit_cell requires { id, edits }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    // Validate before any bridge call.
    validate_params(&params)?;

    let id = &params.id;
    let edits = &params.edits;
    let pinned_version = params.expected_version;

    let (version, replacements, changed, snippet) =
        match run_edit_attempt(bridge, id, edits, pinned_version).await {
            Ok(result) => result,
            Err(err) => {
                // Stale-write retry only when the caller did NOT pin expected_version
                // and the write failed with stale_version.
                if pinned_version.is_none() && is_stale_version_error(&err) {
                    run_edit_attempt(bridge, id, edits, None).await?
                } else {
                    return Err(err);
                }
            }
        };

    Ok(CallToolResult::structured(json!({
        "version": version,
        "replacements": replacements,
        "changed": changed,
        "snippet": snippet
    })))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use crate::mcp::{
        bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
        ServerDeps,
    };

    use super::{apply_edits, call, CellEdit};

    // ---------------------------------------------------------------------------
    // Test bridge helpers
    // ---------------------------------------------------------------------------

    /// A bridge that serves a fixed read_cell response and captures write_cell calls.
    struct CapturingBridge {
        read_source: String,
        read_version: u64,
        write_params: Mutex<Option<Value>>,
        write_version: u64,
    }

    impl CapturingBridge {
        fn new(source: impl Into<String>, version: u64, write_version: u64) -> Arc<Self> {
            Arc::new(Self {
                read_source: source.into(),
                read_version: version,
                write_params: Mutex::new(None),
                write_version,
            })
        }

        fn take_write_params(&self) -> Option<Value> {
            self.write_params.lock().expect("params lock").take()
        }
    }

    impl BridgeRequester for CapturingBridge {
        fn listener_registered(&self) -> bool {
            true
        }
        fn window_alive(&self) -> bool {
            true
        }
        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            method: &'static str,
            params: Value,
            _timeout: std::time::Duration,
        ) -> BridgeRequestFuture<'a> {
            match method {
                "notebook.read_cell" => {
                    let response = json!({
                        "id": params["id"],
                        "kind": "code",
                        "version": self.read_version,
                        "source": self.read_source,
                        "exec_count": null,
                        "status": "idle",
                        "outputs": []
                    });
                    Box::pin(async move { Ok(response) })
                }
                "notebook.write_cell" => {
                    *self.write_params.lock().expect("params lock") = Some(params);
                    let version = self.write_version;
                    Box::pin(async move { Ok(json!({ "version": version })) })
                }
                other => {
                    let msg = format!("unexpected method: {other}");
                    Box::pin(async move {
                        Err(BridgeError::Handler {
                            code: "unexpected_method".to_string(),
                            message: msg,
                        })
                    })
                }
            }
        }
    }

    /// A bridge that returns `stale_version` on the first write, then succeeds.
    struct RetryBridge {
        read_source: String,
        read_version: u64,
        write_call_count: Mutex<u32>,
        write_version: u64,
    }

    impl RetryBridge {
        fn new(source: impl Into<String>, version: u64, write_version: u64) -> Arc<Self> {
            Arc::new(Self {
                read_source: source.into(),
                read_version: version,
                write_call_count: Mutex::new(0),
                write_version,
            })
        }
    }

    impl BridgeRequester for RetryBridge {
        fn listener_registered(&self) -> bool {
            true
        }
        fn window_alive(&self) -> bool {
            true
        }
        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            method: &'static str,
            params: Value,
            _timeout: std::time::Duration,
        ) -> BridgeRequestFuture<'a> {
            match method {
                "notebook.read_cell" => {
                    let source = self.read_source.clone();
                    let count = *self.write_call_count.lock().expect("lock");
                    // After the first failed write, serve incremented version so the
                    // retry's implicit version matches what we write with.
                    let version = if count >= 1 {
                        self.read_version + 1
                    } else {
                        self.read_version
                    };
                    let id = params["id"].clone();
                    Box::pin(async move {
                        Ok(json!({
                            "id": id,
                            "kind": "code",
                            "version": version,
                            "source": source,
                            "exec_count": null,
                            "status": "idle",
                            "outputs": []
                        }))
                    })
                }
                "notebook.write_cell" => {
                    let mut count = self.write_call_count.lock().expect("lock");
                    *count += 1;
                    let n = *count;
                    let write_version = self.write_version;
                    if n == 1 {
                        Box::pin(async move {
                            Err(BridgeError::Handler {
                                code: "stale_version".to_string(),
                                message: "version mismatch".to_string(),
                            })
                        })
                    } else {
                        Box::pin(async move { Ok(json!({ "version": write_version })) })
                    }
                }
                other => {
                    let msg = format!("unexpected: {other}");
                    Box::pin(async move {
                        Err(BridgeError::Handler {
                            code: "unexpected".to_string(),
                            message: msg,
                        })
                    })
                }
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Unit tests for apply_edits (pure function, no bridge)
    // ---------------------------------------------------------------------------

    #[test]
    fn apply_edits_single_replacement() {
        let source = "hello world";
        let edits = vec![CellEdit {
            old_string: "world".to_string(),
            new_string: "Rust".to_string(),
            replace_all: false,
        }];
        let result = apply_edits(source, &edits).expect("single edit should succeed");
        assert_eq!(result.source, "hello Rust");
        assert_eq!(result.replacements, 1);
    }

    #[test]
    fn apply_edits_not_found_returns_error() {
        let source = "hello world";
        let edits = vec![CellEdit {
            old_string: "missing".to_string(),
            new_string: "x".to_string(),
            replace_all: false,
        }];
        let err = apply_edits(source, &edits).expect_err("missing old_string should fail");
        match err {
            super::EditError::NotFound { index, .. } => assert_eq!(index, 0),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn apply_edits_ambiguous_without_replace_all() {
        let source = "foo bar foo baz foo";
        let edits = vec![CellEdit {
            old_string: "foo".to_string(),
            new_string: "qux".to_string(),
            replace_all: false,
        }];
        let err = apply_edits(source, &edits).expect_err("ambiguous should fail");
        match err {
            super::EditError::Ambiguous { index, count, .. } => {
                assert_eq!(index, 0);
                assert_eq!(count, 3);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn apply_edits_replace_all_replaces_every_occurrence() {
        let source = "foo bar foo baz foo";
        let edits = vec![CellEdit {
            old_string: "foo".to_string(),
            new_string: "qux".to_string(),
            replace_all: true,
        }];
        let result = apply_edits(source, &edits).expect("replace_all should succeed");
        assert_eq!(result.source, "qux bar qux baz qux");
        assert_eq!(result.replacements, 3);
    }

    #[test]
    fn apply_edits_sequential_edits_chain() {
        let source = "hello world";
        let edits = vec![
            CellEdit {
                old_string: "hello".to_string(),
                new_string: "goodbye cruel".to_string(),
                replace_all: false,
            },
            CellEdit {
                old_string: "cruel world".to_string(),
                new_string: "place".to_string(),
                replace_all: false,
            },
        ];
        let result = apply_edits(source, &edits).expect("sequential edits should succeed");
        assert_eq!(result.source, "goodbye place");
        assert_eq!(result.replacements, 2);
    }

    // ---------------------------------------------------------------------------
    // Integration tests — all 10 spec test cases
    // ---------------------------------------------------------------------------

    // Test 1: Happy path
    #[tokio::test]
    async fn happy_path_single_edit_writes_spliced_source() {
        let bridge = CapturingBridge::new("hello world", 4, 5);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let result = call(
            &deps,
            json!({
                "id": "cell-1",
                "edits": [{ "old_string": "world", "new_string": "Rust" }]
            }),
        )
        .await
        .expect("happy path should succeed");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["version"], 5, "version from write_cell");
        assert_eq!(body["replacements"], 1);
        assert_eq!(body["changed"], true);
        assert!(
            !body["snippet"].as_str().unwrap_or("").is_empty(),
            "snippet non-empty"
        );

        let write_params = bridge
            .take_write_params()
            .expect("write should have been called");
        assert_eq!(write_params["id"], "cell-1");
        assert_eq!(write_params["source"], "hello Rust");
        assert_eq!(write_params["expected_version"], 4);
        assert_eq!(write_params["last_edited_by"], "brain");
    }

    // Test 2: Not found — no write dispatched
    #[tokio::test]
    async fn not_found_no_write_dispatched() {
        let bridge = CapturingBridge::new("hello world", 1, 2);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let error = call(
            &deps,
            json!({
                "id": "cell-A",
                "edits": [{ "old_string": "missing", "new_string": "x" }]
            }),
        )
        .await
        .expect_err("not found should return error");

        assert!(
            error.message.contains("not found") || error.message.contains("old_string"),
            "error message should mention not found: {:?}",
            error.message
        );
        assert!(
            error.message.contains("cell-A"),
            "error should name the cell: {:?}",
            error.message
        );
        assert!(
            error.message.contains("0"),
            "error should name edit index 0: {:?}",
            error.message
        );
        assert!(bridge.take_write_params().is_none(), "no write expected");
    }

    // Test 3: Ambiguous — no write dispatched
    #[tokio::test]
    async fn ambiguous_no_write_dispatched() {
        let bridge = CapturingBridge::new("foo bar foo baz", 1, 2);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let error = call(
            &deps,
            json!({
                "id": "cell-B",
                "edits": [{ "old_string": "foo", "new_string": "qux" }]
            }),
        )
        .await
        .expect_err("ambiguous should return error");

        assert!(
            error.message.contains("2") || error.message.contains("matched"),
            "error should mention match count: {:?}",
            error.message
        );
        assert!(bridge.take_write_params().is_none(), "no write expected");
    }

    // Test 4: replace_all replaces every occurrence; `replacements` reflects count
    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let bridge = CapturingBridge::new("foo bar foo baz foo", 2, 3);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let result = call(
            &deps,
            json!({
                "id": "cell-C",
                "edits": [{ "old_string": "foo", "new_string": "qux", "replace_all": true }]
            }),
        )
        .await
        .expect("replace_all should succeed");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["replacements"], 3);
        assert_eq!(body["changed"], true);

        let write_params = bridge.take_write_params().expect("write called");
        assert_eq!(write_params["source"], "qux bar qux baz qux");
    }

    // Test 5: Sequential edits — edit 2 matches text produced by edit 1
    #[tokio::test]
    async fn sequential_edits_chain() {
        let bridge = CapturingBridge::new("hello world", 1, 2);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let result = call(
            &deps,
            json!({
                "id": "cell-D",
                "edits": [
                    { "old_string": "hello", "new_string": "goodbye cruel" },
                    { "old_string": "cruel world", "new_string": "place" }
                ]
            }),
        )
        .await
        .expect("sequential edits should succeed");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["replacements"], 2);

        let write_params = bridge.take_write_params().expect("write called");
        assert_eq!(write_params["source"], "goodbye place");
    }

    // Test 6: Pinned version mismatch — stale error before any write
    #[tokio::test]
    async fn pinned_version_mismatch_no_write() {
        // read returns version 5, caller pins 3
        let bridge = CapturingBridge::new("hello world", 5, 6);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let error = call(
            &deps,
            json!({
                "id": "cell-E",
                "edits": [{ "old_string": "hello", "new_string": "hi" }],
                "expected_version": 3
            }),
        )
        .await
        .expect_err("pinned version mismatch should error");

        assert!(
            error.message.contains("stale_version") || error.message.contains("stale"),
            "error should mention stale_version: {:?}",
            error.message
        );
        assert!(bridge.take_write_params().is_none(), "no write expected");
    }

    // Test 7: Param validation — old_string == new_string rejected before bridge
    #[tokio::test]
    async fn param_validation_old_equals_new_rejected() {
        let bridge = CapturingBridge::new("hello world", 3, 4);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let error = call(
            &deps,
            json!({
                "id": "cell-F",
                "edits": [{ "old_string": "hello", "new_string": "hello" }]
            }),
        )
        .await
        .expect_err("old_string == new_string should be rejected");

        assert!(
            error.message.contains("old_string") || error.message.contains("new_string"),
            "error should mention old/new string: {:?}",
            error.message
        );
        assert!(
            bridge.take_write_params().is_none(),
            "no bridge write expected"
        );
    }

    // Test 7b: No-change roundtrip skips write
    #[tokio::test]
    async fn no_change_roundtrip_skips_write() {
        let bridge = CapturingBridge::new("hello world", 3, 4);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let result = call(
            &deps,
            json!({
                "id": "cell-G",
                "edits": [
                    { "old_string": "hello", "new_string": "hi" },
                    { "old_string": "hi", "new_string": "hello" }
                ]
            }),
        )
        .await
        .expect("roundtrip no-change should succeed");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["changed"], false);
        assert_eq!(body["version"], 3, "current version when unchanged");
        assert!(
            bridge.take_write_params().is_none(),
            "no write expected for no-change"
        );
    }

    // Test 8a: Stale-write retry — unpinned version retries once and succeeds
    #[tokio::test]
    async fn stale_write_retry_without_pinned_version_succeeds() {
        let bridge = RetryBridge::new("hello world", 1, 3);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let result = call(
            &deps,
            json!({
                "id": "cell-H",
                "edits": [{ "old_string": "hello", "new_string": "hi" }]
            }),
        )
        .await
        .expect("retry should succeed on second attempt");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["version"], 3);
        assert_eq!(body["changed"], true);
    }

    // Test 8b: Stale-write with pinned version propagates immediately
    #[tokio::test]
    async fn stale_write_with_pinned_version_propagates() {
        let bridge = RetryBridge::new("hello world", 1, 3);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let error = call(
            &deps,
            json!({
                "id": "cell-I",
                "edits": [{ "old_string": "hello", "new_string": "hi" }],
                "expected_version": 1
            }),
        )
        .await
        .expect_err("pinned version stale should propagate immediately");

        assert!(
            error.message.contains("stale_version") || error.message.contains("stale"),
            "error should mention stale: {:?}",
            error.message
        );
    }

    // Test 9: Param validation — various invalid inputs
    #[tokio::test]
    async fn param_validation_empty_edits_rejected() {
        let bridge = CapturingBridge::new("source", 1, 2);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let error = call(&deps, json!({ "id": "cell-J", "edits": [] }))
            .await
            .expect_err("empty edits should be rejected");

        assert!(
            error.message.contains("edits"),
            "error should mention edits: {:?}",
            error.message
        );
        assert!(bridge.take_write_params().is_none(), "no bridge traffic");
    }

    #[tokio::test]
    async fn param_validation_empty_old_string_rejected() {
        let bridge = CapturingBridge::new("source", 1, 2);
        let deps = ServerDeps::from_bridge(bridge.clone());

        let _error = call(
            &deps,
            json!({
                "id": "cell-K",
                "edits": [{ "old_string": "", "new_string": "x" }]
            }),
        )
        .await
        .expect_err("empty old_string should be rejected");

        assert!(bridge.take_write_params().is_none(), "no bridge traffic");
    }

    // Test 10: Registry test is in mod.rs (tools_include_direct_notebook_file_tools)
    // — extended there to assert notebook.edit_cell is present.
}
