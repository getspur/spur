use crate::client::{PosthogClient, PosthogEvent};
use crate::redact::{classify_panic, payload_hash, scrub_stack, PanicType};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::backtrace::Backtrace;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;
use uuid::Uuid;

static INSTALL_ONCE: Once = Once::new();
static REPORT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CrashReport {
    panic_type: String,
    payload_hash: String,
    sanitized_stack: String,
    #[serde(rename = "crate")]
    crate_: Option<String>,
    module: Option<String>,
    line: Option<u32>,
}

pub(crate) fn install(anonymous_id: Uuid) {
    INSTALL_ONCE.call_once(|| {
        let prior = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            run_hook_chain(
                || write_crash_file(anonymous_id, panic_info),
                || prior(panic_info),
            );
        }));
    });
}

fn run_hook_chain<W, P>(write: W, prior: P)
where
    W: FnOnce(),
    P: FnOnce(),
{
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(write));
    prior();
}

fn write_crash_file(anonymous_id: Uuid, panic_info: &std::panic::PanicHookInfo<'_>) {
    let Some(path) = crash_report_path(anonymous_id) else {
        return;
    };

    let payload = panic_payload_to_string(panic_info.payload());
    let panic_type = panic_type_name(classify_panic(&payload)).to_string();
    let payload_hash = payload_hash(&payload, &anonymous_id.to_string());
    let sanitized_stack = scrub_stack(&Backtrace::force_capture().to_string());
    let (crate_, module, line) = panic_location_parts(
        panic_info.location().map(|loc| loc.file()),
        panic_info.location().map(|loc| loc.line()),
    );

    let report = CrashReport {
        panic_type,
        payload_hash,
        sanitized_stack,
        crate_,
        module,
        line,
    };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(encoded) = serde_json::to_vec(&report) {
        let mut tmp = path.clone();
        tmp.set_extension("json.tmp");
        if std::fs::write(&tmp, encoded).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

pub(crate) async fn upload_pending(client: &PosthogClient, anonymous_id: Uuid) -> usize {
    let Some(crash_dir) = crash_report_dir() else {
        return 0;
    };

    upload_pending_with_sender(&crash_dir, anonymous_id, |event| async move {
        client.send_batch(&[event]).await
    })
    .await
}

async fn upload_pending_with_sender<F, Fut>(dir: &Path, anonymous_id: Uuid, send: F) -> usize
where
    F: Fn(PosthogEvent) -> Fut,
    Fut: std::future::Future<Output = crate::Result<()>>,
{
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };

    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();

    let mut uploaded = 0usize;
    for path in paths {
        let Some(report) = read_crash_report(&path) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };

        let event = PosthogEvent {
            event: "$exception".to_string(),
            distinct_id: anonymous_id.to_string(),
            properties: json!({
                "panic_type": report.panic_type,
                "payload_hash": report.payload_hash,
                "sanitized_stack": report.sanitized_stack,
                "crate": report.crate_,
                "module": report.module,
                "line": report.line,
            }),
            timestamp: Utc::now(),
        };

        if send(event).await.is_ok() {
            let _ = std::fs::remove_file(&path);
            uploaded += 1;
        }
    }

    uploaded
}

fn read_crash_report(path: &Path) -> Option<CrashReport> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<CrashReport>(&bytes).ok()
}

fn panic_type_name(kind: PanicType) -> &'static str {
    match kind {
        PanicType::Bounds => "bounds",
        PanicType::Unwrap => "unwrap",
        PanicType::OptionUnwrap => "option_unwrap",
        PanicType::ResultUnwrap => "result_unwrap",
        PanicType::Assertion => "assertion",
        PanicType::Other => "other",
    }
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(msg) = payload.downcast_ref::<&'static str>() {
        return (*msg).to_string();
    }
    if let Some(msg) = payload.downcast_ref::<String>() {
        return msg.clone();
    }
    "non_string_panic_payload".to_string()
}

fn panic_location_parts(
    file: Option<&str>,
    line: Option<u32>,
) -> (Option<String>, Option<String>, Option<u32>) {
    let Some(file) = file else {
        return (None, None, line);
    };

    let normalized = file.replace('\\', "/");
    let crate_ = normalized
        .split("crates/")
        .nth(1)
        .and_then(|tail| tail.split('/').next())
        .map(ToString::to_string);

    let module = normalized
        .split("src/")
        .nth(1)
        .map(|tail| tail.trim_end_matches(".rs").replace('/', "::"));

    (crate_, module, line)
}

