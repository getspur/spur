use std::collections::HashMap;

use indexmap::IndexMap;
use openapiv3::{
    OpenAPI, Operation, Parameter, PathItem, ReferenceOr, Response, Schema, SchemaKind, StatusCode,
    Type,
};

use crate::adapter::manifest::{ColumnCfg, FilterCfg, TableCfg};

const MAX_FLATTEN_DEPTH: usize = 4;
const MAX_COLUMNS: usize = 80;
const COLLECTION_KEYS: [&str; 4] = ["data", "items", "results", "records"];
const PAGINATION_PARAMS: [&str; 7] = [
    "page",
    "per_page",
    "limit",
    "offset",
    "cursor",
    "starting_after",
    "ending_before",
];

pub fn parse_spec(text: &str) -> Result<OpenAPI, String> {
    let mut value = parse_to_value(text)?;
    normalize_spec(&mut value);
    serde_json::from_value(value)
        .map_err(|err| format!("failed to deserialize OpenAPI after normalization: {err}"))
}

/// Parse the spec text into a JSON value tree, accepting either JSON or YAML.
fn parse_to_value(text: &str) -> Result<serde_json::Value, String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => Ok(value),
        Err(json_err) => serde_yaml::from_str::<LenientValue>(text)
            .map(|lenient| lenient.0)
            .map_err(|yaml_err| {
                format!("failed to parse OpenAPI as JSON ({json_err}) or YAML ({yaml_err})")
            }),
    }
}

/// A `serde_json::Value` built through a visitor that tolerates the `i128`/`u128`
/// scalars `serde_yaml` produces for integers outside the `i64`/`u64` range
/// (e.g. OpenAI's `±9223372036854776000` schema bounds). Plain
/// `serde_yaml::Value`/`serde_json::Value` reject those before the normalizer
/// can drop them; here they degrade to `f64` instead.
struct LenientValue(serde_json::Value);

impl<'de> serde::Deserialize<'de> for LenientValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_any(LenientVisitor)
            .map(LenientValue)
    }
}

struct LenientVisitor;

