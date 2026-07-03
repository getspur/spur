use std::path::PathBuf;

use anyhow::Result;

pub async fn serve(socket: Option<PathBuf>) -> Result<()> {
    spur_analyst::embed_service::serve(socket).await
}
