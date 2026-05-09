//! Conformance oracle tests — Session 74b
//!
//! GROUP BY + window function coverage (leveraging the execute_group_by_window_select fix),
//! complex aggregates, CASE expressions, recursive CTEs, NULL functions, WHERE patterns,
//! string functions, and UPSERT conflict handling.

use fsqlite_core::connection::Connection;
use fsqlite_types::value::SqliteValue;

fn oracle_compare(
    fconn: &Connection,
    rconn: &rusqlite::Connection,
    queries: &[&str],
) -> Vec<String> {
    let mut mismatches = Vec::new();
    for query in queries {
        let frank_result = fconn.query(query);
        let csql_result: std::result::Result<Vec<Vec<String>>, String> = (|| {
            let mut stmt = rconn.prepare(query).map_err(|e| format!("prepare: {e}"))?;
            let col_count = stmt.column_count();
            let rows: Vec<Vec<String>> = stmt
                .query_map([], |row| {
                    let mut vals = Vec::new();
                    for i in 0..col_count {
                        let v: rusqlite::types::Value = row.get_unwrap(i);
                        let s = match v {
                            rusqlite::types::Value::Null => "NULL".to_owned(),
                            rusqlite::types::Value::Integer(n) => n.to_string(),
                            rusqlite::types::Value::Real(f) => format!("{f}"),
                            rusqlite::types::Value::Text(s) => format!("'{s}'"),
                            rusqlite::types::Value::Blob(b) => format!(
                                "X'{}'",
                                b.iter().map(|x| format!("{x:02X}")).collect::<String>()
                            ),
                        };
                        vals.push(s);
                    }
                    Ok(vals)
                })
                .map_err(|e| format!("query: {e}"))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| format!("row: {e}"))?;
            Ok(rows)
        })();
        let csql_result = match csql_result {
            Ok(r) => r,
            Err(csql_err) => {
                if frank_result.is_ok() {
                    mismatches.push(format!(
                        "DIVERGE: {query}\n  frank: OK\n  csql:  ERROR({csql_err})"
                    ));
                }
                continue;
            }
        };
        match frank_result {
            Ok(rows) => {
                let frank_strs: Vec<Vec<String>> = rows
                    .iter()
                    .map(|row| {
                        row.values()
                            .iter()
                            .map(|v| match v {
                                SqliteValue::Null => "NULL".to_owned(),
                                SqliteValue::Integer(n) => n.to_string(),
                                SqliteValue::Float(f) => format!("{f}"),
                                SqliteValue::Text(s) => format!("'{s}'"),
                                SqliteValue::Blob(b) => format!(
                                    "X'{}'",
                                    b.iter().map(|x| format!("{x:02X}")).collect::<String>()
                                ),
                            })
                            .collect()
                    })
                    .collect();
                if frank_strs != csql_result {
                    mismatches.push(format!(
                        "MISMATCH: {query}\n  frank: {frank_strs:?}\n  csql:  {csql_result:?}"
                    ));
                }
            }
            Err(e) => {
                mismatches.push(format!(
                    "FRANK_ERR: {query}\n  frank: {e}\n  csql:  {csql_result:?}"
                ));
            }
        }
    }
    mismatches
}

#[test]
fn test_conformance_group_by_rank_window_s74aa() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &[
        "CREATE TABLE s74aa_t(id INTEGER PRIMARY KEY, grp TEXT, val INTEGER)",
        "INSERT INTO s74aa_t VALUES(1,'A',10),(2,'A',20),(3,'B',30),(4,'C',15),(5,'C',25),(6,'D',5)",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    let queries = &[
        "SELECT grp, SUM(val), RANK() OVER (ORDER BY SUM(val) DESC) as rnk FROM s74aa_t GROUP BY grp ORDER BY rnk, grp",
        "SELECT grp, AVG(val), DENSE_RANK() OVER (ORDER BY AVG(val)) as dr FROM s74aa_t GROUP BY grp ORDER BY dr, grp",
        "SELECT grp, MAX(val), ROW_NUMBER() OVER (ORDER BY MAX(val) DESC, grp) as rn FROM s74aa_t GROUP BY grp ORDER BY rn",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74aa mismatches", mismatches.len());
    }
}

#[test]
fn test_conformance_case_complex_s74ab() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &[
        "CREATE TABLE s74ab_t(id INTEGER PRIMARY KEY, name TEXT, score INTEGER, status TEXT)",
        "INSERT INTO s74ab_t VALUES(1,'Alice',95,'active'),(2,'Bob',60,'inactive'),(3,'Carol',80,'active'),(4,'Dave',45,'active'),(5,'Eve',70,'inactive')",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    let queries = &[
        "SELECT name, CASE WHEN score >= 90 THEN 'A' WHEN score >= 80 THEN 'B' WHEN score >= 70 THEN 'C' ELSE 'F' END as grade FROM s74ab_t ORDER BY name",
        "SELECT status, COUNT(*), AVG(score), CASE WHEN AVG(score) > 70 THEN 'good' ELSE 'needs work' END as assessment FROM s74ab_t GROUP BY status ORDER BY status",
        "SELECT name, IIF(score > 75, 'pass', 'fail') as result FROM s74ab_t ORDER BY name",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74ab mismatches", mismatches.len());
    }
}

