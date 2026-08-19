use serde_json::{Map, Value};
use std::borrow::Cow;

/// Borrow the payload from an envelope containing exactly one non-error text block.
///
/// This is the allocation-free recognition step used by `extract_observe` before
/// the general envelope path materializes an intermediate `Value::String`.
/// Returning `None` keeps all multi-block, error, mixed-media, and JSON behavior
/// on [`unwrap_envelope`].
pub(super) fn single_text(v: &Value) -> Option<&str> {
    if let Some(items) = v.get("items").and_then(Value::as_array) {
        let [item] = items.as_slice() else {
            return None;
        };
        return item.get("Text").and_then(Value::as_str);
    }

    if v.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }

    let content = v.get("content").and_then(Value::as_array)?;
    let [block] = content.as_slice() else {
        return None;
    };
    (block.get("type").and_then(Value::as_str) == Some("text"))
        .then(|| block.get("text").and_then(Value::as_str))
        .flatten()
}

/// Unwrap MCP / ACP tool-result envelopes into a single Value for observe extractors.
///
/// Strategy (deterministic, documented):
///
/// **ACP `items` envelope** `{"items": [{"Json": J}, {"Text": T}, ...]}`:
/// - Non-envelope (no `items` array) → try standard MCP shape, else passthrough.
/// - 0 items → passthrough.
/// - Single `Json` item → the inner Json value.
/// - Single `Text` item → `Value::String(text)`.
/// - Multi-item, all Text → concatenated `Value::String` joined by `\n`.
/// - Multi-item, any Json → FIRST `Json` value with a `__truncated__: true`
///   sentinel merged into its object (if object) so downstream extractors
///   can flag partial data. If first Json is non-object, wrap in
///   `{"value": <original>, "__truncated__": true}`.
/// - Unrecursive: only the outer envelope is unwrapped, never nested.
///
/// **Standard MCP `CallToolResult`** `{"content":[{"type":"text","text":…}], "isError"?}`:
/// - All text blocks → joined `Value::String` (or `{error:true, message}` when `isError`).
/// - Otherwise passthrough so extractors can still match other shapes.
pub fn unwrap_envelope(v: &Value) -> Cow<'_, Value> {
    let Some(items) = v.get("items").and_then(|i| i.as_array()) else {
        return unwrap_mcp_content_array(v);
    };

    if items.is_empty() {
        return Cow::Borrowed(v);
    }

    // Collect text and json blocks
    let mut text_parts: Vec<&str> = Vec::new();
    let mut first_json: Option<&Value> = None;
    let mut has_json = false;

    for item in items {
        if let Some(text) = item.get("Text").and_then(|t| t.as_str()) {
            text_parts.push(text);
        } else if let Some(json_val) = item.get("Json") {
            if first_json.is_none() {
                first_json = Some(json_val);
            }
            has_json = true;
        }
    }

    if items.len() == 1 {
        // Single item cases
        if let Some(json_val) = first_json {
            return Cow::Borrowed(json_val);
        }
        if !text_parts.is_empty() {
            return Cow::Owned(Value::String(text_parts[0].to_string()));
        }
    }

    // Multi-item cases
    if has_json {
        // Any Json present: return first Json + __truncated__: true
        let json_val = first_json.unwrap();
        let result = if let Some(obj) = json_val.as_object() {
            let mut merged = obj.clone();
            merged.insert("__truncated__".to_string(), Value::Bool(true));
            Value::Object(merged)
        } else {
            let mut wrapper = Map::new();
            wrapper.insert("value".to_string(), json_val.clone());
            wrapper.insert("__truncated__".to_string(), Value::Bool(true));
            Value::Object(wrapper)
        };
        return Cow::Owned(result);
    }

    // All Text: concatenate with newline
    if !text_parts.is_empty() {
        return Cow::Owned(Value::String(text_parts.join("\n")));
    }

    // Nothing recognizable — passthrough
    Cow::Borrowed(v)
}

