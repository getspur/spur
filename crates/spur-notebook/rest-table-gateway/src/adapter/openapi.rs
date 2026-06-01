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
    match serde_json::from_str(text) {
        Ok(spec) => Ok(spec),
        Err(json_err) => serde_yaml::from_str(text).map_err(|yaml_err| {
            format!("failed to parse OpenAPI as JSON ({json_err}) or YAML ({yaml_err})")
        }),
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
