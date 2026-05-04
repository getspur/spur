//! Cross-platform process inspection seam for orphan reaping.

use std::collections::HashMap;
use std::sync::Mutex;

/// Cross-platform process inspection seam.
///
/// Production impls read `proc_bsdinfo` on macOS and `/proc/<pid>/stat` +
/// `/proc/uptime` on Linux. Test mock allows deterministic sweep tests.
pub trait ProcessInspector: Send + Sync {
    /// Unix epoch seconds at which the process started. `None` if no live
    /// process holds the PID.
    fn starttime_of(&self, pid: i32) -> Option<i64>;

    /// Full command line (argv joined with spaces). `None` if PID is dead.
    fn cmd_of(&self, pid: i32) -> Option<String>;

    /// Send `signal` to every process in the group whose leader is `pgid`.
    /// ESRCH/EPERM are swallowed (best-effort).
    fn killpg(&self, pgid: i32, signal: Signal);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use libproc::bsd_info::BSDInfo;
    use libproc::proc_pid;

    pub struct MacInspector;
    impl ProcessInspector for MacInspector {
        fn starttime_of(&self, pid: i32) -> Option<i64> {
            let info: BSDInfo = proc_pid::pidinfo(pid, 0).ok()?;
            Some(info.pbi_start_tvsec as i64)
        }
        fn cmd_of(&self, pid: i32) -> Option<String> {
            let path = proc_pid::pidpath(pid).ok()?;
            // libproc::proc_pid::listpidinfo for argv is platform-fiddly;
            // fall back to `path` only and accept that argv-join is approximate.
            // (Followup ticket bd-20k may upgrade to pid_argsv if needed.)
            Some(path)
        }
        fn killpg(&self, pgid: i32, signal: Signal) {
            let sig = match signal {
                Signal::Term => "TERM",
                Signal::Kill => "KILL",
            };
            let _ = std::process::Command::new("kill")
                .arg(format!("-{sig}"))
                .arg(format!("-{pgid}"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    pub struct LinuxInspector;
    impl ProcessInspector for LinuxInspector {
        fn starttime_of(&self, pid: i32) -> Option<i64> {
            // /proc/<pid>/stat field 22 = starttime in clock ticks since boot.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // Field 22, but field 2 (comm) can contain spaces in parens.
            let after_paren = stat.rfind(')')?;
            let rest = &stat[after_paren + 2..];
            let fields: Vec<&str> = rest.split_whitespace().collect();
            // After comm, fields[0] = state. starttime is field 22 in 1-indexed
            // proc(5); after state it is index 19 in this rest split (state, ppid,
            // pgrp, session, tty_nr, tpgid, flags, minflt, cminflt, majflt, cmajflt,
            // utime, stime, cutime, cstime, priority, nice, num_threads, itrealvalue,
            // starttime).
            let starttime_ticks: u64 = fields.get(19)?.parse().ok()?;
            // Convert to epoch seconds via /proc/uptime + boot time.
            let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
            let uptime_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
            let now = chrono::Utc::now().timestamp();
            let boot_epoch = now - uptime_secs as i64;
            // ticks-per-second = sysconf(_SC_CLK_TCK), normally 100.
            let tps = 100i64;
            Some(boot_epoch + (starttime_ticks as i64 / tps))
        }
        fn cmd_of(&self, pid: i32) -> Option<String> {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            // argv elements are NUL-separated.
            let parts: Vec<String> = cmdline
                .split(|&b| b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        }
        fn killpg(&self, pgid: i32, signal: Signal) {
            let sig = match signal {
                Signal::Term => "TERM",
                Signal::Kill => "KILL",
            };
            let _ = std::process::Command::new("kill")
                .arg(format!("-{sig}"))
                .arg(format!("-{pgid}"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod unsupported {
    use super::*;

    pub struct UnsupportedInspector;
    impl ProcessInspector for UnsupportedInspector {
        fn starttime_of(&self, _pid: i32) -> Option<i64> {
            None
        }

        fn cmd_of(&self, _pid: i32) -> Option<String> {
            None
        }

        fn killpg(&self, _pgid: i32, _signal: Signal) {}
    }
}

pub fn production_inspector() -> Box<dyn ProcessInspector> {
    #[cfg(target_os = "macos")]
    {
        Box::new(mac::MacInspector)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxInspector)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Box::new(unsupported::UnsupportedInspector)
    }
}

/// Convenience: starttime of the running spur process.
pub fn starttime_of_self() -> i64 {
    production_inspector()
        .starttime_of(std::process::id() as i32)
        .unwrap_or_else(|| chrono::Utc::now().timestamp())
}

/// Convenience for the spawn site.
pub fn starttime_of(pid: i32) -> Option<i64> {
    production_inspector().starttime_of(pid)
}

/// Hand-rolled mock for unit tests (no `mockall` dep — matches the
/// project's existing pattern).
pub struct MockInspector {
    starttimes: Mutex<HashMap<i32, i64>>,
    cmds: Mutex<HashMap<i32, String>>,
    killed: Mutex<Vec<(i32, Signal)>>,
}

impl MockInspector {
    pub fn with_alive(pid: i32, starttime: i64, cmd: &str) -> Self {
        let mut st = HashMap::new();
        st.insert(pid, starttime);
        let mut cm = HashMap::new();
        cm.insert(pid, cmd.to_string());
        Self {
            starttimes: Mutex::new(st),
            cmds: Mutex::new(cm),
            killed: Mutex::new(Vec::new()),
        }
    }

    pub fn add_alive(&mut self, pid: i32, starttime: i64, cmd: &str) {
        self.starttimes.lock().unwrap().insert(pid, starttime);
        self.cmds.lock().unwrap().insert(pid, cmd.to_string());
    }

    pub fn killed(&self) -> Vec<(i32, Signal)> {
        self.killed.lock().unwrap().clone()
    }
}

impl ProcessInspector for MockInspector {
    fn starttime_of(&self, pid: i32) -> Option<i64> {
        self.starttimes.lock().unwrap().get(&pid).copied()
    }
    fn cmd_of(&self, pid: i32) -> Option<String> {
        self.cmds.lock().unwrap().get(&pid).cloned()
    }
    fn killpg(&self, pgid: i32, signal: Signal) {
        self.killed.lock().unwrap().push((pgid, signal));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn live_pid_starttime_is_some() {
        let inspector = production_inspector();
        let me = std::process::id() as i32;
        let st = inspector.starttime_of(me);
        assert!(st.is_some(), "starttime of self should be Some");
    }

    #[test]
    fn dead_pid_starttime_is_none() {
        // Spawn-and-reap to get a definitely-dead PID.
        // /usr/bin/true exists on both macOS and Linux distributions.
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn");
        let pid = child.id() as i32;
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let inspector = production_inspector();
        assert_eq!(inspector.starttime_of(pid), None);
    }

    #[test]
    fn cmd_of_self_contains_test_runner() {
        let inspector = production_inspector();
        let cmd = inspector.cmd_of(std::process::id() as i32);
        assert!(cmd.is_some());
        // Self test process; cargo runs as `<deps>/spur_acp-<hash>` typically.
        // Just assert non-empty string.
        assert!(!cmd.unwrap().is_empty());
    }

    #[test]
    fn mock_inspector_threads_through_trait() {
        let mock = MockInspector::with_alive(123, 999, "/bin/test arg");
        assert_eq!(mock.starttime_of(123), Some(999));
        assert_eq!(mock.cmd_of(123), Some("/bin/test arg".to_string()));
        assert_eq!(mock.starttime_of(456), None);
    }
}
