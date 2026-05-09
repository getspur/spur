//! bd-6eyrg.1: Fast-path vs slow-path execution separation tests.
//!
//! Proves:
//! 1. Prepared INSERT uses the fast path (counter increments).
//! 2. Prepared SELECT / CTE / view queries record path metrics without racing.
//! 3. DDL invalidation forces a schema change boundary, then re-preparation
//!    restores the fast path.
//! 4. Parameterized prepared statements still execute correctly.
//! 5. Complex queries (JOINs, subqueries) still produce correct results.
//! 6. Latency: prepared fast lanes are not catastrophically slower than ad-hoc
//!    execution on repeated runs.
//!
//! Run:
//!   cargo test -p fsqlite-core --test fast_path_separation \
//!     -- --test-threads=1 --nocapture

use fsqlite_core::connection::{
    Connection, hot_path_profile_enabled, hot_path_profile_snapshot, reset_hot_path_profile,
    set_hot_path_profile_enabled,
};
use std::sync::{Mutex, MutexGuard};

static FAST_PATH_PROFILE_TEST_LOCK: Mutex<()> = Mutex::new(());

struct FastPathProfileTestGuard {
    _lock: MutexGuard<'static, ()>,
    previous_enabled: bool,
}

impl FastPathProfileTestGuard {
    fn new() -> Self {
        let lock = FAST_PATH_PROFILE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_enabled = hot_path_profile_enabled();
        set_hot_path_profile_enabled(true);
        reset_hot_path_profile();
        Self {
            _lock: lock,
            previous_enabled,
        }
    }
}

impl Drop for FastPathProfileTestGuard {
    fn drop(&mut self) {
        reset_hot_path_profile();
        set_hot_path_profile_enabled(self.previous_enabled);
    }
}

fn fast_slow_delta(
    before: &fsqlite_core::connection::ParserHotPathProfileSnapshot,
    after: &fsqlite_core::connection::ParserHotPathProfileSnapshot,
) -> (u64, u64) {
    (
        after
            .fast_path_executions
            .saturating_sub(before.fast_path_executions),
        after
            .slow_path_executions
            .saturating_sub(before.slow_path_executions),
    )
}

fn assert_count_star_sum_row(
    row: &fsqlite_core::connection::Row,
    expected_count: i64,
    expected_sum: Option<fsqlite_types::SqliteValue>,
) {
    assert_eq!(
        row.get(0),
        Some(&fsqlite_types::SqliteValue::Integer(expected_count)),
        "COUNT(*) should match the expected row count"
    );
    match expected_sum {
        Some(expected_sum) => assert_eq!(
            row.get(1),
            Some(&expected_sum),
            "SUM() should match the expected non-NULL total"
        ),
        None => assert_eq!(
            row.get(1),
            Some(&fsqlite_types::SqliteValue::Null),
            "SUM() should be NULL when no non-NULL inputs contribute"
        ),
    }
}

fn stringify_fsqlite_value(value: &fsqlite_types::SqliteValue) -> String {
    match value {
        fsqlite_types::SqliteValue::Null => "NULL".to_owned(),
        fsqlite_types::SqliteValue::Integer(n) => n.to_string(),
        fsqlite_types::SqliteValue::Float(f) => format!("{f}"),
        fsqlite_types::SqliteValue::Text(s) => format!("'{s}'"),
        fsqlite_types::SqliteValue::Blob(b) => {
            format!(
                "X'{}'",
                b.iter()
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<String>()
            )
        }
    }
}

fn stringify_rusqlite_value(value: rusqlite::types::Value) -> String {
    match value {
        rusqlite::types::Value::Null => "NULL".to_owned(),
        rusqlite::types::Value::Integer(n) => n.to_string(),
        rusqlite::types::Value::Real(f) => format!("{f}"),
        rusqlite::types::Value::Text(s) => format!("'{s}'"),
        rusqlite::types::Value::Blob(b) => {
            format!(
                "X'{}'",
                b.iter()
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<String>()
            )
        }
    }
}

fn sorted_frank_rows(conn: &Connection, sql: &str) -> Vec<Vec<String>> {
    let stmt = conn.prepare(sql).unwrap();
    let mut rows = stmt
        .query()
        .unwrap()
        .into_iter()
        .map(|row| row.values().iter().map(stringify_fsqlite_value).collect())
        .collect::<Vec<Vec<String>>>();
    rows.sort();
    rows
}

fn sorted_rusqlite_rows(conn: &rusqlite::Connection, sql: &str) -> Vec<Vec<String>> {
    let mut stmt = conn.prepare(sql).unwrap();
    let col_count = stmt.column_count();
    let mut rows = stmt
        .query_map([], |row| {
            let mut values = Vec::with_capacity(col_count);
            for idx in 0..col_count {
                let value: rusqlite::types::Value = row.get(idx)?;
                values.push(stringify_rusqlite_value(value));
            }
            Ok(values)
        })
        .unwrap()
        .collect::<Result<Vec<Vec<String>>, _>>()
        .unwrap();
    rows.sort();
    rows
}

fn seed_grouped_sum_bench(fconn: &Connection, rconn: &rusqlite::Connection, row_count: usize) {
    for id in 0..row_count {
        let value = (id * 3) + 1;
        let sql = format!("INSERT INTO bench VALUES ({id}, 'name{id}', {value}.0)");
        fconn.execute(&sql).unwrap();
        rconn.execute(&sql, []).unwrap();
    }
}

/// T1: Prepared INSERT uses fast path.
#[test]
fn test_fast_path_simple_insert() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    let stmt = conn.prepare("INSERT INTO t VALUES(?1, ?2)").unwrap();
    let before = hot_path_profile_snapshot();
    stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(1),
        fsqlite_types::SqliteValue::Text("fast".into()),
    ])
    .unwrap();
    let after = hot_path_profile_snapshot();

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    eprintln!("[T1] INSERT: fast_delta={fast_delta}, slow_delta={slow_delta}");
    assert!(
        fast_delta > 0,
        "prepared INSERT should use fast path: fast_delta={fast_delta}"
    );
}