impl<'de> serde::de::Visitor<'de> for LenientVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::from(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::from(value))
    }

    fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E> {
        Ok(int_to_value(value))
    }

    fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E> {
        Ok(i128::try_from(value).map_or_else(|_| float_value(value as f64), int_to_value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
        Ok(float_value(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(LenientVisitor)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(LenientValue(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(serde_json::Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entries = serde_json::Map::new();
        while let Some((LenientValue(key), LenientValue(value))) = map.next_entry()? {
            let key = match key {
                serde_json::Value::String(key) => key,
                other => other.to_string(),
            };
            entries.insert(key, value);
        }
        Ok(serde_json::Value::Object(entries))
    }
}

fn int_to_value(value: i128) -> serde_json::Value {
    if let Ok(signed) = i64::try_from(value) {
        serde_json::Value::from(signed)
    } else if let Ok(unsigned) = u64::try_from(value) {
        serde_json::Value::from(unsigned)
    } else {
        float_value(value as f64)
    }
}

fn float_value(value: f64) -> serde_json::Value {
    serde_json::Number::from_f64(value).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// Reshape real-world specs into the subset the `openapiv3` (3.0) model accepts.
///
/// Tier-A provider specs use constructs the 3.0 deserializer rejects: Swagger
/// 2.0 layouts (Mailchimp), OpenAPI 3.1 type arrays and oversized numeric
/// bounds (OpenAI), `deepObject` styles on path parameters (Zendesk), and
/// `null` where a sequence is expected (Square). None of these affect the
/// table columns/filters we extract, so we drop or rewrite them.
fn normalize_spec(value: &mut serde_json::Value) {
    if is_swagger_2(value) {
        convert_swagger_2(value);
    }
    normalize_node(value);
}

fn is_swagger_2(value: &serde_json::Value) -> bool {
    value
        .get("swagger")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|version| version.starts_with("2."))
}

const SCHEMA_BOUND_KEYS: [&str; 5] = [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
];

fn normalize_node(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("style").and_then(serde_json::Value::as_str) == Some("deepObject") {
                map.remove("style");
            }
            map.retain(|_, child| !child.is_null());
            for key in SCHEMA_BOUND_KEYS {
                if matches!(map.get(key), Some(serde_json::Value::Number(n)) if number_out_of_i64_range(n))
                {
                    map.remove(key);
                }
            }
            flatten_type_array(map);
            for child in map.values_mut() {
                normalize_node(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_node(item);
            }
        }
        _ => {}
    }
}

/// True for whole numbers that cannot fit `i64`, which the 3.0 integer schema
/// model stores its bounds as. (When the bound stays, `openapiv3` fails to
/// deserialize the integer schema and silently falls back to an untyped `Any`,
/// so the column degrades to `Utf8`.)
fn number_out_of_i64_range(n: &serde_json::Number) -> bool {
    if n.as_i64().is_some() {
        return false;
    }
    if let Some(unsigned) = n.as_u64() {
        return unsigned > i64::MAX as u64;
    }
    // Neither `i64` nor `u64` represents it. An integral value here is outside
    // `i64` range by definition; a fractional value belongs to an f64-backed
    // number schema and stays. f64 comparisons against the bounds are unsafe —
    // `i64::MIN` and `i64::MIN - 192` collapse to the same f64.
    n.as_f64().is_some_and(|float| float.fract() == 0.0)
}

/// Collapse an OpenAPI 3.1 `type: ["string", "null"]` array into the 3.0 form
/// (`type: "string"`, `nullable: true`).
fn flatten_type_array(map: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(serde_json::Value::Array(members)) = map.get("type") else {
        return;
    };

    let mut nullable = false;
    let mut primary = None;
    for member in members {
        match member.as_str() {
            Some("null") => nullable = true,
            Some(other) if primary.is_none() => primary = Some(other.to_string()),
            _ => {}
        }
    }

    match primary {
        Some(primary) => {
            map.insert("type".to_string(), serde_json::Value::String(primary));
            if nullable {
                map.entry("nullable")
                    .or_insert(serde_json::Value::Bool(true));
            }
        }
        None => {
            map.remove("type");
        }
    }
}

/// Best-effort Swagger 2.0 → OpenAPI 3.0 reshape, covering only what
/// `spec_to_tables` reads: collection `GET` response schemas and query filters.
fn convert_swagger_2(value: &mut serde_json::Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };

    root.remove("swagger");
    root.insert(
        "openapi".to_string(),
        serde_json::Value::String("3.0.0".to_string()),
    );

    // Move shared definitions under `components` so `$ref`s resolve after the
    // ref-prefix rewrite below. `securityDefinitions` is intentionally dropped:
    // its 2.0 shape is incompatible with the 3.0 model and tables never use it.
    let definitions = root.remove("definitions");
    let shared_parameters = root.remove("parameters");
    let shared_responses = root.remove("responses");
    root.remove("securityDefinitions");

    let components = root
        .entry("components")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(components) = components.as_object_mut() {
        if let Some(definitions) = definitions {
            components.insert("schemas".to_string(), definitions);
        }
        if let Some(shared_parameters) = shared_parameters {
            components.insert("parameters".to_string(), shared_parameters);
        }
        if let Some(shared_responses) = shared_responses {
            components.insert("responses".to_string(), shared_responses);
        }
    }

    rewrite_swagger_2_nodes(value);

    let Some(root) = value.as_object_mut() else {
        return;
    };
    if let Some(paths) = root
        .get_mut("paths")
        .and_then(serde_json::Value::as_object_mut)
    {
        for path_item in paths.values_mut() {
            convert_swagger_2_path_item(path_item);
        }
    }
    if let Some(components) = root
        .get_mut("components")
        .and_then(serde_json::Value::as_object_mut)
    {
        if let Some(responses) = components
            .get_mut("responses")
            .and_then(serde_json::Value::as_object_mut)
        {
            for response in responses.values_mut() {
                wrap_swagger_2_response(response);
            }
        }
        if let Some(parameters) = components
            .get_mut("parameters")
            .and_then(serde_json::Value::as_object_mut)
        {
            for parameter in parameters.values_mut() {
                wrap_swagger_2_parameter(parameter);
            }
        }
    }
}

fn rewrite_swagger_2_nodes(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(reference)) = map.get_mut("$ref") {
                for (old, new) in [
                    ("#/definitions/", "#/components/schemas/"),
                    ("#/parameters/", "#/components/parameters/"),
                    ("#/responses/", "#/components/responses/"),
                ] {
                    if let Some(rest) = reference.strip_prefix(old) {
                        *reference = format!("{new}{rest}");
                        break;
                    }
                }
            }
            // 2.0 `discriminator` is the property name string; 3.0 wraps it in
            // an object under `propertyName`.
            if let Some(serde_json::Value::String(property)) = map.get("discriminator") {
                let mut discriminator = serde_json::Map::new();
                discriminator.insert(
                    "propertyName".to_string(),
                    serde_json::Value::String(property.clone()),
                );
                map.insert(
                    "discriminator".to_string(),
                    serde_json::Value::Object(discriminator),
                );
            }
            for child in map.values_mut() {
                rewrite_swagger_2_nodes(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_swagger_2_nodes(item);
            }
        }
        _ => {}
    }
}

const HTTP_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "patch", "options", "head", "trace",
];

const PARAMETER_SCHEMA_KEYS: [&str; 13] = [
    "type",
    "format",
    "items",
    "enum",
    "default",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "maxLength",
    "minLength",
    "pattern",
    "multipleOf",
];

fn convert_swagger_2_path_item(path_item: &mut serde_json::Value) {
    let Some(item) = path_item.as_object_mut() else {
        return;
    };

    convert_swagger_2_parameters(item.get_mut("parameters"));

    for method in HTTP_METHODS {
        let Some(operation) = item
            .get_mut(method)
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        convert_swagger_2_parameters(operation.get_mut("parameters"));
        if let Some(responses) = operation
            .get_mut("responses")
            .and_then(serde_json::Value::as_object_mut)
        {
            for response in responses.values_mut() {
                wrap_swagger_2_response(response);
            }
        }
    }
}

fn convert_swagger_2_parameters(parameters: Option<&mut serde_json::Value>) {
    let Some(serde_json::Value::Array(parameters)) = parameters else {
        return;
    };
    // `body`/`formData` parameters have no 3.0 query/path/header/cookie variant
    // and are never read as table filters, so drop them.
    parameters.retain(|parameter| {
        !matches!(
            parameter.get("in").and_then(serde_json::Value::as_str),
            Some("body") | Some("formData")
        )
    });
    for parameter in parameters.iter_mut() {
        wrap_swagger_2_parameter(parameter);
    }
}

/// 2.0 responses carry the body schema inline; 3.0 nests it under
/// `content."application/json".schema`.
fn wrap_swagger_2_response(response: &mut serde_json::Value) {
    let Some(response) = response.as_object_mut() else {
        return;
    };
    if response.contains_key("$ref") {
        return;
    }
    if let Some(schema) = response.remove("schema") {
        let mut media_type = serde_json::Map::new();
        media_type.insert("schema".to_string(), schema);
        let mut content = serde_json::Map::new();
        content.insert(
            "application/json".to_string(),
            serde_json::Value::Object(media_type),
        );
        response.insert("content".to_string(), serde_json::Value::Object(content));
    }
    response.remove("examples");
}

/// 2.0 non-body parameters put schema keys (`type`, `items`, …) directly on the
/// parameter; 3.0 requires them nested under `schema`.
fn wrap_swagger_2_parameter(parameter: &mut serde_json::Value) {
    let Some(parameter) = parameter.as_object_mut() else {
        return;
    };
    if parameter.contains_key("$ref") {
        return;
    }
    parameter.remove("collectionFormat");

    if parameter.contains_key("schema") {
        for key in PARAMETER_SCHEMA_KEYS {
            parameter.remove(key);
        }
        return;
    }

    let mut schema = serde_json::Map::new();
    for key in PARAMETER_SCHEMA_KEYS {
        if let Some(field) = parameter.remove(key) {
            schema.insert(key.to_string(), field);
        }
    }
    if !schema.is_empty() {
        parameter.insert("schema".to_string(), serde_json::Value::Object(schema));
    }
}

pub fn map_type(schema: &Schema) -> &'static str {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Integer(_)) => "Int64",
        SchemaKind::Type(Type::Number(_)) => "Float64",
        SchemaKind::Type(Type::Boolean(_)) => "Boolean",
        SchemaKind::Type(Type::String(_)) => "Utf8",
        _ => "Utf8",
    }
}

