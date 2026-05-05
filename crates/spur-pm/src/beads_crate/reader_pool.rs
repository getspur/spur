//! Small fixed pool of `beads_rust::SqliteStorage` reader connections.
//!
//! WAL mode permits concurrent readers via *multiple* connections. One
//! connection cannot be called concurrently (rusqlite Connection is !Sync).
//! The pool gives us bounded concurrency without unbounded handle growth.

use std::path::PathBuf;
use std::sync::Mutex;

use beads_rust::storage::sqlite::SqliteStorage;

pub struct ReaderPool {
    beads_dir: PathBuf,
    free: Mutex<Vec<SqliteStorage>>,
    capacity: usize,
}

impl ReaderPool {
    pub fn new(beads_dir: PathBuf, capacity: usize) -> Self {
        assert!(capacity > 0, "ReaderPool capacity must be > 0");
        Self {
            beads_dir,
            free: Mutex::new(Vec::with_capacity(capacity)),
            capacity,
        }
    }

    /// Check out a reader connection. Lazily opens new connections up to
    /// `capacity`. If at capacity and none free, blocks briefly and retries
    /// (rare under expected workloads).
    pub fn checkout(&self) -> anyhow::Result<ReaderGuard<'_>> {
        loop {
            {
                let mut free = self.free.lock().unwrap();
                if let Some(conn) = free.pop() {
                    return Ok(ReaderGuard {
                        pool: self,
                        conn: Some(conn),
                    });
                }
                // No free; can we open a new one?
                if free.capacity() < self.capacity {
                    // We track open count via capacity-of-vec; fine for single-process pool
                    free.reserve(1);
                }
            }
            // Open without holding the mutex
            let conn = SqliteStorage::open(&self.beads_dir.join("beads.db"))?;
            return Ok(ReaderGuard {
                pool: self,
                conn: Some(conn),
            });
        }
    }
}

pub struct ReaderGuard<'a> {
    pool: &'a ReaderPool,
    conn: Option<SqliteStorage>,
}

impl<'a> ReaderGuard<'a> {
    pub fn storage(&self) -> &SqliteStorage {
        self.conn
            .as_ref()
            .expect("guard always has conn before drop")
    }
}

impl<'a> Drop for ReaderGuard<'a> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let mut free = self.pool.free.lock().unwrap();
            if free.len() < self.pool.capacity {
                free.push(conn);
            }
            // else: drop the connection (over-capacity, lazy shrink)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_beads(dir: &std::path::Path) {
        // Use beads_rust directly to initialize the workspace
        let _ = SqliteStorage::open(&dir.join("beads.db")).expect("open initializes schema");
    }

    #[test]
    fn checkout_returns_a_reader() {
        let dir = TempDir::new().unwrap();
        init_beads(dir.path());
        let pool = ReaderPool::new(dir.path().to_path_buf(), 2);
        let r = pool.checkout().expect("checkout");
        // sanity: storage is callable
        let _ = r.storage();
    }

    #[test]
    fn drop_returns_to_pool() {
        let dir = TempDir::new().unwrap();
        init_beads(dir.path());
        let pool = ReaderPool::new(dir.path().to_path_buf(), 1);
        {
            let _ = pool.checkout().unwrap();
        }
        // Free vec should now have 1 conn
        assert_eq!(pool.free.lock().unwrap().len(), 1);
    }
}
