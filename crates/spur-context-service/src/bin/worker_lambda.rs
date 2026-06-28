//! Lambda worker entry point for low-latency context indexing.

#![recursion_limit = "256"]

use anyhow::Context as _;
use lambda_runtime::{Error, LambdaEvent};
use serde::Deserialize;
use serde_json::{json, Value};
use spur_context_service::worker::{run_job_and_record, JobEnv, JobFromLayer};

#[derive(Debug, Deserialize)]
struct LambdaWorkerEvent {
    job_id: String,
    package: String,
    revision: String,
    source: String,
    source_url: String,
    source_kind: String,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::args().any(|arg| arg == "--smoke") {
        println!("spur-context-worker-lambda smoke ok");
        return Ok(());
    }

    if std::env::var("HOME").is_err() {
        std::env::set_var("HOME", "/tmp");
    }

    lambda_runtime::run(lambda_runtime::service_fn(handler)).await
}

async fn handler(event: LambdaEvent<LambdaWorkerEvent>) -> Result<Value, Error> {
    let payload = event.payload;
    let catalog_dsn = std::env::var("SPUR_CATALOG_DSN")
        .or_else(|_| std::env::var("SPUR_CATALOG_S3_URI"))
        .context("SPUR_CATALOG_DSN or SPUR_CATALOG_S3_URI must be set")?;

    let env = JobEnv {
        task_token: String::new(),
        job_id: payload.job_id,
        package: payload.package,
        revision: payload.revision,
        source: payload.source,
        source_url: payload.source_url,
        source_kind: payload.source_kind,
        catalog_dsn,
        from_layer: JobFromLayer::Source,
    };

    match run_job_and_record(&env).await {
        Ok(stats) => Ok(json!({
            "status": "complete",
            "snapshot_id": stats.snapshot_id,
            "rows_inserted": stats.rows_inserted,
        })),
        Err(error) => Ok(json!({
            "status": "failed",
            "error": worker_error_code(&error),
            "cause": format!("{error:#}"),
        })),
    }
}

fn worker_error_code(error: &spur_context_service::worker::WorkerError) -> &'static str {
    match error {
        spur_context_service::worker::WorkerError::Fetch(_) => "fetch",
        spur_context_service::worker::WorkerError::Build(_) => "build",
        spur_context_service::worker::WorkerError::Translate(_) => "commit",
        spur_context_service::worker::WorkerError::SpotInterrupted => "spot_interrupted",
        spur_context_service::worker::WorkerError::SfnSend(_) => "sfn_send",
    }
}
