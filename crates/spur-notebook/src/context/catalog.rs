use std::{
    collections::BTreeMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use jute::{
    backend::notebook::{Cell, CellDagMetadata, MultilineString, NotebookRoot},
    commands::{Column, DatasourceEntry, DatasourceKind, Table},
};
use serde::Serialize;

use crate::context::refs::Ref;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogNode {
    pub r#ref: String,
    pub node_type: &'static str,
    pub name: String,
    pub kind: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CatalogNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoke: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<ColumnSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<UsedBy>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ColumnSchema {
    pub name: String,
    pub sql_type: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UsedBy {
    pub cell: String,
    pub via: &'static str,
}

pub fn datasource_id(entry: &DatasourceEntry, all: &[DatasourceEntry]) -> String {
    let slug = slugify(&entry.name);
    let collides = all
        .iter()
        .filter(|candidate| slugify(&candidate.name) == slug)
        .take(2)
        .count()
        > 1;
    if !collides {
        return slug;
    }

    let mut hasher = DefaultHasher::new();
    entry.path.hash(&mut hasher);
    kind_name(entry.kind).hash(&mut hasher);
    format!("{slug}-{:06x}", hasher.finish() & 0x00ff_ffff)
}

pub fn catalog_layer1(entries: &[DatasourceEntry]) -> Vec<CatalogNode> {
    entries
        .iter()
        .map(|entry| entry_node(entry, entries, false))
        .collect()
}

pub fn descend(entries: &[DatasourceEntry], target: &Ref) -> Option<CatalogNode> {
    let Ref::Datasource { id, table } = target else {
        return None;
    };
    let entry = entries
        .iter()
        .find(|entry| datasource_id(entry, entries) == *id)?;

    match table {
        Some(table) => table_node(entry, entries, table, true),
        None => Some(entry_node(entry, entries, true)),
    }
}

pub fn used_by_map(
    root: &NotebookRoot,
    entries: &[DatasourceEntry],
) -> BTreeMap<String, Vec<UsedBy>> {
    let mut used = BTreeMap::<String, Vec<UsedBy>>::new();
    let table_invokes = table_invokes(entries);

    for cell in &root.cells {
        let Some(cell_ref) = cell_ref(cell) else {
            continue;
        };

        if let Some(source) = cell_dag(cell).and_then(|dag| dag.source.as_ref()) {
            for entry in entries {
                if source.kind == kind_name(entry.kind) && source.port == entry.name {
                    push_used(
                        &mut used,
                        datasource_id(entry, entries),
                        UsedBy {
                            cell: cell_ref.clone(),
                            via: "dag.source",
                        },
                    );
                }
            }
        }

        let Some(source_text) = cell_source(cell) else {
            continue;
        };
        for (entry_id, needle) in &table_invokes {
            if source_text.contains(needle) {
                push_used(
                    &mut used,
                    entry_id.clone(),
                    UsedBy {
                        cell: cell_ref.clone(),
                        via: "table_function",
                    },
                );
            }
        }
    }

    used
}

fn entry_node(
    entry: &DatasourceEntry,
    all: &[DatasourceEntry],
    include_children: bool,
) -> CatalogNode {
    let id = datasource_id(entry, all);
    let is_connection = is_connection_kind(entry.kind);
    let children = if is_connection && include_children {
        entry
            .tables
            .iter()
            .map(|table| table_node_from_table(entry, &id, table, false))
            .collect()
    } else if is_connection {
        entry
            .tables
            .iter()
            .map(|table| table_node_from_table(entry, &id, table, false))
            .collect()
    } else {
        Vec::new()
    };

    CatalogNode {
        r#ref: Ref::Datasource { id, table: None }.to_string(),
        node_type: if is_connection { "connection" } else { "table" },
        name: entry.name.clone(),
        kind: kind_name(entry.kind).to_string(),
        status: status(entry.kind),
        children,
        invoke: (!is_connection).then(|| invoke_name(entry, &entry.name)),
        columns: (!is_connection).then(|| columns(&entry.columns)),
        row_count: (!is_connection).then_some(entry.row_count).flatten(),
        used_by: Vec::new(),
    }
}

fn table_node(
    entry: &DatasourceEntry,
    all: &[DatasourceEntry],
    table_name: &str,
    full_schema: bool,
) -> Option<CatalogNode> {
    let id = datasource_id(entry, all);
    if is_connection_kind(entry.kind) {
        let table = entry.tables.iter().find(|table| table.name == table_name)?;
        return Some(table_node_from_table(entry, &id, table, full_schema));
    }
    (table_name == entry.name).then(|| {
        let mut node = entry_node(entry, all, false);
        if !full_schema {
            node.columns = None;
        }
        node
    })
}

fn table_node_from_table(
    entry: &DatasourceEntry,
    id: &str,
    table: &Table,
    full_schema: bool,
) -> CatalogNode {
    CatalogNode {
        r#ref: Ref::Datasource {
            id: id.to_string(),
            table: Some(table.name.clone()),
        }
        .to_string(),
        node_type: "table",
        name: table.name.clone(),
        kind: kind_name(entry.kind).to_string(),
        status: status(entry.kind),
        children: Vec::new(),
        invoke: Some(invoke_name(entry, &table.name)),
        columns: full_schema.then(|| columns(&table.columns)),
        row_count: table.row_count,
        used_by: Vec::new(),
    }
}

fn table_invokes(entries: &[DatasourceEntry]) -> Vec<(String, String)> {
    entries
        .iter()
        .flat_map(|entry| {
            let id = datasource_id(entry, entries);
            if is_connection_kind(entry.kind) {
                entry
                    .tables
                    .iter()
                    .map(|table| (id.clone(), format!("{}(", invoke_stem(entry, &table.name))))
                    .collect::<Vec<_>>()
            } else {
                vec![(id, format!("{}(", invoke_stem(entry, &entry.name)))]
            }
        })
        .collect()
}

fn push_used(used: &mut BTreeMap<String, Vec<UsedBy>>, id: String, evidence: UsedBy) {
    let entries = used.entry(id).or_default();
    if !entries.contains(&evidence) {
        entries.push(evidence);
    }
}

fn columns(columns: &[Column]) -> Vec<ColumnSchema> {
    columns
        .iter()
        .map(|column| ColumnSchema {
            name: column.name.clone(),
            sql_type: column.sql_type.clone(),
        })
        .collect()
}

fn cell_ref(cell: &Cell) -> Option<String> {
    let id = cell_id(cell)?;
    let version = cell_version(cell)?;
    Some(
        Ref::Cell {
            id,
            version: Some(version),
        }
        .to_string(),
    )
}

fn cell_id(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => cell.id.clone(),
        Cell::Markdown(cell) => cell.id.clone(),
        Cell::Code(cell) => cell.id.clone(),
    }
}

fn cell_version(cell: &Cell) -> Option<u64> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Code(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
    }
}

