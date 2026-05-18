use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

pub(crate) fn spawn_blocking_pool_probe() -> JoinHandle<()> {
    spawn_probe(Duration::from_secs(1), |latency| {
        tracing::info!(
            target: "spur.pm.blocking_pool",
            latency_ms = latency.as_millis() as u64,
            "blocking-pool RTT"
        );
    })
}

fn spawn_probe<F>(interval: Duration, reporter: F) -> JoinHandle<()>
where
    F: Fn(Duration) + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let start = Instant::now();
            if tokio::task::spawn_blocking(|| {}).await.is_err() {
                break;
            }
            reporter(start.elapsed());
            tokio::time::sleep(interval).await;
        }
    })
}

#[cfg(test)]
fn spawn_probe_for_test<F>(interval: Duration, reporter: F) -> JoinHandle<()>
where
    F: Fn(Duration) + Send + 'static,
{
    spawn_probe(interval, reporter)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn probe_reports_blocking_pool_starvation_and_recovery() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let probe = super::spawn_probe_for_test(Duration::from_millis(10), move |latency| {
                let _ = tx.send(latency);
            });

            let blocker =
                tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(160)));

            let mut saw_starved = false;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            while tokio::time::Instant::now() < deadline {
                let Some(latency) = rx.recv().await else {
                    break;
                };
                if latency >= Duration::from_millis(100) {
                    saw_starved = true;
                    break;
                }
            }
            assert!(saw_starved, "probe should report queued blocking-pool RTT");

            blocker.await.unwrap();

            let mut saw_recovered = false;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            while tokio::time::Instant::now() < deadline {
                let Some(latency) = rx.recv().await else {
                    break;
                };
                if latency < Duration::from_millis(50) {
                    saw_recovered = true;
                    break;
                }
            }

            probe.abort();
            assert!(
                saw_recovered,
                "probe should recover after blocking load drops"
            );
        });
    }
}