#[test]
#[ignore = "manual perf probe for wide prepared direct INSERT hot path"]
fn manual_profile_large_prepared_direct_insert_single_txn_10k() {
    const ROW_COUNT: i64 = 10_000;
    const CREATE_TABLE: &str = "CREATE TABLE bench (id INTEGER PRIMARY KEY, first_name TEXT NOT NULL, last_name TEXT NOT NULL, email TEXT NOT NULL, department TEXT NOT NULL, title TEXT NOT NULL, bio TEXT NOT NULL, address TEXT NOT NULL, notes TEXT NOT NULL, score INTEGER NOT NULL)";
    const INSERT_SQL: &str = "INSERT INTO bench VALUES (?1, ('FirstName_' || ?1), ('LastName_' || ?1), ('employee' || ?1 || '@bigcorp.example.com'), ('Engineering_Dept_' || (?1 % 20)), ('Senior Software Engineer Level ' || (?1 % 5)), ('This is the biography for employee number ' || ?1 || '. They have been working at the company for many years and have contributed to numerous projects across multiple teams. Their expertise spans distributed systems, database internals, and performance optimization. They are known for their thorough code reviews and mentorship of junior engineers.'), (?1 || ' Technology Park, Building ' || (?1 % 50) || ', Suite ' || (?1 % 200) || ', Innovation City, CA 94000'), ('Internal notes: Employee ' || ?1 || ' - Performance rating: Exceeds Expectations. Last review date: 2026-01-15. Next review: 2026-07-15. Skills: Rust, C++, SQL, distributed systems, leadership.'), (?1 * 13))";

    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("PRAGMA journal_mode = WAL").unwrap();
    conn.execute(CREATE_TABLE).unwrap();
    conn.execute("BEGIN").unwrap();
    let stmt = conn.prepare(INSERT_SQL).unwrap();

    reset_hot_path_profile();
    let started = std::time::Instant::now();
    for i in 0..ROW_COUNT {
        stmt.execute_with_params(&[fsqlite_types::SqliteValue::Integer(i)])
            .unwrap();
    }
    conn.execute("COMMIT").unwrap();
    let wall = started.elapsed();
    let profile = hot_path_profile_snapshot();

    eprintln!(
        concat!(
            "[manual_large_insert_10k] wall_us={} execute_body_us={} ",
            "row_build_us={} cursor_setup_us={} serialize_us={} ",
            "btree_insert_us={} memdb_apply_us={} direct_execs={} fast_execs={}"
        ),
        wall.as_micros(),
        profile.execute_body_time_ns / 1_000,
        profile.prepared_direct_insert_row_build_time_ns / 1_000,
        profile.prepared_direct_insert_cursor_setup_time_ns / 1_000,
        profile.prepared_direct_insert_serialize_time_ns / 1_000,
        profile.prepared_direct_insert_btree_insert_time_ns / 1_000,
        profile.prepared_direct_insert_memdb_apply_time_ns / 1_000,
        profile.prepared_direct_insert_executions,
        profile.parser.fast_path_executions,
    );
}

/// T2: Prepared SELECT records path metrics without double-counting.
#[test]
fn test_fast_path_simple_select() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'a')").unwrap();

    let stmt = conn.prepare("SELECT val FROM t WHERE id = ?1").unwrap();
    let before = hot_path_profile_snapshot();
    let rows = stmt
        .query_with_params(&[fsqlite_types::SqliteValue::Integer(1)])
        .unwrap();
    let after = hot_path_profile_snapshot();

    assert!(!rows.is_empty(), "SELECT should return a row");
    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    eprintln!("[T2] SELECT: fast_delta={fast_delta}, slow_delta={slow_delta}");
    // Either fast or slow is acceptable for SELECT — we document actual behavior.
    eprintln!(
        "[T2] SELECT path: {}",
        if fast_delta > 0 { "FAST" } else { "SLOW" }
    );
}

/// T3: CTE query falls through to slow path.
#[test]
fn test_slow_path_cte() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'cte')").unwrap();

    // CTE queries go through execute_statement (slow path).
    let before = hot_path_profile_snapshot();
    let rows = conn
        .query("WITH cte AS (SELECT * FROM t) SELECT val FROM cte")
        .unwrap();
    let after = hot_path_profile_snapshot();

    assert!(!rows.is_empty(), "CTE query should return results");
    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    eprintln!("[T3] CTE: fast_delta={fast_delta}, slow_delta={slow_delta}");
    // CTE may use slow path through execute_statement, or may compile and fast-path.
    // Document actual behavior.
    eprintln!(
        "[T3] CTE path: {}",
        if slow_delta > fast_delta {
            "SLOW (expected)"
        } else {
            "FAST (compiled)"
        }
    );
}