fn resolve<'a>(spec: &'a OpenAPI, item: &'a ReferenceOr<Schema>) -> Option<&'a Schema> {
    match item {
        ReferenceOr::Item(schema) => Some(schema),
        ReferenceOr::Reference { reference } => resolve_schema_ref(spec, reference),
    }
}

fn resolve_boxed<'a>(spec: &'a OpenAPI, item: &'a ReferenceOr<Box<Schema>>) -> Option<&'a Schema> {
    match item {
        ReferenceOr::Item(schema) => Some(schema.as_ref()),
        ReferenceOr::Reference { reference } => resolve_schema_ref(spec, reference),
    }
}

fn resolve_schema_ref<'a>(spec: &'a OpenAPI, reference: &str) -> Option<&'a Schema> {
    let name = reference.strip_prefix("#/components/schemas/")?;
    let schemas = &spec.components.as_ref()?.schemas;
    match schemas.get(name)? {
        ReferenceOr::Item(schema) => Some(schema),
        ReferenceOr::Reference { reference } => {
            let nested = reference.strip_prefix("#/components/schemas/")?;
            match schemas.get(nested)? {
                ReferenceOr::Item(schema) => Some(schema),
                ReferenceOr::Reference { .. } => None,
            }
        }
    }
}

fn flatten(
    spec: &OpenAPI,
    schema: &Schema,
    json_prefix: &str,
    name_prefix: &str,
    depth: usize,
    out: &mut Vec<(String, String, &'static str)>,
) {
    if depth >= MAX_FLATTEN_DEPTH {
        push_leaf(out, name_prefix, json_prefix, "Utf8");
        return;
    }

    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(object)) => {
            if object.properties.is_empty() {
                push_leaf(out, name_prefix, json_prefix, "Utf8");
            } else {
                flatten_properties(
                    spec,
                    &object.properties,
                    json_prefix,
                    name_prefix,
                    depth,
                    out,
                );
            }
        }
        SchemaKind::Type(Type::Array(_)) => push_leaf(out, name_prefix, json_prefix, "Utf8"),
        SchemaKind::Type(_) => push_leaf(out, name_prefix, json_prefix, map_type(schema)),
        SchemaKind::AllOf { all_of } => {
            flatten_all_of(spec, all_of, json_prefix, name_prefix, depth, out)
        }
        SchemaKind::OneOf { one_of } => {
            flatten_union(spec, one_of, json_prefix, name_prefix, depth, out)
        }
        SchemaKind::AnyOf { any_of } => {
            flatten_union(spec, any_of, json_prefix, name_prefix, depth, out)
        }
        SchemaKind::Any(any) if !any.properties.is_empty() => {
            flatten_properties(spec, &any.properties, json_prefix, name_prefix, depth, out);
        }
        _ => push_leaf(out, name_prefix, json_prefix, "Utf8"),
    }
}

