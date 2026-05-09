//! Lock coverage for boot-time init uses a same-process held lock
//! intentionally: `fs2` file locks block a second acquisition in the same
//! process, so no cross-process helper is needed for this regression.

use std::fs::OpenOptions;
use std::time::{Duration, Instant};

use fs2::FileExt;
use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use tempfile::TempDir;

#[tokio::test]
async fn adapter_open_times_out_when_boot_init_lock_is_held() {
    let dir = TempDir::new().unwrap();
    let held_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join(".write.lock"))
        .expect("open write lock");
    held_lock
        .lock_exclusive()
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