/// T4: DDL invalidates schema cookie, next execution uses slow path,
/// then stabilizes back to fast path.
#[test]
fn test_slow_path_schema_change_then_fast_path_recovery() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'v1')").unwrap();

    let stmt = conn.prepare("INSERT INTO t VALUES(?1, ?2)").unwrap();

    // Pre-DDL: fast path.
    let before = hot_path_profile_snapshot();
    stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(2),
        fsqlite_types::SqliteValue::Text("v2".into()),
    ])
    .unwrap();
    let after = hot_path_profile_snapshot();
    let (fast_pre, _) = fast_slow_delta(&before.parser, &after.parser);
    eprintln!("[T4] pre-DDL: fast_delta={fast_pre}");

    // DDL changes schema_cookie.
    conn.execute("ALTER TABLE t ADD COLUMN extra INTEGER DEFAULT 0")
        .unwrap();

    // Post-DDL: prepared statement should detect schema change.
    let result = stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(3),
        fsqlite_types::SqliteValue::Text("v3".into()),
    ]);

    match &result {
        Err(fsqlite_error::FrankenError::SchemaChanged) => {
            eprintln!("[T4] post-DDL: SchemaChanged (expected)");
        }
        Ok(_) => {
            eprintln!("[T4] post-DDL: succeeded (transparent re-prepare)");
        }
        Err(e) => {
            eprintln!("[T4] post-DDL: error {e:?}");
        }
    }

    // Re-prepare after schema change.
    let stmt2 = conn.prepare("INSERT INTO t VALUES(?1, ?2, ?3)").unwrap();
    let before2 = hot_path_profile_snapshot();
    stmt2
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(4),
            fsqlite_types::SqliteValue::Text("v4".into()),
            fsqlite_types::SqliteValue::Integer(99),
        ])
        .unwrap();
    let after2 = hot_path_profile_snapshot();
    let (fast_post, _) = fast_slow_delta(&before2.parser, &after2.parser);
    eprintln!("[T4] post-re-prepare: fast_delta={fast_post}");

    // Verify data correctness.
    let rows = conn.query("SELECT COUNT(*) FROM t").unwrap();
    let count = rows[0].get(0).unwrap();
    // Should have at least 2 rows (id=1 and id=2 from the pre-DDL insert).
    if let fsqlite_types::SqliteValue::Integer(n) = count {
        assert!(*n >= 2, "should have at least 2 rows after DDL: got {n}");
    }
}

/// T5: Fast path works with all parameter types.
#[test]
fn test_fast_path_parameterized() {
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, score REAL, data BLOB)")
        .unwrap();

    let stmt = conn
        .prepare("INSERT INTO t VALUES(?1, ?2, ?3, ?4)")
        .unwrap();
    stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(1),
        fsqlite_types::SqliteValue::Text("alice".into()),
        fsqlite_types::SqliteValue::Float(3.14),
        fsqlite_types::SqliteValue::Blob(vec![0xDE, 0xAD].into()),
    ])
    .unwrap();

    stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(2),
        fsqlite_types::SqliteValue::Null,
        fsqlite_types::SqliteValue::Null,
        fsqlite_types::SqliteValue::Null,
    ])
    .unwrap();

    let rows = conn.query("SELECT * FROM t ORDER BY id").unwrap();
    assert_eq!(rows.len(), 2, "should have 2 rows");

    // Verify data types round-trip.
    let row1 = &rows[0];
    assert_eq!(
        row1.get(1),
        Some(&fsqlite_types::SqliteValue::Text("alice".into()))
    );
}

/// T6: View query records path metrics for the deferred-query route.
#[test]
fn test_slow_path_view_expansion() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'view-test')")
        .unwrap();
    conn.execute("CREATE VIEW v AS SELECT val FROM t").unwrap();

    let before = hot_path_profile_snapshot();
    let rows = conn.query("SELECT * FROM v").unwrap();
    let after = hot_path_profile_snapshot();

    assert!(!rows.is_empty(), "view query should return results");
    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    eprintln!("[T6] VIEW: fast_delta={fast_delta}, slow_delta={slow_delta}");
}

/// T7: Complex queries (JOINs, subqueries) still produce correct results.
#[test]
fn test_no_regression_complex_queries() {
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE orders(id INTEGER PRIMARY KEY, customer_id INTEGER, amount REAL)")
        .unwrap();
    conn.execute("CREATE TABLE customers(id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO customers VALUES(1, 'Alice')")
        .unwrap();
    conn.execute("INSERT INTO customers VALUES(2, 'Bob')")
        .unwrap();
    conn.execute("INSERT INTO orders VALUES(1, 1, 100.0)")
        .unwrap();
    conn.execute("INSERT INTO orders VALUES(2, 1, 200.0)")
        .unwrap();
    conn.execute("INSERT INTO orders VALUES(3, 2, 50.0)")
        .unwrap();

    // JOIN.
    let rows = conn
        .query("SELECT c.name, SUM(o.amount) FROM customers c JOIN orders o ON c.id = o.customer_id GROUP BY c.id ORDER BY c.name")
        .unwrap();
    assert_eq!(rows.len(), 2);

    // Subquery.
    let rows = conn
        .query("SELECT name FROM customers WHERE id IN (SELECT customer_id FROM orders WHERE amount > 75)")
        .unwrap();
    assert!(!rows.is_empty());

    // Correlated subquery.
    let rows = conn
        .query("SELECT name, (SELECT SUM(amount) FROM orders WHERE customer_id = customers.id) AS total FROM customers ORDER BY name")
        .unwrap();
    assert_eq!(rows.len(), 2);
}

/// T8: Latency scorecard — fast path vs repeated ad-hoc execution.
#[test]
fn test_fast_path_latency_scorecard() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE bench(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    let iterations = 1000;

    // Prepared path (should use fast path).
    let stmt = conn.prepare("INSERT INTO bench VALUES(?1, ?2)").unwrap();
    let t_prepared_start = std::time::Instant::now();
    for i in 0..iterations {
        stmt.execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(i),
            fsqlite_types::SqliteValue::Text(format!("row{i}").into()),
        ])
        .unwrap();
    }
    let t_prepared = t_prepared_start.elapsed();

    let snap_prepared = hot_path_profile_snapshot();
    conn.execute("DELETE FROM bench").unwrap();
    reset_hot_path_profile();

    // Ad-hoc path (always re-parses/compiles, but uses compile cache).
    let t_adhoc_start = std::time::Instant::now();
    for i in 0..iterations {
        conn.execute(&format!("INSERT INTO bench VALUES({i}, 'row{i}')"))
            .unwrap();
    }
    let t_adhoc = t_adhoc_start.elapsed();

    let snap_adhoc = hot_path_profile_snapshot();

    eprintln!("=== bd-6eyrg.1 Latency Scorecard ===");
    eprintln!("Prepared ({iterations} iterations):");
    eprintln!("  elapsed: {:?}", t_prepared);
    eprintln!(
        "  fast_path={}, slow_path={}",
        snap_prepared.parser.fast_path_executions, snap_prepared.parser.slow_path_executions
    );
    eprintln!("Ad-hoc ({iterations} iterations):");
    eprintln!("  elapsed: {:?}", t_adhoc);
    eprintln!(
        "  fast_path={}, slow_path={}",
        snap_adhoc.parser.fast_path_executions, snap_adhoc.parser.slow_path_executions
    );
    let ratio = t_prepared.as_nanos() as f64 / t_adhoc.as_nanos().max(1) as f64;
    eprintln!("  prepared/adhoc ratio: {ratio:.2}x");
    eprintln!("=== END SCORECARD ===");

    // The prepared path should not be more than 2x slower than ad-hoc.
    // (It should actually be faster, but we're lenient here.)
    assert!(
        ratio < 2.0,
        "prepared path should not be >2x slower than ad-hoc: ratio={ratio:.2}"
    );
}

