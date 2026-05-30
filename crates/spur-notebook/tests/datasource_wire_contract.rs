use jute::commands::{
    Column as JuteColumn, DatasourceEntry as JuteDatasourceEntry,
    DatasourceKind as JuteDatasourceKind, Table as JuteTable,
};
use serde_json::Value;
use spur_acp::{
    Column as AcpColumn, DatasourceEntry as AcpDatasourceEntry,
    DatasourceKind as AcpDatasourceKind, Table as AcpTable,
};

#[test]
fn datasource_entries_are_wire_compatible_from_jute_to_acp() {
    for (index, kind) in jute_kinds().into_iter().enumerate() {
        let entry = populated_jute_entry(kind, index);
        let json = serde_json::to_string(&entry).expect("jute datasource serializes");
        let decoded: AcpDatasourceEntry =
            serde_json::from_str(&json).expect("acp datasource decodes jute JSON");

        assert_acp_matches_jute(&decoded, &entry);

        let none_entry = jute_entry_with_optional_fields(kind, index);
        let none_json =
            serde_json::to_string(&none_entry).expect("jute datasource with None serializes");
        assert_option_fields_are_serialized_as_null(&none_json);
        let none_decoded: AcpDatasourceEntry =
            serde_json::from_str(&none_json).expect("acp datasource decodes jute None JSON");

        assert_acp_matches_jute(&none_decoded, &none_entry);
    }
}

#[test]
fn datasource_entries_are_wire_compatible_from_acp_to_jute() {
    for (index, kind) in acp_kinds().into_iter().enumerate() {
        let entry = populated_acp_entry(kind, index);
        let json = serde_json::to_string(&entry).expect("acp datasource serializes");
        let decoded: JuteDatasourceEntry =
            serde_json::from_str(&json).expect("jute datasource decodes acp JSON");

        assert_jute_matches_acp(&decoded, &entry);

        let none_entry = acp_entry_with_optional_fields(kind, index);
        let none_json =
            serde_json::to_string(&none_entry).expect("acp datasource with None serializes");
        assert_option_fields_are_serialized_as_null(&none_json);
        let none_decoded: JuteDatasourceEntry =
            serde_json::from_str(&none_json).expect("jute datasource decodes acp None JSON");

        assert_jute_matches_acp(&none_decoded, &none_entry);
    }
}

fn jute_kinds() -> [JuteDatasourceKind; 4] {
    [
        JuteDatasourceKind::Csv,
        JuteDatasourceKind::Parquet,
        JuteDatasourceKind::Json,
        JuteDatasourceKind::DuckDb,
    ]
}

fn acp_kinds() -> [AcpDatasourceKind; 4] {
    [
        AcpDatasourceKind::Csv,
        AcpDatasourceKind::Parquet,
        AcpDatasourceKind::Json,
        AcpDatasourceKind::DuckDb,
    ]
}

fn populated_jute_entry(kind: JuteDatasourceKind, index: usize) -> JuteDatasourceEntry {
    JuteDatasourceEntry {
        name: format!("jute_source_{index}"),
        path: format!("/tmp/spur/jute_source_{index}.data"),
        kind,
        group: Some(format!("group_{index}")),
        columns: vec![JuteColumn {
            name: format!("amount_{index}"),
            sql_type: "DECIMAL(18, 4) NOT NULL".to_string(),
        }],
        row_count: Some(10_000 + index as u64),
        tables: vec![JuteTable {
            name: format!("table_{index}"),
            columns: vec![JuteColumn {
                name: format!("table_amount_{index}"),
                sql_type: "BIGINT".to_string(),
            }],
            row_count: Some(30_000 + index as u64),
        }],
    }
}

fn populated_acp_entry(kind: AcpDatasourceKind, index: usize) -> AcpDatasourceEntry {
    AcpDatasourceEntry {
        name: format!("acp_source_{index}"),
        path: format!("/tmp/spur/acp_source_{index}.data"),
        kind,
        group: Some(format!("group_{index}")),
        columns: vec![AcpColumn {
            name: format!("amount_{index}"),
            sql_type: "DECIMAL(18, 4) NOT NULL".to_string(),
        }],
        row_count: Some(20_000 + index as u64),
        tables: vec![AcpTable {
            name: format!("table_{index}"),
            columns: vec![AcpColumn {
                name: format!("table_amount_{index}"),
                sql_type: "BIGINT".to_string(),
            }],
            row_count: Some(40_000 + index as u64),
        }],
    }
}

