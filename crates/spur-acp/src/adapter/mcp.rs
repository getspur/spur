use serde_json::{Map, Value};
use std::borrow::Cow;

/// Unwrap the standard MCP content-block envelope
/// `{"items": [{"Json": J}, {"Text": T}, ...]}` into a single Value.
///
/// Strategy (deterministic, documented):
/// - Non-envelope (no `items` array) → passthrough.
/// - 0 items → passthrough.
/// - Single `Json` item → the inner Json value.
/// - Single `Text` item → `Value::String(text)`.
/// - Multi-item, all Text → concatenated `Value::String` joined by `\n`.
/// - Multi-item, any Json → FIRST `Json` value with a `__truncated__: true`
///   sentinel merged into its object (if object) so downstream extractors
///   can flag partial data. If first Json is non-object, wrap in
///   `{"value": <original>, "__truncated__": true}`.
/// - Unrecursive: only the outer envelope is unwrapped, never nested.
pub fn unwrap_envelope(v: &Value) -> Cow<'_, Value> {
    let Some(items) = v.get("items").and_then(|i| i.as_array()) else {
        return Cow::Borrowed(v);
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
}