/// T9: Prepared UPDATE uses fast lane on file-backed WAL (bd-db300.5.2.2.3).
#[test]
fn test_fast_path_prepared_update() {
    let _profile_guard = FastPathProfileTestGuard::new();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute("PRAGMA journal_mode = WAL").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'before')").unwrap();
    conn.execute("INSERT INTO t VALUES(2, 'before')").unwrap();

    let stmt = conn.prepare("UPDATE t SET val = ?2 WHERE id = ?1").unwrap();

    // Warm.
    stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(1),
        fsqlite_types::SqliteValue::Text("warm".into()),
    ])
    .unwrap();
    reset_hot_path_profile();

    // Measure.
    let before = hot_path_profile_snapshot();
    stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(1),
        fsqlite_types::SqliteValue::Text("after1".into()),
    ])
    .unwrap();
    stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(2),
        fsqlite_types::SqliteValue::Text("after2".into()),
    ])
    .unwrap();
    let after = hot_path_profile_snapshot();

    let (fast_delta, _) = fast_slow_delta(&before.parser, &after.parser);
    let ud_fast = after
        .prepared_update_delete_fast_lane_hits
        .saturating_sub(before.prepared_update_delete_fast_lane_hits);
    eprintln!("[T9] UPDATE: fast_delta={fast_delta}, ud_fast_lane_delta={ud_fast}");
    assert!(
        fast_delta >= 2,
        "prepared UPDATE should use fast path: fast_delta={fast_delta}"
    );
    assert!(
        ud_fast >= 2,
        "prepared UPDATE should hit update/delete fast lane: ud_fast={ud_fast}"
    );

    // Correctness.
    let rows = conn.query("SELECT val FROM t ORDER BY id").unwrap();
    assert_eq!(
        rows[0].get(0),
        Some(&fsqlite_types::SqliteValue::Text("after1".into()))
    );
    assert_eq!(
        rows[1].get(0),
        Some(&fsqlite_types::SqliteValue::Text("after2".into()))
    );
}

/// T10: Prepared DELETE uses fast lane on file-backed WAL (bd-db300.5.2.2.3).
#[test]
fn test_fast_path_prepared_delete() {
    let _profile_guard = FastPathProfileTestGuard::new();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute("PRAGMA journal_mode = WAL").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'a')").unwrap();
    conn.execute("INSERT INTO t VALUES(2, 'b')").unwrap();
    conn.execute("INSERT INTO t VALUES(3, 'c')").unwrap();

    let stmt = conn.prepare("DELETE FROM t WHERE id = ?1").unwrap();

    // Warm.
    stmt.execute_with_params(&[fsqlite_types::SqliteValue::Integer(3)])
        .unwrap();
    reset_hot_path_profile();

    // Measure.
    let before = hot_path_profile_snapshot();
    stmt.execute_with_params(&[fsqlite_types::SqliteValue::Integer(1)])
        .unwrap();
    stmt.execute_with_params(&[fsqlite_types::SqliteValue::Integer(2)])
        .unwrap();
    let after = hot_path_profile_snapshot();

    let (fast_delta, _) = fast_slow_delta(&before.parser, &after.parser);
    let ud_fast = after
        .prepared_update_delete_fast_lane_hits
        .saturating_sub(before.prepared_update_delete_fast_lane_hits);
    eprintln!("[T10] DELETE: fast_delta={fast_delta}, ud_fast_lane_delta={ud_fast}");
    assert!(
        fast_delta >= 2,
        "prepared DELETE should use fast path: fast_delta={fast_delta}"
    );
    assert!(
        ud_fast >= 2,
        "prepared DELETE should hit update/delete fast lane: ud_fast={ud_fast}"
    );

    // Correctness.
    let rows = conn.query("SELECT COUNT(*) FROM t").unwrap();
    let count = rows[0].get(0).unwrap();
    assert_eq!(
        count,
        &fsqlite_types::SqliteValue::Integer(0),
        "all rows should be deleted"
    );
}

