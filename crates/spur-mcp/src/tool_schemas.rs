use schemars::{schema_for, JsonSchema};
use serde_json::Value;

pub fn schema_value<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    let mut value = serde_json::to_value(&schema).unwrap();
    normalize_defs_refs(&mut value);
    value
}

pub fn schema_object<T: JsonSchema>() -> serde_json::Map<String, Value> {
    match schema_value::<T>() {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn normalize_defs_refs(value: &mut Value) {
    if let Value::Object(map) = value {
        if let Some(definitions) = map.remove("definitions") {
            map.insert("$defs".to_string(), definitions);
        }
    }

    rewrite_definitions_refs(value);
}

fn rewrite_definitions_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get_mut("$ref") {
                if let Some(suffix) = reference.strip_prefix("#/definitions/") {
                    *reference = format!("#/$defs/{suffix}");
                }
            }

            for nested in map.values_mut() {
                rewrite_definitions_refs(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                rewrite_definitions_refs(nested);
            }
        }
        _ => {}
    }
}