fn jute_entry_with_optional_fields(kind: JuteDatasourceKind, index: usize) -> JuteDatasourceEntry {
    JuteDatasourceEntry {
        group: None,
        row_count: None,
        tables: Vec::new(),
        ..populated_jute_entry(kind, index)
    }
}

fn acp_entry_with_optional_fields(kind: AcpDatasourceKind, index: usize) -> AcpDatasourceEntry {
    AcpDatasourceEntry {
        group: None,
        row_count: None,
        tables: Vec::new(),
        ..populated_acp_entry(kind, index)
    }
}

fn assert_acp_matches_jute(acp: &AcpDatasourceEntry, jute: &JuteDatasourceEntry) {
    assert_eq!(acp.name, jute.name);
    assert_eq!(acp.path, jute.path);
    assert_eq!(acp.kind, acp_kind_for_jute(jute.kind));
    assert_eq!(acp.group, jute.group);
    assert_eq!(acp.columns.len(), jute.columns.len());
    for (acp_column, jute_column) in acp.columns.iter().zip(&jute.columns) {
        assert_eq!(acp_column.name, jute_column.name);
        assert_eq!(acp_column.sql_type, jute_column.sql_type);
    }
    assert_eq!(acp.row_count, jute.row_count);
    assert_eq!(acp.tables.len(), jute.tables.len());
    for (acp_table, jute_table) in acp.tables.iter().zip(&jute.tables) {
        assert_eq!(acp_table.name, jute_table.name);
        assert_eq!(acp_table.row_count, jute_table.row_count);
        assert_eq!(acp_table.columns.len(), jute_table.columns.len());
        for (acp_column, jute_column) in acp_table.columns.iter().zip(&jute_table.columns) {
            assert_eq!(acp_column.name, jute_column.name);
            assert_eq!(acp_column.sql_type, jute_column.sql_type);
        }
    }
}

fn assert_jute_matches_acp(jute: &JuteDatasourceEntry, acp: &AcpDatasourceEntry) {
    assert_eq!(jute.name, acp.name);
    assert_eq!(jute.path, acp.path);
    assert_eq!(jute.kind, jute_kind_for_acp(acp.kind));
    assert_eq!(jute.group, acp.group);
    assert_eq!(jute.columns.len(), acp.columns.len());
    for (jute_column, acp_column) in jute.columns.iter().zip(&acp.columns) {
        assert_eq!(jute_column.name, acp_column.name);
        assert_eq!(jute_column.sql_type, acp_column.sql_type);
    }
    assert_eq!(jute.row_count, acp.row_count);
    assert_eq!(jute.tables.len(), acp.tables.len());
    for (jute_table, acp_table) in jute.tables.iter().zip(&acp.tables) {
        assert_eq!(jute_table.name, acp_table.name);
        assert_eq!(jute_table.row_count, acp_table.row_count);
        assert_eq!(jute_table.columns.len(), acp_table.columns.len());
        for (jute_column, acp_column) in jute_table.columns.iter().zip(&acp_table.columns) {
            assert_eq!(jute_column.name, acp_column.name);
            assert_eq!(jute_column.sql_type, acp_column.sql_type);
        }
    }
}

fn acp_kind_for_jute(kind: JuteDatasourceKind) -> AcpDatasourceKind {
    match kind {
        JuteDatasourceKind::Csv => AcpDatasourceKind::Csv,
        JuteDatasourceKind::Parquet => AcpDatasourceKind::Parquet,
        JuteDatasourceKind::Json => AcpDatasourceKind::Json,
        JuteDatasourceKind::DuckDb => AcpDatasourceKind::DuckDb,
    }
}

fn jute_kind_for_acp(kind: AcpDatasourceKind) -> JuteDatasourceKind {
    match kind {
        AcpDatasourceKind::Csv => JuteDatasourceKind::Csv,
        AcpDatasourceKind::Parquet => JuteDatasourceKind::Parquet,
        AcpDatasourceKind::Json => JuteDatasourceKind::Json,
        AcpDatasourceKind::DuckDb => JuteDatasourceKind::DuckDb,
    }
}

fn assert_option_fields_are_serialized_as_null(json: &str) {
    let value: Value = serde_json::from_str(json).expect("datasource JSON parses as value");

    assert_eq!(value.get("group"), Some(&Value::Null));
    assert_eq!(value.get("rowCount"), Some(&Value::Null));
}