fn cell_dag(cell: &Cell) -> Option<&CellDagMetadata> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
    }
}

fn cell_source(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Code(cell) => Some(multiline_to_string(&cell.source)),
        Cell::Raw(cell) => Some(multiline_to_string(&cell.source)),
        Cell::Markdown(cell) => Some(multiline_to_string(&cell.source)),
    }
}

fn multiline_to_string(value: &MultilineString) -> String {
    match value {
        MultilineString::Single(value) => value.clone(),
        MultilineString::Multi(lines) => lines.join(""),
    }
}

fn invoke_name(entry: &DatasourceEntry, table: &str) -> String {
    format!("{}()", invoke_stem(entry, table))
}

fn invoke_stem(entry: &DatasourceEntry, table: &str) -> String {
    if entry.kind == DatasourceKind::ApiTables {
        format!(
            "{}_{}",
            slug_identifier(&entry.name),
            slug_identifier(table)
        )
    } else {
        slug_identifier(table)
    }
}

fn is_connection_kind(kind: DatasourceKind) -> bool {
    matches!(
        kind,
        DatasourceKind::ApiTables | DatasourceKind::DuckDb | DatasourceKind::Sqlite
    )
}

fn status(kind: DatasourceKind) -> &'static str {
    if kind == DatasourceKind::ApiTables {
        "connected"
    } else {
        "static"
    }
}

fn kind_name(kind: DatasourceKind) -> &'static str {
    match kind {
        DatasourceKind::Csv => "csv",
        DatasourceKind::Parquet => "parquet",
        DatasourceKind::Json => "json",
        DatasourceKind::DuckDb => "duck_db",
        DatasourceKind::Sqlite => "sqlite",
        DatasourceKind::ApiTables => "api_tables",
    }
}

fn slugify(value: &str) -> String {
    let slug = value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "datasource".to_string()
    } else {
        slug
    }
}