#[test]
fn test_conformance_recursive_cte_agg_s74ac() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &[
        "CREATE TABLE s74ac_tree(id INTEGER PRIMARY KEY, pid INTEGER, name TEXT, val INTEGER)",
        "INSERT INTO s74ac_tree VALUES(1,NULL,'root',100),(2,1,'c1',50),(3,1,'c2',30),(4,2,'gc1',20),(5,2,'gc2',10),(6,3,'gc3',15)",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    let queries = &[
        "WITH RECURSIVE tp AS (SELECT id, name, pid, 0 as depth FROM s74ac_tree WHERE pid IS NULL UNION ALL SELECT t.id, t.name, t.pid, tp.depth+1 FROM s74ac_tree t JOIN tp ON t.pid=tp.id) SELECT name, depth FROM tp ORDER BY depth, name",
        "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n < 5) SELECT n, n*n as sq FROM c",
        "SELECT pid, COUNT(*) as ch, SUM(val) as tot FROM s74ac_tree WHERE pid IS NOT NULL GROUP BY pid ORDER BY pid",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74ac mismatches", mismatches.len());
    }
}

#[test]
fn test_conformance_null_functions_s74ad() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &[
        "CREATE TABLE s74ad_t(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, c TEXT)",
        "INSERT INTO s74ad_t VALUES(1,10,NULL,'x'),(2,NULL,20,'y'),(3,30,30,NULL),(4,NULL,NULL,'z')",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    let queries = &[
        "SELECT id, COALESCE(a, b, 0) as val FROM s74ad_t ORDER BY id",
        "SELECT id, NULLIF(a, b) as diff FROM s74ad_t ORDER BY id",
        "SELECT id, IIF(a IS NOT NULL, a, IIF(b IS NOT NULL, b, -1)) as resolved FROM s74ad_t ORDER BY id",
        "SELECT id, COALESCE(c, 'missing') as lbl, COALESCE(a, 0) + COALESCE(b, 0) as tot FROM s74ad_t ORDER BY id",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74ad mismatches", mismatches.len());
    }
}

#[test]
fn test_conformance_complex_where_s74af() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &[
        "CREATE TABLE s74af_t(id INTEGER PRIMARY KEY, x INTEGER, y REAL, z TEXT)",
        "INSERT INTO s74af_t VALUES(1,10,1.5,'alpha'),(2,20,2.5,'beta'),(3,30,3.5,'gamma'),(4,40,4.5,'delta'),(5,50,5.5,'epsilon'),(6,NULL,NULL,NULL)",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    let queries = &[
        "SELECT * FROM s74af_t WHERE x > 15 AND y < 5.0 ORDER BY id",
        "SELECT * FROM s74af_t WHERE x BETWEEN 20 AND 40 ORDER BY id",
        "SELECT * FROM s74af_t WHERE z LIKE '%a%' ORDER BY id",
        "SELECT * FROM s74af_t WHERE x IS NULL OR y IS NULL ORDER BY id",
        "SELECT * FROM s74af_t WHERE x IN (10, 30, 50) ORDER BY id",
        "SELECT * FROM s74af_t WHERE NOT (x > 25) AND x IS NOT NULL ORDER BY id",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74af mismatches", mismatches.len());
    }
}

#[test]
fn test_conformance_aggregate_edges_s74ag() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &[
        "CREATE TABLE s74ag_t(id INTEGER PRIMARY KEY, grp TEXT, val INTEGER)",
        "INSERT INTO s74ag_t VALUES(1,'A',10),(2,'A',10),(3,'A',20),(4,'B',NULL),(5,'B',30),(6,'C',40),(7,'C',40),(8,'C',40)",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    let queries = &[
        "SELECT grp, COUNT(val), COUNT(DISTINCT val), SUM(val), SUM(DISTINCT val) FROM s74ag_t GROUP BY grp ORDER BY grp",
        "SELECT grp, AVG(val), MIN(val), MAX(val), total(val) FROM s74ag_t GROUP BY grp ORDER BY grp",
        "SELECT COUNT(*), COUNT(val), SUM(val), AVG(val) FROM s74ag_t",
        "SELECT grp, GROUP_CONCAT(val, ',') FROM s74ag_t GROUP BY grp ORDER BY grp",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74ag mismatches", mismatches.len());
    }
}

#[test]
fn test_conformance_string_functions_s74ai() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    let queries = &[
        "SELECT length('hello'), length('')",
        "SELECT upper('hello'), lower('WORLD')",
        "SELECT substr('abcdef', 2, 3)",
        "SELECT replace('hello world', 'world', 'rust')",
        "SELECT trim('  hello  '), ltrim('  hello'), rtrim('hello  ')",
        "SELECT instr('hello world', 'world')",
        "SELECT 'hello' || ' ' || 'world'",
        "SELECT typeof(NULL), typeof(42), typeof(3.14), typeof('text'), typeof(X'00')",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74ai mismatches", mismatches.len());
    }
}

#[test]
fn test_conformance_upsert_conflict_s74aj() {
    let fconn = Connection::open(":memory:").unwrap();
    let rconn = rusqlite::Connection::open_in_memory().unwrap();
    for s in &[
        "CREATE TABLE s74aj_t(id INTEGER PRIMARY KEY, name TEXT UNIQUE, val INTEGER DEFAULT 0)",
        "INSERT INTO s74aj_t VALUES(1,'Alice',10),(2,'Bob',20)",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    for s in &[
        "INSERT OR REPLACE INTO s74aj_t VALUES(1,'Alice',100)",
        "INSERT OR IGNORE INTO s74aj_t VALUES(2,'Bob',200)",
        "INSERT OR REPLACE INTO s74aj_t VALUES(3,'Carol',30)",
    ] {
        fconn.execute(s).unwrap();
        rconn.execute_batch(s).unwrap();
    }
    let queries = &[
        "SELECT * FROM s74aj_t ORDER BY id",
        "SELECT COUNT(*) FROM s74aj_t",
    ];
    let mismatches = oracle_compare(&fconn, &rconn, queries);
    if !mismatches.is_empty() {
        for m in &mismatches {
            eprintln!("{m}\n");
        }
        panic!("{} s74aj mismatches", mismatches.len());
    }
}
