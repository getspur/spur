//! Lock coverage for boot-time init uses a same-process held lock
//! intentionally: beads_rust's own `blocking_write_lock_with_timeout` tests
//! show that a held `std::fs::File` lock blocks a second acquisition in the
//! same process, so no cross-process helper is needed for this regression.

use std::time::{Duration, Instant};

use pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm as pm;
use tempfile::TempDir;

#[tokio::test]
async fn adapter_open_times_out_when_boot_init_lock_is_held() {
    let dir = TempDir::new().unwrap();
    let held_lock = beads_rust::sync::blocking_write_lock_with_timeout(dir.path(), Some(60_000))
        .expect("test should acquire write lock");

    let path = dir.path().to_path_buf();
    let start = Instant::now();
    let handle = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime")
            .block_on(BeadsCrateAdapter::open(
                &path,
                AdapterConfig {
                    lock_timeout_ms: 200,
                    ..Default::default()
                },
            ))
    });

    let err = match handle.join().expect("adapter thread should not panic") {
        Ok(_) => panic!("adapter open should time out behind held write lock"),
        Err(err) => err,
    };
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "adapter open should fail promptly, elapsed: {elapsed:?}"
    );
    assert!(
        err.to_string().contains("Timed out after 200ms"),
        "unexpected error: {err:#}"
    );

    drop(held_lock);
}
