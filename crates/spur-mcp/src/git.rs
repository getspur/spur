pub async fn run_git_capture(
    repo_root: &std::path::Path,
    cwd: Option<&std::path::Path>,
    args: &[&str],
) -> Result<String, String> {
    let work_dir = cwd.unwrap_or(repo_root);
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .output()
        .await
        .map_err(|e| format!("failed to execute git {}: {e}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
