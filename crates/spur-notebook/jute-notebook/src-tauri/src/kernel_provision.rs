use std::{future::Future, path::PathBuf, pin::Pin};

use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;
use tokio::sync::Mutex;

use crate::{
    backend::local::environment::{self, KernelSpec},
    Error,
};

static PYTHON3_KERNELSPEC_LOCK: Mutex<()> = Mutex::const_new(());

pub(crate) async fn ensure_python3_kernelspec(app: &AppHandle) -> Result<(), Error> {
    let _guard = PYTHON3_KERNELSPEC_LOCK.lock().await;
    let spur_jupyter =
        environment::spur_jupyter_dir().ok_or_else(|| Error::KernelProvisionFailed {
            stage: "home_dir",
            cause: "could not determine home directory".to_string(),
        })?;
    let runner = AppProvisionRunner { app: app.clone() };

    ensure_python3_kernelspec_in_dir(&spur_jupyter, &runner).await
}

trait ProvisionRunner: Sync {
    fn run_uv(
        &self,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    fn run_python(
        &self,
        python: PathBuf,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}

struct AppProvisionRunner {
    app: AppHandle,
}

impl ProvisionRunner for AppProvisionRunner {
    fn run_uv(
        &self,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(run_uv_sidecar(self.app.clone(), args))
    }

    fn run_python(
        &self,
        python: PathBuf,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(run_python_process(python, args))
    }
}

async fn ensure_python3_kernelspec_in_dir<R>(
    spur_jupyter: &std::path::Path,
    runner: &R,
) -> Result<(), Error>
where
    R: ProvisionRunner,
{
    let kernelspec = spur_jupyter
        .join("kernels")
        .join("python3")
        .join("kernel.json");
    if kernelspec_is_valid(&kernelspec).await {
        return Ok(());
    }

    tokio::fs::create_dir_all(spur_jupyter)
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "prepare_jupyter_dir",
            cause: error.to_string(),
        })?;

    let venv = spur_jupyter.join("venv");
    let python = venv_python_path(&venv);
    if !python.exists() {
        create_venv_with_fallback(runner, &venv, &python).await?;
    }

    runner
        .run_uv(vec![
            "pip".to_string(),
            "install".to_string(),
            "--python".to_string(),
            path_to_string(&python, "ipykernel_install")?,
            "ipykernel".to_string(),
        ])
        .await
        .map_err(|cause| Error::KernelProvisionFailed {
            stage: "ipykernel_install",
            cause,
        })?;

    runner
        .run_python(
            python,
            vec![
                "-m".to_string(),
                "ipykernel".to_string(),
                "install".to_string(),
                "--prefix".to_string(),
                path_to_string(spur_jupyter, "ipykernel_install_kernelspec")?,
                "--name".to_string(),
                "python3".to_string(),
                "--display-name".to_string(),
                "Python 3 (SPUR)".to_string(),
            ],
        )
        .await
        .map_err(|cause| Error::KernelProvisionFailed {
            stage: "ipykernel_install_kernelspec",
            cause,
        })?;

    if kernelspec_is_valid(&kernelspec).await {
        Ok(())
    } else {
        Err(Error::KernelProvisionFailed {
            stage: "kernelspec_validate",
            cause: format!(
                "{} was not created with a valid python argv",
                kernelspec.display()
            ),
        })
    }
}

async fn create_venv_with_fallback<R>(
    runner: &R,
    venv: &std::path::Path,
    python: &std::path::Path,
) -> Result<(), Error>
where
    R: ProvisionRunner,
{
    let mut causes = Vec::new();
    for version in ["3.12", "3.11"] {
        if venv.exists() {
            tokio::fs::remove_dir_all(venv).await.map_err(|error| {
                Error::KernelProvisionFailed {
                    stage: "venv_prepare",
                    cause: error.to_string(),
                }
            })?;
        }

        let result = runner
            .run_uv(vec![
                "venv".to_string(),
                "--no-project".to_string(),
                "--seed".to_string(),
                "--python".to_string(),
                version.to_string(),
                "--python-preference".to_string(),
                "managed".to_string(),
                path_to_string(venv, "venv_create")?,
            ])
            .await;

        match result {
            Ok(()) if python.exists() => return Ok(()),
            Ok(()) => causes.push(format!(
                "python {version}: uv completed but {} was not created",
                python.display()
            )),
            Err(cause) => causes.push(format!("python {version}: {cause}")),
        }
    }

    Err(Error::KernelProvisionFailed {
        stage: "venv_create",
        cause: causes.join("; "),
    })
}

async fn kernelspec_is_valid(path: &std::path::Path) -> bool {
    let Ok(contents) = tokio::fs::read(path).await else {
        return false;
    };
    let Ok(spec) = serde_json::from_slice::<KernelSpec>(&contents) else {
        return false;
    };
    let Some(command) = spec.argv.first() else {
        return false;
    };
    let command = std::path::Path::new(command);
    command.is_absolute() && command.exists()
}

fn venv_python_path(venv: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn path_to_string(path: &std::path::Path, stage: &'static str) -> Result<String, Error> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::KernelProvisionFailed {
            stage,
            cause: format!("path is not valid UTF-8: {}", path.display()),
        })
}

async fn run_uv_sidecar(app: AppHandle, args: Vec<String>) -> Result<(), String> {
    let output = app
        .shell()
        .sidecar("uv")
        .map_err(|error| error.to_string())?
        .args(["--color", "never"])
        .args(args)
        .output()
        .await
        .map_err(|error| error.to_string())?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format_command_failure(
            output.status.code(),
            &output.stdout,
            &output.stderr,
        ))
    }
}

async fn run_python_process(python: PathBuf, args: Vec<String>) -> Result<(), String> {
    let output = tokio::process::Command::new(&python)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("{}: {error}", python.display()))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format_command_failure(
            output.status.code(),
            &output.stdout,
            &output.stderr,
        ))
    }
}

fn format_command_failure(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let mut message = match code {
        Some(code) => format!("process exited with status {code}"),
        None => "process terminated by signal".to_string(),
    };

    let stderr = stderr.trim();
    if !stderr.is_empty() {
        message.push_str(": ");
        message.push_str(stderr);
    }

    let stdout = stdout.trim();
    if !stdout.is_empty() {
        message.push_str(" stdout: ");
        message.push_str(stdout);
    }

    message
}

#[cfg(test)]
mod tests {
    use std::{future::Future, path::PathBuf, pin::Pin};

    use uuid::Uuid;

    use super::*;

    struct PanicRunner;

    impl ProvisionRunner for PanicRunner {
        fn run_uv(
            &self,
            _args: Vec<String>,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async { panic!("valid kernelspec should skip uv") })
        }

        fn run_python(
            &self,
            _python: PathBuf,
            _args: Vec<String>,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async { panic!("valid kernelspec should skip python") })
        }
    }

    #[tokio::test]
    async fn existing_valid_kernelspec_skips_provision_commands() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let kernelspec = root.join("kernels").join("python3").join("kernel.json");
        let python = venv_python_path(&root.join("venv"));

        tokio::fs::create_dir_all(python.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&python, b"").await.unwrap();
        tokio::fs::create_dir_all(kernelspec.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &kernelspec,
            serde_json::json!({
                "argv": [
                    python.to_string_lossy(),
                    "-m",
                    "ipykernel_launcher",
                    "-f",
                    "{connection_file}"
                ],
                "display_name": "Python 3 (SPUR)",
                "language": "python"
            })
            .to_string(),
        )
        .await
        .unwrap();

        ensure_python3_kernelspec_in_dir(&root, &PanicRunner)
            .await
            .unwrap();

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
