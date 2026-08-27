#![recursion_limit = "256"]

use lambda_runtime::Error;
use spur_context_service::lambda::{self, BackendKind};

#[tokio::main]
async fn main() -> Result<(), Error> {
    lambda_runtime::run_concurrent(lambda_runtime::service_fn(|event| {
        lambda::handler_for(BackendKind::Code, event)
    }))
    .await
}
