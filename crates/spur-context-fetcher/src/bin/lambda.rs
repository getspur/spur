//! Lambda runtime entrypoint for the non-VPC SPUR context source fetcher.

use lambda_runtime::Error;

#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::args().any(|arg| arg == "--smoke") {
        println!("spur-context-fetcher-lambda smoke ok");
        return Ok(());
    }

    lambda_runtime::run(lambda_runtime::service_fn(spur_context_fetcher::handler)).await
}