fn slug_identifier(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_underscore = false;
    for ch in value.to_ascii_lowercase().chars() {
        let next = if ch.is_ascii_alphanumeric() { ch } else { '_' };
        if next == '_' {
            if !last_was_underscore && !out.is_empty() {
                out.push(next);
            }
            last_was_underscore = true;
        } else {
            out.push(next);
            last_was_underscore = false;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "table".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
            NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        commands::{Column, DatasourceEntry, DatasourceKind, Table},
    };
    use serde_json::Value;

    use super::*;

    fn csv_entry(name: &str, path: &str) -> DatasourceEntry {
        DatasourceEntry {
            name: name.to_string(),
            path: path.to_string(),
            kind: DatasourceKind::Csv,
            group: None,
            columns: vec![Column {
                name: "amount".to_string(),
                sql_type: "DOUBLE".to_string(),
            }],
            row_count: Some(12),
            tables: Vec::new(),
        }
    }

    fn api_entry(name: &str) -> DatasourceEntry {
        DatasourceEntry {
            name: name.to_string(),
            path: format!("api://{name}"),
            kind: DatasourceKind::ApiTables,
            group: None,
            columns: Vec::new(),
            row_count: None,
            tables: vec![Table {
                name: "markets".to_string(),
                columns: vec![Column {
                    name: "market_id".to_string(),
                    sql_type: "TEXT".to_string(),
                }],
                row_count: Some(3200),
            }],
        }
    }

    fn notebook(cells: Vec<Cell>) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells,
        }
    }

    fn cell(id: &str, source_text: &str, source: Option<DagSource>) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version: 7,
                    last_edited_by: Some("brain".to_string()),
                    datasource_setup: None,
                    dag: Some(CellDagMetadata {
                        produces: vec![PortSpec {
                            port: "sales".to_string(),
                            repr: "arrow".to_string(),
                            display: None,
                            class: None,
                            schema: None,
                        }],
                        consumes: Vec::new(),
                        source,
                    }),
                    code_type: None,
                    frontend: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single(source_text.to_string()),
            execution_count: Some(3),
            outputs: Vec::new(),
        })
    }

    #[test]
    fn csv_is_a_leaf_and_api_tables_is_a_connection() {
        let entries = vec![csv_entry("sales", "/d/sales.csv"), api_entry("polymarket")];

        let nodes = catalog_layer1(&entries);

        assert_eq!(nodes[0].node_type, "table");
        assert_eq!(nodes[0].status, "static");
        assert_eq!(nodes[1].node_type, "connection");
        assert_eq!(nodes[1].status, "connected");
        assert!(!nodes[1].children.is_empty());
    }

    #[test]
    fn colliding_names_get_hash_suffix() {
        let entries = vec![
            csv_entry("sales", "/a/sales.csv"),
            csv_entry("sales", "/b/sales.csv"),
        ];

        let ids: Vec<_> = entries
            .iter()
            .map(|entry| datasource_id(entry, &entries))
            .collect();

        assert_ne!(ids[0], ids[1]);
        assert!(ids[0].starts_with("sales-"));
        assert_eq!(ids[0].len(), "sales-".len() + 6);
    }

    #[test]
    fn descend_returns_table_leaf_with_full_schema_and_invoke() {
        let entries = vec![api_entry("polymarket")];
        let ds_ref = crate::context::refs::Ref::parse("ds://polymarket/markets").unwrap();

        let node = descend(&entries, &ds_ref).expect("table node");

        assert_eq!(node.node_type, "table");
        assert_eq!(node.invoke.as_deref(), Some("polymarket_markets()"));
        assert_eq!(node.columns.as_ref().unwrap()[0].name, "market_id");
    }

    #[test]
    fn scope_used_matches_dag_source_and_invoke_literal() {
        let root = notebook(vec![
            cell(
                "source",
                "sales = read_csv()",
                Some(DagSource {
                    kind: "csv".to_string(),
                    port: "sales".to_string(),
                    class: None,
                    schema: None,
                }),
            ),
            cell("api", "df = polymarket_markets()", None),
        ]);
        let entries = vec![csv_entry("sales", "/d/sales.csv"), api_entry("polymarket")];

        let used = used_by_map(&root, &entries);

        assert!(used.contains_key("sales"));
        assert!(used
            .values()
            .flatten()
            .any(|used_by| used_by.cell == "cell://source@v7" && used_by.via == "dag.source"));
        assert!(used
            .values()
            .flatten()
            .any(|used_by| { used_by.cell == "cell://api@v7" && used_by.via == "table_function" }));
    }

    #[test]
    fn serialized_nodes_do_not_expose_secret_keys() {
        let entries = vec![csv_entry("sales", "/d/sales.csv"), api_entry("polymarket")];
        let mut nodes = catalog_layer1(&entries);
        let ds_ref = crate::context::refs::Ref::parse("ds://polymarket/markets").unwrap();
        nodes.push(descend(&entries, &ds_ref).expect("descended table"));

        let value = serde_json::to_value(&nodes).expect("serialize nodes");
        let mut keys = Vec::new();
        collect_keys(&value, &mut keys);

        let forbidden = ["token", "authorization", "secret", "password"];
        assert!(
            keys.iter()
                .all(|key| forbidden.iter().all(|needle| !key.contains(needle))),
            "forbidden key present in {keys:?}"
        );
    }

    fn collect_keys(value: &Value, keys: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    keys.push(key.to_ascii_lowercase());
                    collect_keys(child, keys);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_keys(item, keys);
                }
            }
            _ => {}
        }
    }
}