/// Peel standard MCP `CallToolResult.content[]` text blocks into a string Value.
///
/// Without this, generic observe falls through to pretty-printed raw JSON and
/// the TUI shows the envelope scaffolding (`"content": [ { "type": "text" …`)
/// instead of the tool's message — the garbled MCP tool output in session detail.
fn unwrap_mcp_content_array(v: &Value) -> Cow<'_, Value> {
    let Some(content) = v.get("content").and_then(|c| c.as_array()) else {
        return Cow::Borrowed(v);
    };
    if content.is_empty() {
        return Cow::Borrowed(v);
    }

    let mut texts: Vec<&str> = Vec::new();
    for block in content {
        let ty = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "text" {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                texts.push(t);
            }
        } else {
            // Mixed media (image/resource) — leave structured for extractors.
            return Cow::Borrowed(v);
        }
    }
    if texts.is_empty() {
        return Cow::Borrowed(v);
    }

    let joined = texts.join("\n");
    if v.get("isError").and_then(|e| e.as_bool()).unwrap_or(false) {
        let mut err = Map::new();
        err.insert("error".to_string(), Value::Bool(true));
        err.insert("message".to_string(), Value::String(joined));
        return Cow::Owned(Value::Object(err));
    }
    Cow::Owned(Value::String(joined))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_items_passthrough() {
        let v = json!({"items": []});
        let result = unwrap_envelope(&v);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(*result, v);
    }

    #[test]
    fn single_json_returns_inner() {
        let v = json!({"items": [{"Json": {"key": "value"}}]});
        let result = unwrap_envelope(&v);
        assert_eq!(*result, json!({"key": "value"}));
    }

    #[test]
    fn single_text_returns_string() {
        let v = json!({"items": [{"Text": "hello world"}]});
        let result = unwrap_envelope(&v);
        assert_eq!(*result, Value::String("hello world".to_string()));
    }

    #[test]
    fn multi_text_concatenated_with_newline() {
        let v =
            json!({"items": [{"Text": "line one"}, {"Text": "line two"}, {"Text": "line three"}]});
        let result = unwrap_envelope(&v);
        assert_eq!(
            *result,
            Value::String("line one\nline two\nline three".to_string())
        );
    }

    #[test]
    fn text_and_json_mixed_returns_first_json_with_truncated() {
        let v = json!({"items": [{"Text": "some text"}, {"Json": {"result": 42}}]});
        let result = unwrap_envelope(&v);
        assert_eq!(result["result"], json!(42));
        assert_eq!(result["__truncated__"], json!(true));
    }

    #[test]
    fn json_and_json_returns_first_json_with_truncated() {
        let v = json!({"items": [{"Json": {"first": 1}}, {"Json": {"second": 2}}]});
        let result = unwrap_envelope(&v);
        assert_eq!(result["first"], json!(1));
        assert_eq!(result["__truncated__"], json!(true));
        // second Json is not present
        assert!(result.get("second").is_none());
    }

    #[test]
    fn non_envelope_object_passthrough() {
        let v = json!({"key": "value", "num": 42});
        let result = unwrap_envelope(&v);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(*result, v);
    }

    #[test]
    fn non_envelope_array_passthrough() {
        let v = json!([1, 2, 3]);
        let result = unwrap_envelope(&v);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(*result, v);
    }

    #[test]
    fn non_envelope_string_passthrough() {
        let v = Value::String("hi".to_string());
        let result = unwrap_envelope(&v);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(*result, v);
    }

    #[test]
    fn non_envelope_null_passthrough() {
        let v = Value::Null;
        let result = unwrap_envelope(&v);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(*result, v);
    }

    #[test]
    fn nested_envelope_not_recursed() {
        // The Json item is itself an envelope — we should NOT recurse into it
        let inner_envelope = json!({"items": [{"Json": {"deep": true}}]});
        let v = json!({"items": [{"Json": inner_envelope}]});
        let result = unwrap_envelope(&v);
        // Should return the inner_envelope value as-is (not the deep value)
        assert_eq!(*result, json!({"items": [{"Json": {"deep": true}}]}));
    }

    #[test]
    fn non_object_json_wrapped_with_truncated() {
        // First Json is a non-object (e.g. a string) — should be wrapped
        let v = json!({"items": [{"Json": "raw string"}, {"Text": "extra"}]});
        let result = unwrap_envelope(&v);
        assert_eq!(result["value"], json!("raw string"));
        assert_eq!(result["__truncated__"], json!(true));
    }

    #[test]
    fn standard_mcp_content_text_array_unwraps_to_string() {
        let v = json!({
            "content": [
                {"type": "text", "text": "Created issue spur-abc"},
                {"type": "text", "text": "status: open"}
            ]
        });
        let result = unwrap_envelope(&v);
        assert_eq!(
            *result,
            Value::String("Created issue spur-abc\nstatus: open".to_string())
        );
    }

    #[test]
    fn standard_mcp_content_is_error_becomes_error_object() {
        let v = json!({
            "content": [{"type": "text", "text": "permission denied"}],
            "isError": true
        });
        let result = unwrap_envelope(&v);
        assert_eq!(result["error"], json!(true));
        assert_eq!(result["message"], json!("permission denied"));
    }

    #[test]
    fn standard_mcp_content_with_image_passthrough() {
        let v = json!({
            "content": [
                {"type": "text", "text": "caption"},
                {"type": "image", "data": "abc", "mimeType": "image/png"}
            ]
        });
        let result = unwrap_envelope(&v);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(*result, v);
    }

    #[test]
    fn single_text_borrows_acp_item() {
        let v = json!({"items": [{"Text": "hello world"}]});
        let expected = v["items"][0]["Text"].as_str().unwrap();

        let actual = single_text(&v).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(actual.as_ptr(), expected.as_ptr());
    }

    #[test]
    fn single_text_borrows_standard_mcp_content() {
        let v = json!({"content": [{"type": "text", "text": "hello world"}]});
        let expected = v["content"][0]["text"].as_str().unwrap();

        let actual = single_text(&v).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(actual.as_ptr(), expected.as_ptr());
    }

    #[test]
    fn single_text_rejects_shapes_that_need_existing_unwrap_semantics() {
        let cases = [
            json!({
                "content": [{"type": "text", "text": "permission denied"}],
                "isError": true
            }),
            json!({
                "content": [
                    {"type": "text", "text": "one"},
                    {"type": "text", "text": "two"}
                ]
            }),
            json!({
                "content": [
                    {"type": "text", "text": "caption"},
                    {"type": "image", "data": "abc", "mimeType": "image/png"}
                ]
            }),
            json!({
                "items": [],
                "content": [{"type": "text", "text": "items takes precedence"}]
            }),
        ];

        for case in cases {
            assert!(single_text(&case).is_none(), "unexpected fast path: {case}");
        }
    }
}