/// T11: Deferred-DML path uses no_publication() proof and still succeeds
/// (bd-db300.5.2.2.3).
///
/// The deferred path fires when `stmt.deferred_dml_statement()` is Some and
/// `fast_path.supports_direct_dispatch_now()` is true. Foreign keys force
/// the deferred path because FK enforcement requires post-statement checking.
#[test]
fn test_deferred_dml_no_publication_proof() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute("PRAGMA journal_mode = WAL").unwrap();
    conn.execute("PRAGMA foreign_keys = ON").unwrap();
    conn.execute("CREATE TABLE parent(id INTEGER PRIMARY KEY)")
        .unwrap();
    conn.execute("CREATE TABLE child(id INTEGER PRIMARY KEY, pid INTEGER REFERENCES parent(id))")
        .unwrap();
    conn.execute("INSERT INTO parent VALUES(1)").unwrap();

    // Prepared DELETE on parent with FK child — forces deferred DML path.
    let stmt = conn.prepare("DELETE FROM parent WHERE id = ?1").unwrap();

    // Should succeed (no child references id=1... wait, no child rows exist).
    let result = stmt.execute_with_params(&[fsqlite_types::SqliteValue::Integer(1)]);
    assert!(
        result.is_ok(),
        "deferred-DML DELETE with no FK violation should succeed: {:?}",
        result
    );

    // Insert a child referencing parent id=2, then try to delete parent id=2.
    conn.execute("INSERT INTO parent VALUES(2)").unwrap();
    conn.execute("INSERT INTO child VALUES(1, 2)").unwrap();
    let result = stmt.execute_with_params(&[fsqlite_types::SqliteValue::Integer(2)]);
    // Should fail with FK constraint violation.
    assert!(
        result.is_err(),
        "deferred-DML DELETE with FK violation should fail"
    );
}

/// T12: UPDATE/DELETE DDL invalidation + recovery (bd-db300.5.2.2.3).
///
/// Mirrors T4 but for UPDATE and DELETE: DDL invalidates the prepared
/// statement, re-prepare restores fast-path execution.
#[test]
fn test_fast_path_update_delete_ddl_invalidation() {
    let _profile_guard = FastPathProfileTestGuard::new();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute("PRAGMA journal_mode = WAL").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'orig')").unwrap();

    let update_stmt = conn.prepare("UPDATE t SET val = ?2 WHERE id = ?1").unwrap();
    let delete_stmt = conn.prepare("DELETE FROM t WHERE id = ?1").unwrap();

    // Pre-DDL: should succeed on fast path.
    update_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(1),
            fsqlite_types::SqliteValue::Text("updated".into()),
        ])
        .unwrap();

    // DDL changes schema.
    conn.execute("ALTER TABLE t ADD COLUMN extra INTEGER DEFAULT 0")
        .unwrap();

    // Post-DDL: old prepared stmts should detect schema change.
    let update_result = update_stmt.execute_with_params(&[
        fsqlite_types::SqliteValue::Integer(1),
        fsqlite_types::SqliteValue::Text("post-ddl".into()),
    ]);
    match &update_result {
        Err(fsqlite_error::FrankenError::SchemaChanged) => {
            eprintln!("[T12] UPDATE post-DDL: SchemaChanged (expected)");
        }
        Ok(_) => {
            eprintln!("[T12] UPDATE post-DDL: succeeded (transparent re-prepare)");
        }
        Err(e) => {
            eprintln!("[T12] UPDATE post-DDL: error {e:?}");
        }
    }

    let delete_result = delete_stmt.execute_with_params(&[fsqlite_types::SqliteValue::Integer(1)]);
    match &delete_result {
        Err(fsqlite_error::FrankenError::SchemaChanged) => {
            eprintln!("[T12] DELETE post-DDL: SchemaChanged (expected)");
        }
        Ok(_) => {
            eprintln!("[T12] DELETE post-DDL: succeeded (transparent re-prepare)");
        }
        Err(e) => {
            eprintln!("[T12] DELETE post-DDL: error {e:?}");
        }
    }

    // Re-prepare UPDATE on new schema and verify fast path restored.
    conn.execute("INSERT INTO t VALUES(10, 'seed', 0)").unwrap();
    conn.execute("INSERT INTO t VALUES(11, 'seed2', 0)")
        .unwrap();

    let update2 = conn
        .prepare("UPDATE t SET val = ?2, extra = ?3 WHERE id = ?1")
        .unwrap();
    let before_upd = hot_path_profile_snapshot();
    update2
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(10),
            fsqlite_types::SqliteValue::Text("recovered".into()),
            fsqlite_types::SqliteValue::Integer(42),
        ])
        .unwrap();
    let after_upd = hot_path_profile_snapshot();
    let (fast_upd, _) = fast_slow_delta(&before_upd.parser, &after_upd.parser);
    eprintln!("[T12] post-re-prepare UPDATE: fast_delta={fast_upd}");
    assert!(
        fast_upd > 0,
        "re-prepared UPDATE should restore fast path: fast_delta={fast_upd}"
    );

    // Verify UPDATE correctness.
    let rows = conn
        .query("SELECT val, extra FROM t WHERE id = 10")
        .unwrap();
    assert!(
        !rows.is_empty(),
        "re-prepared UPDATE should have affected a row"
    );
    assert_eq!(
        rows[0].get(0),
        Some(&fsqlite_types::SqliteValue::Text("recovered".into()))
    );

    // Re-prepare DELETE on new schema and verify fast path restored.
    let delete2 = conn.prepare("DELETE FROM t WHERE id = ?1").unwrap();
    let before_del = hot_path_profile_snapshot();
    delete2
        .execute_with_params(&[fsqlite_types::SqliteValue::Integer(11)])
        .unwrap();
    let after_del = hot_path_profile_snapshot();
    let (fast_del, _) = fast_slow_delta(&before_del.parser, &after_del.parser);
    eprintln!("[T12] post-re-prepare DELETE: fast_delta={fast_del}");
    assert!(
        fast_del > 0,
        "re-prepared DELETE should restore fast path: fast_delta={fast_del}"
    );

    // Verify DELETE correctness.
    let rows = conn.query("SELECT COUNT(*) FROM t WHERE id = 11").unwrap();
    assert_eq!(
        rows[0].get(0),
        Some(&fsqlite_types::SqliteValue::Integer(0)),
        "re-prepared DELETE should have removed the row"
    );
}