fn flatten_all_of(
    spec: &OpenAPI,
    members: &[ReferenceOr<Schema>],
    json_prefix: &str,
    name_prefix: &str,
    depth: usize,
    out: &mut Vec<(String, String, &'static str)>,
) {
    let before = out.len();
    for member in members {
        if let Some(schema) = resolve(spec, member) {
            match &schema.schema_kind {
                SchemaKind::Type(Type::Object(object)) if !object.properties.is_empty() => {
                    flatten_properties(
                        spec,
                        &object.properties,
                        json_prefix,
                        name_prefix,
                        depth,
                        out,
                    );
                }
                SchemaKind::AllOf { all_of } => {
                    flatten_all_of(spec, all_of, json_prefix, name_prefix, depth, out);
                }
                _ => {}
            }
        }
    }

    if out.len() == before {
        push_leaf(out, name_prefix, json_prefix, "Utf8");
    }
}

fn flatten_union(
    spec: &OpenAPI,
    members: &[ReferenceOr<Schema>],
    json_prefix: &str,
    name_prefix: &str,
    depth: usize,
    out: &mut Vec<(String, String, &'static str)>,
) {
    if let Some(schema) = members
        .iter()
        .filter_map(|member| resolve(spec, member))
        .find(|schema| is_object_like(schema))
    {
        flatten(spec, schema, json_prefix, name_prefix, depth, out);
    } else {
        push_leaf(out, name_prefix, json_prefix, "Utf8");
    }
}

fn is_object_like(schema: &Schema) -> bool {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(object)) => !object.properties.is_empty(),
        SchemaKind::AllOf { all_of } => !all_of.is_empty(),
        SchemaKind::Any(any) => !any.properties.is_empty(),
        _ => false,
    }
}

fn flatten_properties(
    spec: &OpenAPI,
    properties: &IndexMap<String, ReferenceOr<Box<Schema>>>,
    json_prefix: &str,
    name_prefix: &str,
    depth: usize,
    out: &mut Vec<(String, String, &'static str)>,
) {
    for (prop, item) in properties {
        let json_path = append_json_path(json_prefix, prop);
        let name = append_name(name_prefix, prop);
        if let Some(schema) = resolve_boxed(spec, item) {
            flatten(spec, schema, &json_path, &name, depth + 1, out);
        } else {
            push_leaf(out, &name, &json_path, "Utf8");
        }
    }
}

fn push_leaf(
    out: &mut Vec<(String, String, &'static str)>,
    name: &str,
    json_path: &str,
    ty: &'static str,
) {
    let base = sanitize_name(name);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while out
        .iter()
        .any(|(existing, _, _)| existing.as_str() == candidate.as_str())
    {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    out.push((candidate, json_path.to_string(), ty));
}

fn detect_collection<'a>(
    spec: &'a OpenAPI,
    response_schema: &'a Schema,
) -> Option<(Option<String>, &'a Schema)> {
    match &response_schema.schema_kind {
        SchemaKind::Type(Type::Array(array)) => array
            .items
            .as_ref()
            .and_then(|items| resolve_boxed(spec, items))
            .map(|item| (None, item)),
        SchemaKind::Type(Type::Object(object)) => {
            detect_object_collection(spec, &object.properties)
        }
        SchemaKind::Any(any) if !any.properties.is_empty() => {
            detect_object_collection(spec, &any.properties)
        }
        _ => None,
    }
}

fn detect_object_collection<'a>(
    spec: &'a OpenAPI,
    properties: &'a IndexMap<String, ReferenceOr<Box<Schema>>>,
) -> Option<(Option<String>, &'a Schema)> {
    for key in COLLECTION_KEYS {
        if let Some(item) = properties
            .get(key)
            .and_then(|schema| resolve_boxed(spec, schema))
            .and_then(|schema| array_item_schema(spec, schema))
        {
            return Some((Some(format!("$.{key}")), item));
        }
    }

    let mut array_properties = properties.iter().filter_map(|(name, schema)| {
        resolve_boxed(spec, schema)
            .and_then(|schema| array_item_schema(spec, schema))
            .map(|item| (name.as_str(), item))
    });
    let first = array_properties.next()?;
    if array_properties.next().is_none() {
        Some((Some(format!("$.{}", first.0)), first.1))
    } else {
        None
    }
}

fn array_item_schema<'a>(spec: &'a OpenAPI, schema: &'a Schema) -> Option<&'a Schema> {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Array(array)) => array
            .items
            .as_ref()
            .and_then(|items| resolve_boxed(spec, items)),
        _ => None,
    }
}

