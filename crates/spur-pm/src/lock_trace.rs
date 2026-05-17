use std::time::Instant;

#[derive(Clone, Copy)]
enum TraceTarget {
    Lock,
    Conn,
}

pub(crate) struct LockTraceGuard {
    target: TraceTarget,
    lock: &'static str,
    owner: &'static str,
    start: Instant,
}

impl LockTraceGuard {
    pub(crate) fn lock(lock: &'static str, owner: &'static str) -> Self {
        Self::new(TraceTarget::Lock, lock, owner)
    }

    pub(crate) fn conn(lock: &'static str, owner: &'static str) -> Self {
        Self::new(TraceTarget::Conn, lock, owner)
    }

    fn new(target: TraceTarget, lock: &'static str, owner: &'static str) -> Self {
        match target {
            TraceTarget::Lock => {
                tracing::info!(target: "spur.pm.lock", action = "acquire", lock, owner);
            }
            TraceTarget::Conn => {
                tracing::info!(target: "spur.pm.conn", action = "acquire", lock, owner);
            }
        }
        Self {
            target,
            lock,
            owner,
            start: Instant::now(),
        }
    }
}

impl Drop for LockTraceGuard {
    fn drop(&mut self) {
        let hold_ms = self.start.elapsed().as_millis() as u64;
        match self.target {
            TraceTarget::Lock => {
                tracing::info!(
                    target: "spur.pm.lock",
                    action = "release",
                    lock = self.lock,
                    owner = self.owner,
                    hold_ms
                );
            }
            TraceTarget::Conn => {
                tracing::info!(
                    target: "spur.pm.conn",
                    action = "release",
                    lock = self.lock,
                    owner = self.owner,
                    hold_ms
                );
            }
        }
    }
}
