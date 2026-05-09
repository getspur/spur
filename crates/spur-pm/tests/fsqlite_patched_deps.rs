use beads_rust::model::{Issue, IssueType, Priority, Status};
use beads_rust::storage::sqlite::SqliteStorage;
use chrono::Utc;
use fsqlite::SqliteValue;
use rusqlite::ffi;
use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::path::Path;
use tempfile::TempDir;

fn make_issue(id: impl Into<String>, description: Option<String>) -> Issue {
    let id = id.into();
    let now = Utc::now();
    Issue {
        id: id.clone(),
        title: id,
        description,
        status: Status::Open,
        priority: Priority::MEDIUM,
        issue_type: IssueType::Task,
        created_at: now,
        updated_at: now,
        assignee: None,
        owner: None,
        estimated_minutes: None,
        due_at: None,
        defer_until: None,
        external_ref: None,
        ephemeral: false,
        content_hash: None,
        design: None,
        acceptance_criteria: None,
        notes: None,
        created_by: None,
        closed_at: None,
        close_reason: None,
        closed_by_session: None,
        source_system: None,
        source_repo: None,
        deleted_at: None,
        deleted_by: None,
        delete_reason: None,
        original_type: None,
        compaction_level: None,
        compacted_at: None,
        compacted_at_commit: None,
        original_size: None,
        sender: None,
        pinned: false,
        is_template: false,
        labels: Vec::new(),
        dependencies: Vec::new(),
        comments: Vec::new(),
    }
}

fn set_sqlite_reserved_bytes(conn: &rusqlite::Connection, reserved: c_int) {
    let main = CString::new("main").unwrap();
    let mut requested = reserved;
    let rc = unsafe {
        ffi::sqlite3_file_control(
            conn.handle(),
            main.as_ptr(),
            ffi::SQLITE_FCNTL_RESERVE_BYTES,
            (&mut requested as *mut c_int).cast::<c_void>(),
        )
    };
    assert_eq!(rc, ffi::SQLITE_OK, "SQLITE_FCNTL_RESERVE_BYTES failed");
}

fn create_reserved_overflow_fixture(path: &Path) {
    let conn = rusqlite::Connection::open(path).expect("open rusqlite fixture");
    conn.pragma_update(None, "page_size", 4096)
        .expect("set page size");
    set_sqlite_reserved_bytes(&conn, 12);
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE; CREATE TABLE t(id INTEGER PRIMARY KEY, body TEXT);",
    )
    .expect("create fixture table");
    let body = "reserved-overflow ".repeat(600);
    conn.execute("INSERT INTO t(body) VALUES (?1)", [&body])
        .expect("insert overflow row");
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("run integrity_check");
    assert_eq!(integrity, "ok");
    drop(conn);

    let header = std::fs::read(path).expect("read fixture header");
    assert_eq!(header[20], 12, "fixture must use reserved bytes");
}

#[test]
fn fsqlite_reads_reserved_byte_overflow_payloads() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("reserved.db");
    create_reserved_overflow_fixture(&db_path);

    let conn = fsqlite::Connection::open(db_path.to_string_lossy().into_owned())
        .expect("fsqlite opens reserved-byte fixture");
    let row = conn
        .query_row("SELECT count(*), length(body) FROM t")
        .expect("fsqlite reads overflow payload from reserved-byte fixture");
    assert_eq!(row.values()[0], SqliteValue::Integer(1));
    assert_eq!(
        row.values()[1],
        SqliteValue::Integer(i64::try_from("reserved-overflow ".repeat(600).len()).unwrap())
    );
}

#[test]
fn beads_large_issue_writes_leave_sqlite_integrity_ok() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("beads.db");
    {
        let mut storage = SqliteStorage::open(&db_path).expect("open beads storage");
        for i in 0..80 {
            let description = Some(format!(
                "large issue {i}\n{}",
                "payload that forces overflow pages ".repeat(180)
            ));
            storage
                .create_issue(&make_issue(format!("bd-patch-{i:03}"), description), "test")
                .expect("create large issue");
        }
    }

    let conn = rusqlite::Connection::open(&db_path).expect("open with rusqlite");
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("run integrity_check");
    assert_eq!(integrity, "ok");
}