pub fn spec_to_tables(spec: &OpenAPI) -> Vec<TableCfg> {
    let mut tables = Vec::new();
    for (path, path_item) in &spec.paths.paths {
        let Some(path_item) = resolve_path_item(spec, path_item) else {
            continue;
        };
        let Some(operation) = path_item.get.as_ref() else {
            continue;
        };
        let Some(response_schema) = json_response_schema(spec, operation) else {
            continue;
        };
        let Some((response_path, item_schema)) = detect_collection(spec, response_schema) else {
            continue;
        };

        let mut flattened = Vec::new();
        flatten(spec, item_schema, "$", "", 0, &mut flattened);
        flattened.truncate(MAX_COLUMNS);

        let columns = flattened
            .into_iter()
            .map(|(name, json, ty)| {
                (
                    name,
                    ColumnCfg {
                        json,
                        ty: ty.to_string(),
                    },
                )
            })
            .collect();

        tables.push(TableCfg {
            name: table_name(operation, path),
            path: path.clone(),
            response_path,
            columns,
            filters: query_filters(spec, operation),
        });
    }

    tables
}

fn resolve_path_item<'a>(
    spec: &'a OpenAPI,
    path_item: &'a ReferenceOr<PathItem>,
) -> Option<&'a PathItem> {
    match path_item {
        ReferenceOr::Item(path_item) => Some(path_item),
        ReferenceOr::Reference { reference } => {
            let path = decode_path_ref(reference.strip_prefix("#/paths/")?);
            match spec.paths.paths.get(&path)? {
                ReferenceOr::Item(path_item) => Some(path_item),
                ReferenceOr::Reference { .. } => None,
            }
        }
    }
}

fn decode_path_ref(value: &str) -> String {
    value.replace("~1", "/").replace("~0", "~")
}

fn json_response_schema<'a>(spec: &'a OpenAPI, operation: &'a Operation) -> Option<&'a Schema> {
    let response = preferred_response(spec, operation)?;
    response
        .content
        .get("application/json")?
        .schema
        .as_ref()
        .and_then(|schema| resolve(spec, schema))
}

fn preferred_response<'a>(spec: &'a OpenAPI, operation: &'a Operation) -> Option<&'a Response> {
    if let Some(response) = operation.responses.responses.get(&StatusCode::Code(200)) {
        return resolve_response(spec, response);
    }

    operation
        .responses
        .responses
        .iter()
        .find(|(status, _)| is_2xx_status(status))
        .and_then(|(_, response)| resolve_response(spec, response))
}

fn resolve_response<'a>(
    spec: &'a OpenAPI,
    response: &'a ReferenceOr<Response>,
) -> Option<&'a Response> {
    match response {
        ReferenceOr::Item(response) => Some(response),
        ReferenceOr::Reference { reference } => {
            let name = reference.strip_prefix("#/components/responses/")?;
            let responses = &spec.components.as_ref()?.responses;
            match responses.get(name)? {
                ReferenceOr::Item(response) => Some(response),
                ReferenceOr::Reference { reference } => {
                    let nested = reference.strip_prefix("#/components/responses/")?;
                    match responses.get(nested)? {
                        ReferenceOr::Item(response) => Some(response),
                        ReferenceOr::Reference { .. } => None,
                    }
                }
            }
        }
    }
}

fn is_2xx_status(status: &StatusCode) -> bool {
    match status {
        StatusCode::Code(code) => (200..300).contains(code),
        StatusCode::Range(range) => *range == 2,
    }
}

fn query_filters(spec: &OpenAPI, operation: &Operation) -> HashMap<String, FilterCfg> {
    operation
        .parameters
        .iter()
        .filter_map(|parameter| resolve_parameter(spec, parameter))
        .filter_map(|parameter| match parameter {
            Parameter::Query { parameter_data, .. } => Some(parameter_data.name.as_str()),
            _ => None,
        })
        .filter(|name| !is_pagination_param(name))
        .map(|name| {
            (
                sanitize_name(name),
                FilterCfg {
                    param: name.to_string(),
                },
            )
        })
        .collect()
}

