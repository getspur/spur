//! Orphan tree sweeper: walks .spur/pgids/, kills stale trees safely.

use crate::orphan_registry::{PgidRecord, PgidRegistry};
use crate::process_inspector::{ProcessInspector, Signal};
use std::path::Path;
use std::time::Duration;

pub struct OrphanSweeper {
    registry: PgidRegistry,
    inspector: Box<dyn ProcessInspector>,
    grace_period: Duration,
}

#[derive(Debug, Default)]
pub struct SweepReport {
    pub killed: Vec<PgidRecord>,
    pub skipped_alive_owner: usize,
    pub skipped_recycled: usize,
    pub unparseable: usize,
    pub signals_sent: Vec<(i32, Signal)>,
}

impl OrphanSweeper {
    pub fn new(pgids_dir: impl AsRef<Path>, inspector: Box<dyn ProcessInspector>) -> Self {
        Self {
            registry: PgidRegistry::new(pgids_dir.as_ref()),
            inspector,
            grace_period: Duration::from_millis(250),
        }
    }

    pub fn run(&self) -> SweepReport {
        let mut report = SweepReport::default();
        let records = match self.registry.load_all() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "orphan_sweeper: registry load failed");
                return report;
            }
        };

        for rec in records {
            // 1. Is owning spur alive?
            match self.inspector.starttime_of(rec.spur_pid) {
                Some(st) if st == rec.spur_pid_start_time => {
                    report.skipped_alive_owner += 1;
                    continue;
                }
                _ => {} // dead OR recycled → fall through
            }

            // 2. Is the recorded pgid leader still the same process?
            let leader_now = self.inspector.starttime_of(rec.pgid);
            let leader_cmd = self.inspector.cmd_of(rec.pgid);
            if leader_now != Some(rec.pgid_leader_start_time)
                || leader_cmd.as_deref() != Some(rec.cmd.as_str())
            {
                report.skipped_recycled += 1;
                let _ = self.registry.delete(rec.pgid);
                continue;
            }

            // 3. Reap.
            self.inspector.killpg(rec.pgid, Signal::Term);
            report.signals_sent.push((rec.pgid, Signal::Term));
            std::thread::sleep(self.grace_period);
            self.inspector.killpg(rec.pgid, Signal::Kill);
            report.signals_sent.push((rec.pgid, Signal::Kill));
            let _ = self.registry.delete(rec.pgid);
            tracing::warn!(
                agent = %rec.agent_name,
                pgid = rec.pgid,
                age_secs = chrono::Utc::now().timestamp() - rec.spawned_at,
                "orphan_sweeper: reaped stale agent tree"
            );
            report.killed.push(rec);
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orphan_registry::{PgidRecord, PgidRegistry};
    use crate::process_inspector::{MockInspector, Signal};
    use tempfile::tempdir;

    fn make_record(pgid: i32, spur_pid: i32, st: i64, cmd: &str) -> PgidRecord {
        PgidRecord {
            spur_pid,
            spur_pid_start_time: st,
            agent_name: "test".into(),
            cmd: cmd.into(),
            pgid,
            pgid_leader_start_time: st,
            spawned_at: 0,
        }
    }

    #[test]
    fn owner_alive_skip_no_kill() {
        let dir = tempdir().unwrap();
        let pgids = dir.path().join("pgids");
        let registry = PgidRegistry::new(&pgids);
        registry
            .write(&make_record(8001, 1234, 555, "/bin/test"))
            .unwrap();

        // Owner (1234) is alive with matching start_time; pgid leader (8001)
        // also alive with matching start_time + cmd.
        let mut inspector = MockInspector::with_alive(1234, 555, "/proc/spur");
        inspector.add_alive(8001, 555, "/bin/test");

        let report = OrphanSweeper::new(&pgids, Box::new(inspector)).run();
        assert_eq!(report.killed.len(), 0);
        assert_eq!(report.skipped_alive_owner, 1);
    }

    #[test]
    fn owner_dead_pgid_match_then_kill_term_then_kill() {
        let dir = tempdir().unwrap();
        let pgids = dir.path().join("pgids");
        let registry = PgidRegistry::new(&pgids);
        registry
            .write(&make_record(8002, 1234, 555, "/bin/test"))
            .unwrap();

        // Owner 1234 is dead (not in inspector). Pgid leader 8002 alive
        // and identity matches.
        let inspector = MockInspector::with_alive(8002, 555, "/bin/test");

        let report = OrphanSweeper::new(&pgids, Box::new(inspector)).run();
        assert_eq!(report.killed.len(), 1);
        assert_eq!(report.killed[0].pgid, 8002);
        assert!(matches!(
            report.signals_sent[..],
            [(8002, Signal::Term), (8002, Signal::Kill)]
        ));
        // .toml record removed.
        assert_eq!(registry.load_all().unwrap().len(), 0);
    }

    #[test]
    fn pgid_recycled_drops_record_no_kill() {
        let dir = tempdir().unwrap();
        let pgids = dir.path().join("pgids");
        let registry = PgidRegistry::new(&pgids);
        registry
            .write(&make_record(8003, 1234, 555, "/bin/old"))
            .unwrap();

        // Owner dead. Pgid 8003 still alive but has different cmd (recycled).
        let inspector = MockInspector::with_alive(8003, 999, "/bin/different");

        let report = OrphanSweeper::new(&pgids, Box::new(inspector)).run();
        assert_eq!(report.killed.len(), 0);
        assert_eq!(report.skipped_recycled, 1);
        // Record should be cleaned up to avoid future false-positives.
        assert_eq!(registry.load_all().unwrap().len(), 0);
    }
}