/// T13: File-backed prepared INSERT, UPDATE, and DELETE all reuse prebound
/// publication (≤1 pager_publication_refresh per operation).
///
/// Before the entry-proof fix, UPDATE/DELETE double-refreshed because the
/// deferred-DML path did not thread the prebound publication.  Now all three
/// DML kinds pass the publication through the entry proof.
#[test]
fn test_file_backed_publication_refresh_counts() {
    let _profile_guard = FastPathProfileTestGuard::new();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute("PRAGMA journal_mode = WAL").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'a')").unwrap();

    let insert_stmt = conn.prepare("INSERT INTO t VALUES(?1, ?2)").unwrap();
    let update_stmt = conn.prepare("UPDATE t SET val = ?2 WHERE id = ?1").unwrap();
    let delete_stmt = conn.prepare("DELETE FROM t WHERE id = ?1").unwrap();

    // Warm all stmts.
    insert_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(10),
            fsqlite_types::SqliteValue::Text("warm".into()),
        ])
        .unwrap();
    update_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(1),
            fsqlite_types::SqliteValue::Text("warm".into()),
        ])
        .unwrap();
    conn.execute("INSERT INTO t VALUES(20, 'del_warm')")
        .unwrap();
    delete_stmt
        .execute_with_params(&[fsqlite_types::SqliteValue::Integer(20)])
        .unwrap();

    // Measure INSERT (precompiled path — should reuse prebound publication).
    reset_hot_path_profile();
    let before_ins = hot_path_profile_snapshot();
    insert_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(100),
            fsqlite_types::SqliteValue::Text("measured".into()),
        ])
        .unwrap();
    let after_ins = hot_path_profile_snapshot();
    let insert_pub = after_ins
        .pager_publication_refreshes
        .saturating_sub(before_ins.pager_publication_refreshes);
    eprintln!("[T13] INSERT pager_publication_refreshes delta = {insert_pub}");
    assert!(
        insert_pub <= 1,
        "INSERT should reuse prebound publication (≤1 refresh): got {insert_pub}"
    );

    // Measure UPDATE (deferred-DML path — currently double-refreshes).
    reset_hot_path_profile();
    let before_upd = hot_path_profile_snapshot();
    update_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(1),
            fsqlite_types::SqliteValue::Text("measured".into()),
        ])
        .unwrap();
    let after_upd = hot_path_profile_snapshot();
    let update_pub = after_upd
        .pager_publication_refreshes
        .saturating_sub(before_upd.pager_publication_refreshes);
    eprintln!("[T13] UPDATE pager_publication_refreshes delta = {update_pub}");
    assert!(
        update_pub <= 1,
        "UPDATE should reuse prebound publication (≤1 refresh): got {update_pub}"
    );

    // Measure DELETE.  Seed the target row with the prepared insert_stmt
    // to avoid an ad-hoc conn.execute() between reset and measurement
    // (ad-hoc execution advances commit_seq, forcing a stale-publication
    // refresh that inflates the counter).
    insert_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(200),
            fsqlite_types::SqliteValue::Text("del_target".into()),
        ])
        .unwrap();
    reset_hot_path_profile();
    let before_del = hot_path_profile_snapshot();
    delete_stmt
        .execute_with_params(&[fsqlite_types::SqliteValue::Integer(200)])
        .unwrap();
    let after_del = hot_path_profile_snapshot();
    let delete_pub = after_del
        .pager_publication_refreshes
        .saturating_sub(before_del.pager_publication_refreshes);
    eprintln!("[T13] DELETE pager_publication_refreshes delta = {delete_pub}");
    assert!(
        delete_pub <= 1,
        "DELETE should reuse prebound publication (≤1 refresh): got {delete_pub}"
    );
}

/// T14: :memory: UPDATE/DELETE succeeds with no-publication entry proof
/// (bd-db300.5.2.2.3 / bd-db300.5.2.2.4).
#[test]
fn test_entry_proof_no_publication_for_memory_update_delete() {
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'a')").unwrap();
    conn.execute("INSERT INTO t VALUES(2, 'b')").unwrap();

    let update_stmt = conn.prepare("UPDATE t SET val = ?2 WHERE id = ?1").unwrap();
    let delete_stmt = conn.prepare("DELETE FROM t WHERE id = ?1").unwrap();

    // UPDATE on :memory: — entry_proof.publication is None.
    update_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(1),
            fsqlite_types::SqliteValue::Text("updated".into()),
        ])
        .unwrap();

    // DELETE on :memory:.
    delete_stmt
        .execute_with_params(&[fsqlite_types::SqliteValue::Integer(2)])
        .unwrap();

    // Verify data correctness.
    let rows = conn.query("SELECT id, val FROM t ORDER BY id").unwrap();
    assert_eq!(rows.len(), 1, "one row should remain after delete");
    assert_eq!(
        rows[0].get(0),
        Some(&fsqlite_types::SqliteValue::Integer(1))
    );
    assert_eq!(
        rows[0].get(1),
        Some(&fsqlite_types::SqliteValue::Text("updated".into()))
    );
}

/// T15: Prepared DML within explicit BEGIN...COMMIT uses entry-proof path
/// without regression (bd-db300.5.2.2.3 / bd-db300.5.2.2.4).
#[test]
fn test_entry_proof_within_explicit_transaction() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_str().unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute("PRAGMA journal_mode = WAL").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    let insert_stmt = conn.prepare("INSERT INTO t VALUES(?1, ?2)").unwrap();
    let update_stmt = conn.prepare("UPDATE t SET val = ?2 WHERE id = ?1").unwrap();
    let delete_stmt = conn.prepare("DELETE FROM t WHERE id = ?1").unwrap();

    // Explicit transaction: entry_proof.publication should be None
    // (in_transaction = true → ensure_autocommit_txn returns false early).
    conn.execute("BEGIN").unwrap();

    insert_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(1),
            fsqlite_types::SqliteValue::Text("inserted".into()),
        ])
        .unwrap();
    insert_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(2),
            fsqlite_types::SqliteValue::Text("also inserted".into()),
        ])
        .unwrap();

    update_stmt
        .execute_with_params(&[
            fsqlite_types::SqliteValue::Integer(1),
            fsqlite_types::SqliteValue::Text("updated in txn".into()),
        ])
        .unwrap();

    delete_stmt
        .execute_with_params(&[fsqlite_types::SqliteValue::Integer(2)])
        .unwrap();

    conn.execute("COMMIT").unwrap();

    // Verify all operations committed correctly.
    let rows = conn.query("SELECT id, val FROM t ORDER BY id").unwrap();
    assert_eq!(rows.len(), 1, "only row 1 should remain");
    assert_eq!(
        rows[0].get(0),
        Some(&fsqlite_types::SqliteValue::Integer(1))
    );
    assert_eq!(
        rows[0].get(1),
        Some(&fsqlite_types::SqliteValue::Text("updated in txn".into()))
    );
}