fn resolve_parameter<'a>(
    spec: &'a OpenAPI,
    parameter: &'a ReferenceOr<Parameter>,
) -> Option<&'a Parameter> {
    match parameter {
        ReferenceOr::Item(parameter) => Some(parameter),
        ReferenceOr::Reference { reference } => {
            let name = reference.strip_prefix("#/components/parameters/")?;
            let parameters = &spec.components.as_ref()?.parameters;
            match parameters.get(name)? {
                ReferenceOr::Item(parameter) => Some(parameter),
                ReferenceOr::Reference { reference } => {
                    let nested = reference.strip_prefix("#/components/parameters/")?;
                    match parameters.get(nested)? {
                        ReferenceOr::Item(parameter) => Some(parameter),
                        ReferenceOr::Reference { .. } => None,
                    }
                }
            }
        }
    }
}

fn is_pagination_param(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    PAGINATION_PARAMS.contains(&lower.as_str())
}

fn table_name(operation: &Operation, path: &str) -> String {
    operation
        .operation_id
        .as_deref()
        .map(sanitize_name)
        .unwrap_or_else(|| {
            path.rsplit('/')
                .find(|segment| {
                    !segment.is_empty() && !(segment.starts_with('{') && segment.ends_with('}'))
                })
                .map(sanitize_name)
                .unwrap_or_else(|| "table".to_string())
        })
}

pub fn tables_to_toml(tables: &[TableCfg]) -> String {
    let mut out = String::new();
    for table in tables {
        out.push_str("\n[[table]]\n");
        out.push_str(&format!("name = {}\n", toml_str(&table.name)));
        out.push_str(&format!("path = {}\n", toml_str(&table.path)));
        if let Some(response_path) = &table.response_path {
            out.push_str(&format!("response_path = {}\n", toml_str(response_path)));
        }

        out.push_str("\n[table.columns]\n");
        for (name, column) in &table.columns {
            out.push_str(&format!(
                "{} = {{ json = {}, type = {} }}\n",
                name,
                toml_str(&column.json),
                toml_str(&column.ty)
            ));
        }

        if !table.filters.is_empty() {
            out.push_str("\n[table.filters]\n");
            let mut filters = table.filters.iter().collect::<Vec<_>>();
            filters.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (name, filter) in filters {
                out.push_str(&format!(
                    "{} = {{ param = {} }}\n",
                    name,
                    toml_str(&filter.param)
                ));
            }
        }
    }

    out
}

fn append_json_path(prefix: &str, prop: &str) -> String {
    if prefix == "$" {
        format!("$.{prop}")
    } else {
        format!("{prefix}.{prop}")
    }
}

fn append_name(prefix: &str, prop: &str) -> String {
    let prop = sanitize_name(prop);
    if prefix.is_empty() {
        prop
    } else {
        sanitize_name(&format!("{prefix}_{prop}"))
    }
}

fn sanitize_name(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_underscore = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_underscore = false;
        } else if !last_was_underscore {
            out.push('_');
            last_was_underscore = true;
        }
    }

    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "field".to_string()
    } else {
        trimmed
    }
}