fn crash_report_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".spur").join("crash-reports"))
}

fn crash_report_path(anonymous_id: Uuid) -> Option<PathBuf> {
    let dir = crash_report_dir()?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let counter = REPORT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = format!("{anonymous_id}-{timestamp}-{counter}.json");
    Some(dir.join(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::process::Command;
    use tempfile::tempdir;

    const CHILD_MODE_ENV: &str = "SPUR_TELEMETRY_CRASH_CHILD_MODE";
    const CHILD_HOME_ENV: &str = "SPUR_TELEMETRY_CRASH_CHILD_HOME";
    const CHILD_ANON_ENV: &str = "SPUR_TELEMETRY_CRASH_CHILD_ANON_ID";
    #[tokio::test]
    async fn subprocess_roundtrip_panic_file_upload_delete_and_hook_chain() {
        if std::env::var(CHILD_MODE_ENV).ok().as_deref() == Some("panic") {
            run_panic_child();
            return;
        }

        let temp = tempdir().expect("tempdir");
        let anon = Uuid::new_v4();

        let mut child = Command::new(std::env::current_exe().expect("current_exe"));
        child
            .arg("--exact")
            .arg("crash::tests::subprocess_roundtrip_panic_file_upload_delete_and_hook_chain")
            .arg("--nocapture")
            .env(CHILD_MODE_ENV, "panic")
            .env(CHILD_HOME_ENV, temp.path())
            .env(CHILD_ANON_ENV, anon.to_string());

        let output = child.output().expect("spawn child");
        assert!(
            !output.status.success(),
            "panic child must exit non-zero: {output:?}"
        );

        let crash_dir = temp.path().join(".spur").join("crash-reports");
        let mut files = std::fs::read_dir(&crash_dir)
            .expect("crash report dir exists")
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        files.sort();
        assert_eq!(files.len(), 1, "expected one crash file");

        let marker = temp.path().join(".spur").join("prior-hook-ran");
        assert!(marker.exists(), "prior hook marker must exist");

        let report_bytes = std::fs::read(&files[0]).expect("read crash file");
        let report_json: Value = serde_json::from_slice(&report_bytes).expect("valid crash json");
        assert_eq!(
            report_json.get("crate"),
            Some(&Value::String("spur-telemetry".into()))
        );

        let uploaded = upload_pending_with_sender(&crash_dir, anon, |_| async { Ok(()) }).await;
        assert_eq!(uploaded, 1);
        let remaining = std::fs::read_dir(&crash_dir)
            .expect("read dir after upload")
            .flatten()
            .count();
        assert_eq!(remaining, 0, "uploaded crash file must be deleted");
    }

    fn run_panic_child() {
        let home = std::env::var(CHILD_HOME_ENV).expect("child home");
        let anon: Uuid = std::env::var(CHILD_ANON_ENV)
            .expect("child anon id")
            .parse()
            .expect("parse anon id");
        std::env::set_var("HOME", home);

        let prior_marker = PathBuf::from(std::env::var(CHILD_HOME_ENV).expect("child home"))
            .join(".spur")
            .join("prior-hook-ran");
        let prior = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = std::fs::create_dir_all(
                prior_marker
                    .parent()
                    .expect("prior marker parent must exist"),
            );
            let _ = std::fs::write(&prior_marker, "1");
            prior(info);
        }));

        install(anon);
        panic!("child panic for crash hook");
    }

    #[test]
    fn run_hook_chain_swallows_writer_panic_and_calls_prior() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_for_prior = called.clone();

        run_hook_chain(
            || panic!("writer panic"),
            || {
                called_for_prior.store(true, Ordering::SeqCst);
            },
        );

        assert!(called.load(Ordering::SeqCst), "prior hook must run");
    }

    #[tokio::test]
    async fn upload_pending_deletes_malformed_files() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path().join("crash-reports");
        std::fs::create_dir_all(&dir).expect("create crash dir");
        let malformed = dir.join("bad.json");
        std::fs::write(&malformed, "{ this is not json").expect("write malformed");

        let uploaded = upload_pending_with_sender(&dir, Uuid::new_v4(), |_| async { Ok(()) }).await;
        assert_eq!(uploaded, 0);
        assert!(!malformed.exists(), "malformed files must be deleted");
    }
}