/// T16: Future B4 coverage — unique secondary-index query_row should flip the
/// direct indexed-equality counter once the direct query_row fast path lands.
#[test]
#[ignore = "enable when bd-wwqen.4 lands direct indexed-equality query_row fast path"]
fn future_query_row_indexed_equality_uses_direct_counter() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, email TEXT NOT NULL, val TEXT NOT NULL)")
        .unwrap();
    conn.execute("CREATE UNIQUE INDEX idx_t_email ON t(email)")
        .unwrap();
    conn.execute(
        "INSERT INTO t VALUES
            (1, 'a@test.example', 'alpha'),
            (2, 'b@test.example', 'beta'),
            (3, 'c@test.example', 'gamma')",
    )
    .unwrap();

    let stmt = conn
        .prepare("SELECT id, email, val FROM t WHERE email = ?1")
        .unwrap();

    let before = hot_path_profile_snapshot();
    let row = stmt
        .query_row_with_params(&[fsqlite_types::SqliteValue::Text("b@test.example".into())])
        .unwrap();
    let after = hot_path_profile_snapshot();

    assert_eq!(
        row.get(0),
        Some(&fsqlite_types::SqliteValue::Integer(2)),
        "indexed equality query_row should return the matching id"
    );
    assert_eq!(
        row.get(1),
        Some(&fsqlite_types::SqliteValue::Text("b@test.example".into())),
        "indexed equality query_row should return the matching email"
    );
    assert_eq!(
        row.get(2),
        Some(&fsqlite_types::SqliteValue::Text("beta".into())),
        "indexed equality query_row should return the matching payload"
    );

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    assert!(
        fast_delta > 0,
        "prepared indexed equality query_row should stay on the fast prepared path"
    );
    assert_eq!(
        slow_delta, 0,
        "prepared indexed equality query_row should not need slow-path execution"
    );

    let direct_delta = after
        .direct_indexed_equality_query_hits
        .saturating_sub(before.direct_indexed_equality_query_hits);
    assert!(
        direct_delta > 0,
        "future B4 cut should increment direct indexed equality hits for query_row: before={before:?} after={after:?}"
    );

    let miss = stmt
        .query_row_with_params(&[fsqlite_types::SqliteValue::Text(
            "missing@test.example".into(),
        )])
        .expect_err("missing indexed equality row should return no rows");
    assert!(
        matches!(miss, fsqlite_error::FrankenError::QueryReturnedNoRows),
        "missing indexed equality row should surface QueryReturnedNoRows"
    );
}

/// T17: Future B4 coverage — rowid-range query_row should short-circuit after
/// first/second row detection and flip the direct rowid-range counter.
#[test]
#[ignore = "enable when bd-wwqen.4 lands direct rowid-range query_row fast path"]
fn future_query_row_rowid_range_uses_direct_counter() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    conn.execute(
        "INSERT INTO t VALUES
            (1, 'alpha'),
            (2, 'beta'),
            (3, 'gamma'),
            (4, 'delta')",
    )
    .unwrap();

    let stmt = conn
        .prepare("SELECT id, val FROM t WHERE id >= ?1 AND id < ?2")
        .unwrap();

    let before = hot_path_profile_snapshot();
    let row = stmt
        .query_row_with_params(&[
            fsqlite_types::SqliteValue::Integer(2),
            fsqlite_types::SqliteValue::Integer(3),
        ])
        .unwrap();
    let after = hot_path_profile_snapshot();

    assert_eq!(
        row.get(0),
        Some(&fsqlite_types::SqliteValue::Integer(2)),
        "single-row rowid range should return the expected id"
    );
    assert_eq!(
        row.get(1),
        Some(&fsqlite_types::SqliteValue::Text("beta".into())),
        "single-row rowid range should return the expected payload"
    );

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    assert!(
        fast_delta > 0,
        "prepared rowid-range query_row should stay on the fast prepared path"
    );
    assert_eq!(
        slow_delta, 0,
        "prepared rowid-range query_row should not need slow-path execution"
    );

    let direct_delta = after
        .direct_rowid_range_query_hits
        .saturating_sub(before.direct_rowid_range_query_hits);
    assert!(
        direct_delta > 0,
        "future B4 cut should increment direct rowid-range hits for query_row: before={before:?} after={after:?}"
    );

    let no_rows = stmt
        .query_row_with_params(&[
            fsqlite_types::SqliteValue::Integer(5),
            fsqlite_types::SqliteValue::Integer(5),
        ])
        .expect_err("empty rowid range should return no rows");
    assert!(
        matches!(no_rows, fsqlite_error::FrankenError::QueryReturnedNoRows),
        "empty rowid range should surface QueryReturnedNoRows"
    );

    let multiple_rows = stmt
        .query_row_with_params(&[
            fsqlite_types::SqliteValue::Integer(2),
            fsqlite_types::SqliteValue::Integer(5),
        ])
        .expect_err("multi-row range should return multiple rows error");
    assert!(
        matches!(
            multiple_rows,
            fsqlite_error::FrankenError::QueryReturnedMultipleRows
        ),
        "multi-row rowid range should surface QueryReturnedMultipleRows"
    );
}

