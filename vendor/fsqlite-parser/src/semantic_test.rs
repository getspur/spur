use super::*;
use crate::parser::Parser;

fn make_schema() -> Schema {
    let mut schema = Schema::new();
    schema.add_table(TableDef {
        name: "users".to_owned(),
        columns: vec![
            ColumnDef {
                name: "id".to_owned(),
                affinity: TypeAffinity::Integer,
                is_ipk: true,
                not_null: true,
            },
            ColumnDef {
                name: "name".to_owned(),
                affinity: TypeAffinity::Text,
                is_ipk: false,
                not_null: true,
            },
            ColumnDef {
                name: "email".to_owned(),
                affinity: TypeAffinity::Text,
                is_ipk: false,
                not_null: false,
            },
        ],
        without_rowid: false,
        strict: false,
    });
    schema.add_table(TableDef {
        name: "orders".to_owned(),
        columns: vec![
            ColumnDef {
                name: "id".to_owned(),
                affinity: TypeAffinity::Integer,
                is_ipk: true,
                not_null: true,
            },
            ColumnDef {
                name: "user_id".to_owned(),
                affinity: TypeAffinity::Integer,
                is_ipk: false,
                not_null: true,
            },
            ColumnDef {
                name: "amount".to_owned(),
                affinity: TypeAffinity::Real,
                is_ipk: false,
                not_null: false,
            },
        ],
        without_rowid: false,
        strict: false,
    });
    schema
}

fn parse_one(sql: &str) -> Statement {
    let mut p = Parser::from_sql(sql);
    let (stmts, errs) = p.parse_all();
    assert!(errs.is_empty(), "parse errors: {errs:?}");
    assert_eq!(stmts.len(), 1);
    stmts.into_iter().next().unwrap()
}

#[test]
fn test_count_zero_args() {
    let sql = "SELECT count();";
    let (stmts, parse_errors) = crate::parser::Parser::from_sql(sql).parse_all();
    assert!(
        parse_errors.is_empty(),
        "expected no parse errors, got {parse_errors:?}"
    );
    let schema = Schema::new();
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmts.into_iter().next().unwrap());
    assert!(errors.is_empty(), "expected no errors, got {errors:?}");
}

#[test]
fn test_update_returning_from_clause() {
    let schema = make_schema();
    // SQLite's RETURNING clause CANNOT reference tables from the FROM clause.
    // It can ONLY reference the target table being modified.
    let stmt = parse_one(
        "UPDATE users SET id = 1 FROM orders WHERE users.id = orders.id RETURNING orders.id",
    );
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmt);
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");
    assert!(
        matches!(errors[0].kind, SemanticErrorKind::UnresolvedColumn { .. }),
        "Expected UnresolvedColumn error for orders.id, got {:?}",
        errors[0]
    );
}

#[test]
fn test_order_by_select_alias() {
    let schema = make_schema();
    let stmt = parse_one("SELECT id AS alias_id FROM users ORDER BY alias_id");
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmt);
    assert!(errors.is_empty(), "Expected no errors, got {:?}", errors);
}

#[test]
fn test_upsert_excluded_unqualified_column() {
    let schema = make_schema();
    // In UPSERT, unqualified columns in the SET and WHERE clauses should resolve
    // to the target table (users), NOT the `excluded` pseudo-table.
    // The query should pass semantic analysis without 'AmbiguousColumn' errors.
    let stmt = parse_one(
        "INSERT INTO users (id, name) VALUES (1, 'alice') ON CONFLICT (id) DO UPDATE SET name = excluded.name WHERE id > 0",
    );
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmt);
    assert!(
        errors.is_empty(),
        "Expected no errors for UPSERT, got {:?}",
        errors
    );
}

#[test]
fn test_insert_values_cannot_see_target_table() {
    let schema = make_schema();
    // The VALUES clause cannot reference the target table columns
    let stmt = parse_one("INSERT INTO users (id, name) VALUES (id, 'alice')");
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmt);
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");
    assert!(
        matches!(errors[0].kind, SemanticErrorKind::UnresolvedColumn { .. }),
        "Expected UnresolvedColumn error, got {:?}",
        errors[0]
    );
}

#[test]
fn test_insert_select_cannot_see_target_table() {
    let schema = make_schema();
    // The SELECT clause cannot implicitly reference the target table columns
    let stmt = parse_one("INSERT INTO users (id, name) SELECT id, 'alice'");
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmt);
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");
    assert!(
        matches!(errors[0].kind, SemanticErrorKind::UnresolvedColumn { .. }),
        "Expected UnresolvedColumn error, got {:?}",
        errors[0]
    );
}

#[test]
fn test_update_limit_cannot_see_target_table() {
    let schema = make_schema();
    let stmt = parse_one("UPDATE users SET id = 2 LIMIT id");
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmt);
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");
    assert!(
        matches!(errors[0].kind, SemanticErrorKind::UnresolvedColumn { .. }),
        "Expected UnresolvedColumn error, got {:?}",
        errors[0]
    );
}

#[test]
fn test_delete_limit_cannot_see_target_table() {
    let schema = make_schema();
    let stmt = parse_one("DELETE FROM users LIMIT id");
    let mut resolver = Resolver::new(&schema);
    let errors = resolver.resolve_statement(&stmt);
    assert_eq!(errors.len(), 1, "Expected exactly 1 error");
    assert!(
        matches!(errors[0].kind, SemanticErrorKind::UnresolvedColumn { .. }),
        "Expected UnresolvedColumn error, got {:?}",
        errors[0]
    );
}
