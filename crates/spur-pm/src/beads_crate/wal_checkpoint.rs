use std::path::Path;

pub(crate) fn checkpoint_wal_truncate_best_effort(db_path: &Path) {
    let Ok(conn) = rusqlite::Connection::open(db_path) else {
        tracing::debug!(db_path = %db_path.display(), "failed to open database for WAL checkpoint");
        return;
    };

    let _ = conn.busy_timeout(std::time::Duration::ZERO);
    match wal_checkpoint(&conn, "TRUNCATE") {
        Ok((busy, _, _)) if busy == 0 => {}
        Ok(_) => {
            tracing::debug!("WAL TRUNCATE checkpoint busy; falling back to PASSIVE");
            let _ = wal_checkpoint(&conn, "PASSIVE");
        }
        Err(err) => {
            tracing::debug!(
                error = %err,
                "WAL TRUNCATE checkpoint failed; falling back to PASSIVE"
            );
            let _ = wal_checkpoint(&conn, "PASSIVE");
        }
    }
}

fn wal_checkpoint(
    conn: &rusqlite::Connection,
    mode: &'static str,
) -> rusqlite::Result<(i64, i64, i64)> {
    conn.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn truncate_checkpoint_drains_wal_sidecar() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("beads.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)", [])
            .unwrap();
        conn.execute("INSERT INTO items (name) VALUES ('checkpoint-me')", [])
            .unwrap();
        drop(conn);

        checkpoint_wal_truncate_best_effort(&db_path);

        let wal_path = std::path::PathBuf::from(format!("{}-wal", db_path.to_string_lossy()));
        let wal_len = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
        assert!(
            wal_len <= 4096,
            "expected WAL checkpoint to drain sidecar, got {wal_len} bytes"
        );
    }
}