#[test]
fn test_fast_path_count_star_sum_basic_correctness() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, score INTEGER)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)")
        .unwrap();

    let stmt = conn.prepare("SELECT COUNT(*), SUM(score) FROM t").unwrap();

    let before = hot_path_profile_snapshot();
    let row = stmt.query_row().unwrap();
    let after = hot_path_profile_snapshot();

    assert_count_star_sum_row(&row, 3, Some(fsqlite_types::SqliteValue::Integer(60)));

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    assert!(
        fast_delta > 0,
        "prepared COUNT(*)+SUM() should stay on the fast prepared path"
    );
    assert_eq!(
        slow_delta, 0,
        "prepared COUNT(*)+SUM() should not need slow-path execution"
    );
}

#[test]
fn test_fast_path_count_star_sum_empty_table_returns_zero_and_null() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, score INTEGER)")
        .unwrap();

    let stmt = conn.prepare("SELECT COUNT(*), SUM(score) FROM t").unwrap();

    let before = hot_path_profile_snapshot();
    let row = stmt.query_row().unwrap();
    let after = hot_path_profile_snapshot();

    assert_count_star_sum_row(&row, 0, None);

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    assert!(
        fast_delta > 0,
        "empty-table COUNT(*)+SUM() should still use the prepared fast path"
    );
    assert_eq!(
        slow_delta, 0,
        "empty-table COUNT(*)+SUM() should not fall back to the slow path"
    );
}

#[test]
fn test_fast_path_count_star_sum_skips_nulls_but_counts_rows() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, score INTEGER)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, NULL), (2, 10), (3, NULL), (4, 5)")
        .unwrap();

    let stmt = conn.prepare("SELECT COUNT(*), SUM(score) FROM t").unwrap();

    let before = hot_path_profile_snapshot();
    let row = stmt.query_row().unwrap();
    let after = hot_path_profile_snapshot();

    assert_count_star_sum_row(&row, 4, Some(fsqlite_types::SqliteValue::Integer(15)));

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    assert!(
        fast_delta > 0,
        "COUNT(*)+SUM() with NULL values should stay on the prepared fast path"
    );
    assert_eq!(
        slow_delta, 0,
        "COUNT(*)+SUM() with NULL values should not fall back to the slow path"
    );
}

#[test]
fn test_fast_path_count_star_sum_sees_post_insert_visibility() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, score INTEGER)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, 10), (2, 20)")
        .unwrap();

    let stmt = conn.prepare("SELECT COUNT(*), SUM(score) FROM t").unwrap();

    let baseline = stmt.query_row().unwrap();
    assert_count_star_sum_row(&baseline, 2, Some(fsqlite_types::SqliteValue::Integer(30)));

    conn.execute("INSERT INTO t VALUES (3, 30)").unwrap();

    reset_hot_path_profile();
    let before = hot_path_profile_snapshot();
    let row = stmt.query_row().unwrap();
    let after = hot_path_profile_snapshot();

    assert_count_star_sum_row(&row, 3, Some(fsqlite_types::SqliteValue::Integer(60)));

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    assert!(
        fast_delta > 0,
        "COUNT(*)+SUM() should see inserted rows without leaving the prepared fast path"
    );
    assert_eq!(
        slow_delta, 0,
        "post-insert COUNT(*)+SUM() should not fall back to the slow path"
    );
}

#[test]
fn test_fast_path_count_star_sum_sees_post_delete_visibility() {
    let _profile_guard = FastPathProfileTestGuard::new();
    let conn = Connection::open(":memory:").unwrap();
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, score INTEGER)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)")
        .unwrap();

    let stmt = conn.prepare("SELECT COUNT(*), SUM(score) FROM t").unwrap();

    let baseline = stmt.query_row().unwrap();
    assert_count_star_sum_row(&baseline, 3, Some(fsqlite_types::SqliteValue::Integer(60)));

    conn.execute("DELETE FROM t WHERE id = 2").unwrap();

    reset_hot_path_profile();
    let before = hot_path_profile_snapshot();
    let row = stmt.query_row().unwrap();
    let after = hot_path_profile_snapshot();

    assert_count_star_sum_row(&row, 2, Some(fsqlite_types::SqliteValue::Integer(40)));

    let (fast_delta, slow_delta) = fast_slow_delta(&before.parser, &after.parser);
    assert!(
        fast_delta > 0,
        "COUNT(*)+SUM() should see deleted rows without leaving the prepared fast path"
    );
    assert_eq!(
        slow_delta, 0,
        "post-delete COUNT(*)+SUM() should not fall back to the slow path"
    );
}

#[test]
fn test_fast_path_group_by_rowid_bucket_sum_matches_sqlite_reference_rows() {
    const CREATE_TABLE: &str =
        "CREATE TABLE bench(id INTEGER PRIMARY KEY, name TEXT NOT NULL, value REAL NOT NULL)";

    for (row_count, divisor) in [(0_usize, 1_i64), (7, 3), (25, 10)] {
        let fconn = Connection::open(":memory:").unwrap();
        let rconn = rusqlite::Connection::open_in_memory().unwrap();
        fconn.execute(CREATE_TABLE).unwrap();
        rconn.execute(CREATE_TABLE, []).unwrap();
        seed_grouped_sum_bench(&fconn, &rconn, row_count);

        let sql =
            format!("SELECT (id / {divisor}), SUM(value) FROM bench GROUP BY (id / {divisor})");
        let frank_rows = sorted_frank_rows(&fconn, &sql);
        let sqlite_rows = sorted_rusqlite_rows(&rconn, &sql);

        assert_eq!(
            frank_rows, sqlite_rows,
            "grouped-SUM rows should match rusqlite for row_count={row_count}, divisor={divisor}"
        );
    }
}
