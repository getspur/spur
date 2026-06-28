use lambda_runtime::Error;
use spur_context_service::lambda;

#[tokio::main]
async fn main() -> Result<(), Error> {
    copy_bundled_extensions();
    if std::env::var("HOME").is_err() {
        std::env::set_var("HOME", "/tmp");
    }

    lambda_runtime::run(lambda_runtime::service_fn(lambda::handler)).await
}

fn copy_bundled_extensions() {
    let src = "/var/task/.duckdb/extensions";
    let dst = "/tmp/.duckdb/extensions";
    if !std::path::Path::new(src).exists() {
        return;
    }
    if let Err(e) = copy_dir_recursive(src, dst) {
        eprintln!("extension copy warning: {e}");
    }
}

fn copy_dir_recursive(src: &str, dst: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = std::path::Path::new(dst).join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(from.to_str().unwrap(), to.to_str().unwrap())?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