fn toml_str(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\u{08}' => escaped.push_str("\\b"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\r' => escaped.push_str("\\r"),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04X}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use openapiv3::Schema;

    use super::*;
    use crate::adapter::manifest::Manifest;

    fn schema_from_yaml(yaml: &str) -> Schema {
        serde_yaml::from_str(yaml).expect("schema should parse")
    }

    const COLLECTION_RESPONSE: &str = r#"{
      "description": "OK",
      "content": { "application/json": { "schema": {
        "type": "object",
        "properties": { "data": { "type": "array", "items": {
          "type": "object", "properties": { "id": { "type": "string" } }
        } } }
      } } }
    }"#;

    #[test]
    fn parse_spec_strips_null_valued_fields() {
        let text = format!(
            r#"{{
              "openapi": "3.0.0",
              "info": {{ "title": "t", "version": "1" }},
              "paths": {{ "/things": {{ "get": {{
                "operationId": "things",
                "parameters": null,
                "responses": {{ "200": {COLLECTION_RESPONSE} }}
              }} }} }}
            }}"#
        );

        let spec = parse_spec(&text).expect("null parameters should be tolerated");
        let tables = spec_to_tables(&spec);

        assert_eq!(tables.len(), 1);
        assert_eq!("things", tables[0].name);
    }

    #[test]
    fn parse_spec_drops_deep_object_path_style() {
        let text = format!(
            r#"{{
              "openapi": "3.0.0",
              "info": {{ "title": "t", "version": "1" }},
              "paths": {{ "/things/{{id}}": {{ "get": {{
                "operationId": "things",
                "parameters": [ {{
                  "name": "id", "in": "path", "required": true,
                  "style": "deepObject", "schema": {{ "type": "string" }}
                }} ],
                "responses": {{ "200": {COLLECTION_RESPONSE} }}
              }} }} }}
            }}"#
        );

        let spec = parse_spec(&text).expect("deepObject path style should be dropped");

        assert_eq!(spec_to_tables(&spec).len(), 1);
    }

    #[test]
    fn parse_spec_drops_oversized_integer_bounds() {
        let text = r#"{
          "openapi": "3.0.0",
          "info": { "title": "t", "version": "1" },
          "paths": { "/things": { "get": {
            "operationId": "things",
            "responses": { "200": { "description": "OK", "content": { "application/json": {
              "schema": { "type": "object", "properties": { "data": { "type": "array", "items": {
                "type": "object", "properties": { "seed": {
                  "type": "integer",
                  "minimum": -9223372036854776000,
                  "maximum": 9223372036854776000
                } }
              } } } }
            } } } }
          } } }
        }"#;

        let spec = parse_spec(text).expect("out-of-i64 bounds should be dropped");
        let tables = spec_to_tables(&spec);

        assert_eq!(tables.len(), 1);
        assert_eq!("Int64", tables[0].columns["seed"].ty);
    }

    #[test]
    fn parse_spec_drops_oversized_integer_bounds_in_yaml() {
        // serde_yaml cannot even build a Value from an out-of-i64 integer, so
        // the normalizer must run before the typed YAML deserialize.
        let text = "
openapi: 3.0.0
info:
  title: t
  version: \"1\"
paths:
  /things:
    get:
      operationId: things
      responses:
        \"200\":
          description: OK
          content:
            application/json:
              schema:
                type: object
                properties:
                  data:
                    type: array
                    items:
                      type: object
                      properties:
                        seed:
                          type: integer
                          minimum: -9223372036854776000
                          maximum: 9223372036854776000
";

        let spec = parse_spec(text).expect("out-of-i64 YAML bounds should be dropped");
        let tables = spec_to_tables(&spec);

        assert_eq!(tables.len(), 1);
        assert_eq!("Int64", tables[0].columns["seed"].ty);
    }

    #[test]
    fn parse_spec_converts_swagger_2_string_discriminator() {
        let text = r##"{
          "swagger": "2.0",
          "info": { "title": "t", "version": "1" },
          "paths": { "/pets": { "get": {
            "operationId": "pets",
            "responses": { "200": {
              "description": "OK",
              "schema": { "type": "object", "properties": {
                "data": { "type": "array", "items": { "$ref": "#/definitions/Pet" } }
              } }
            } }
          } } },
          "definitions": {
            "Pet": {
              "type": "object",
              "discriminator": "petType",
              "required": ["petType"],
              "properties": { "petType": { "type": "string" } }
            }
          }
        }"##;

        let spec = parse_spec(text).expect("swagger 2.0 string discriminator should convert");

        assert_eq!(spec_to_tables(&spec).len(), 1);
    }

    #[test]
    fn parse_spec_flattens_openapi_31_type_arrays() {
        let text = r#"{
          "openapi": "3.1.0",
          "info": { "title": "t", "version": "1" },
          "paths": { "/things": { "get": {
            "operationId": "things",
            "responses": { "200": { "description": "OK", "content": { "application/json": {
              "schema": { "type": "object", "properties": { "data": { "type": "array", "items": {
                "type": "object", "properties": { "id": { "type": ["string", "null"] } }
              } } } }
            } } } }
          } } }
        }"#;

        let spec = parse_spec(text).expect("3.1 type arrays should be flattened");
        let tables = spec_to_tables(&spec);

        assert_eq!(tables.len(), 1);
        assert_eq!("Utf8", tables[0].columns["id"].ty);
    }

    #[test]
    fn parse_spec_converts_swagger_2() {
        let text = r##"{
          "swagger": "2.0",
          "info": { "title": "t", "version": "1" },
          "paths": { "/widgets": { "get": {
            "operationId": "widgets",
            "parameters": [ { "name": "category", "in": "query", "type": "string" } ],
            "responses": { "200": {
              "description": "OK",
              "schema": { "$ref": "#/definitions/WidgetList" }
            } }
          } } },
          "definitions": {
            "WidgetList": { "type": "object", "properties": {
              "data": { "type": "array", "items": { "$ref": "#/definitions/Widget" } }
            } },
            "Widget": { "type": "object", "properties": {
              "id": { "type": "string" }, "size": { "type": "integer" }
            } }
          }
        }"##;

        let spec = parse_spec(text).expect("swagger 2.0 should convert to OpenAPI 3.0");
        let tables = spec_to_tables(&spec);

        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!("widgets", table.name);
        assert_eq!(Some("$.data".to_string()), table.response_path);
        assert_eq!("Utf8", table.columns["id"].ty);
        assert_eq!("Int64", table.columns["size"].ty);
        assert_eq!("category", table.filters["category"].param);
    }

    fn stripe_charges_spec() -> OpenAPI {
        parse_spec(
            r#"
openapi: 3.0.3
info:
  title: Stripe
  version: "1"
paths:
  /v1/charges:
    get:
      operationId: charges
      parameters:
        - in: query
          name: customer
          schema:
            type: string
        - in: query
          name: limit
          schema:
            type: integer
      responses:
        "200":
          description: OK
          content:
            application/json:
              schema:
                type: object
                properties:
                  data:
                    type: array
                    items:
                      type: object
                      properties:
                        id:
                          type: string
                        amount:
                          type: integer
                        paid:
                          type: boolean
                        billing_details:
                          type: object
                          properties:
                            address:
                              type: object
                              properties:
                                city:
                                  type: string
                        metadata:
                          type: object
                          additionalProperties: true
                        refunds:
                          type: array
                          items:
                            type: object
                            properties:
                              id:
                                type: string
"#,
        )
        .expect("stripe spec should parse")
    }

    #[test]
    fn map_type_covers_scalars() {
        assert_eq!("Int64", map_type(&schema_from_yaml("type: integer")));
        assert_eq!("Float64", map_type(&schema_from_yaml("type: number")));
        assert_eq!("Boolean", map_type(&schema_from_yaml("type: boolean")));
        assert_eq!("Utf8", map_type(&schema_from_yaml("type: string")));
        assert_eq!(
            "Utf8",
            map_type(&schema_from_yaml(
                r#"
type: object
properties:
  id:
    type: string
"#,
            ))
        );
    }

    #[test]
    fn flatten_nested_dotted() {
        let spec = OpenAPI::default();
        let schema = schema_from_yaml(
            r#"
type: object
properties:
  billing:
    type: object
    properties:
      address:
        type: object
        properties:
          city:
            type: string
"#,
        );
        let mut out = Vec::new();

        flatten(&spec, &schema, "$", "", 0, &mut out);

        assert_eq!(
            out,
            vec![(
                "billing_address_city".to_string(),
                "$.billing.address.city".to_string(),
                "Utf8"
            )]
        );
    }

    #[test]
    fn flatten_array_and_freeform_to_utf8() {
        let spec = OpenAPI::default();
        let schema = schema_from_yaml(
            r#"
type: object
properties:
  tags:
    type: array
    items:
      type: string
  metadata:
    type: object
    additionalProperties: true
"#,
        );
        let mut out = Vec::new();

        flatten(&spec, &schema, "$", "", 0, &mut out);

        assert_eq!(
            out,
            vec![
                ("tags".to_string(), "$.tags".to_string(), "Utf8"),
                ("metadata".to_string(), "$.metadata".to_string(), "Utf8"),
            ]
        );
    }

    #[test]
    fn detect_collection_envelope() {
        let spec = OpenAPI::default();
        let schema = schema_from_yaml(
            r#"
type: object
properties:
  data:
    type: array
    items:
      type: object
      properties:
        id:
          type: string
"#,
        );

        let (response_path, item_schema) =
            detect_collection(&spec, &schema).expect("data envelope should be detected");

        assert_eq!(Some("$.data".to_string()), response_path);
        let mut out = Vec::new();
        flatten(&spec, item_schema, "$", "", 0, &mut out);
        assert_eq!(out[0], ("id".to_string(), "$.id".to_string(), "Utf8"));
    }

    #[test]
    fn detect_collection_plain_object_skips() {
        let spec = OpenAPI::default();
        let schema = schema_from_yaml(
            r#"
type: object
properties:
  id:
    type: string
"#,
        );

        assert!(detect_collection(&spec, &schema).is_none());
    }

    #[test]
    fn stripe_charges_example() {
        let spec = stripe_charges_spec();

        let tables = spec_to_tables(&spec);

        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!("charges", table.name);
        assert_eq!("/v1/charges", table.path);
        assert_eq!(Some("$.data".to_string()), table.response_path);
        assert_eq!("Utf8", table.columns["id"].ty);
        assert_eq!("Int64", table.columns["amount"].ty);
        assert_eq!("Boolean", table.columns["paid"].ty);
        assert_eq!("Utf8", table.columns["billing_details_address_city"].ty);
        assert_eq!(
            "$.billing_details.address.city",
            table.columns["billing_details_address_city"].json
        );
        assert_eq!("Utf8", table.columns["metadata"].ty);
        assert_eq!("Utf8", table.columns["refunds"].ty);
        assert_eq!(1, table.filters.len());
        assert_eq!("customer", table.filters["customer"].param);
        assert!(!table.filters.contains_key("limit"));
    }

    #[test]
    fn tables_to_toml_roundtrips() {
        let spec = stripe_charges_spec();
        let tables = spec_to_tables(&spec);
        let toml = format!(
            "[source]\nname = \"x\"\nbase_url = \"http://x\"\n{}",
            tables_to_toml(&tables)
        );

        let reparsed = Manifest::from_toml(&toml).expect("generated TOML should parse");

        assert_eq!(1, reparsed.tables.len());
        let table = &reparsed.tables[0];
        assert_eq!(Some("$.data".to_string()), table.response_path);
        assert!(table.columns.contains_key("id"));
        assert!(table.columns.contains_key("billing_details_address_city"));
        assert_eq!("customer", table.filters["customer"].param);
    }
}
