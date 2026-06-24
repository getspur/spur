//! Worker binary stub for on-demand context indexing.

#[tokio::main]
async fn main() {
    if let Err(error) = spur_context_service::worker::run_from_env().await {
        eprintln!("[worker] {error}");
        std::process::exit(1);
    }
}
