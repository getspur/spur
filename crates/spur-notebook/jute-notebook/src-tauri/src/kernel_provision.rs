use std::{env, future::Future, path::PathBuf, pin::Pin};

use tauri::AppHandle;
use tauri_plugin_shell::ShellExt as _;
use tokio::sync::Mutex;

use crate::{
    backend::local::environment::{self, KernelSpec},
    Error,
};

static PYTHON3_KERNELSPEC_LOCK: Mutex<()> = Mutex::const_new(());
static DENO_KERNELSPEC_LOCK: Mutex<()> = Mutex::const_new(());
static EVCXR_KERNELSPEC_LOCK: Mutex<()> = Mutex::const_new(());
static GONB_KERNELSPEC_LOCK: Mutex<()> = Mutex::const_new(());

// DuckDB 1.5.x is compatible with the stable v1.2.0 C-API extension envelope.
const MANAGED_KERNEL_DUCKDB_VERSION: &str = "1.5.3";
/// Arrow crate major version used by managed Rust kernel bootstrap code.
pub const EVCXR_ARROW_CRATE_VERSION: &str = "55";
/// Arrow Go module path used by managed Go kernel bootstrap code.
pub const GONB_ARROW_GO_MODULE: &str = "github.com/apache/arrow-go/v18";

/// Ensure the bundled `python3` kernelspec is installed for this app.
pub async fn ensure_python3_kernelspec(app: &AppHandle) -> Result<(), Error> {
    let _guard = PYTHON3_KERNELSPEC_LOCK.lock().await;
    let spur_jupyter =
        environment::spur_jupyter_dir().ok_or_else(|| Error::KernelProvisionFailed {
            stage: "home_dir",
            cause: "could not determine home directory".to_owned(),
        })?;
    let runner = AppProvisionRunner { app: app.clone() };

    ensure_python3_kernelspec_in_dir(&spur_jupyter, &runner).await
}

/// Return whether a `python3` kernelspec is already discoverable.
pub async fn python3_kernelspec_is_valid() -> bool {
    let _guard = PYTHON3_KERNELSPEC_LOCK.lock().await;
    environment::list_kernels(None)
        .await
        .into_iter()
        .any(|(path, _spec)| path.file_name().and_then(|name| name.to_str()) == Some("python3"))
}

/// Ensure the bundled `deno` kernelspec is installed for this app.
pub async fn ensure_deno_kernelspec() -> Result<(), Error> {
    let _guard = DENO_KERNELSPEC_LOCK.lock().await;
    let spur_jupyter =
        environment::spur_jupyter_dir().ok_or_else(|| Error::KernelProvisionFailed {
            stage: "home_dir",
            cause: "could not determine home directory".to_owned(),
        })?;

    ensure_deno_kernelspec_in_dir(&spur_jupyter).await
}

/// Ensure the `evcxr` kernelspec is installed for this app.
pub async fn ensure_evcxr_kernelspec() -> Result<(), Error> {
    let _guard = EVCXR_KERNELSPEC_LOCK.lock().await;
    let spur_jupyter =
        environment::spur_jupyter_dir().ok_or_else(|| Error::KernelProvisionFailed {
            stage: "home_dir",
            cause: "could not determine home directory".to_owned(),
        })?;

    ensure_evcxr_kernelspec_installed(&spur_jupyter).await
}

/// Ensure the `gonb` kernelspec is installed for this app.
pub async fn ensure_gonb_kernelspec() -> Result<(), Error> {
    let _guard = GONB_KERNELSPEC_LOCK.lock().await;
    let spur_jupyter =
        environment::spur_jupyter_dir().ok_or_else(|| Error::KernelProvisionFailed {
            stage: "home_dir",
            cause: "could not determine home directory".to_owned(),
        })?;

    ensure_gonb_kernelspec_installed(&spur_jupyter).await
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

async fn ensure_evcxr_kernelspec_installed(spur_jupyter: &std::path::Path) -> Result<(), Error> {
    let kernelspec = spur_jupyter
        .join("kernels")
        .join("evcxr")
        .join("kernel.json");
    if kernelspec_is_valid(&kernelspec).await {
        return Ok(());
    }

    let cargo = cargo_binary_path()?;
    run_process(
        cargo,
        vec![
            "install".to_owned(),
            "--locked".to_owned(),
            "evcxr_jupyter".to_owned(),
        ],
    )
    .await
    .map_err(|cause| Error::KernelProvisionFailed {
        stage: "evcxr_install",
        cause,
    })?;

    let evcxr = evcxr_jupyter_binary_path()?;
    ensure_evcxr_kernelspec_in_dir(spur_jupyter, &evcxr).await
}

async fn ensure_gonb_kernelspec_installed(spur_jupyter: &std::path::Path) -> Result<(), Error> {
    let kernelspec = spur_jupyter
        .join("kernels")
        .join("gonb")
        .join("kernel.json");
    if kernelspec_is_valid(&kernelspec).await {
        return Ok(());
    }

    let go = go_binary_path()?;
    run_process(
        go,
        vec![
            "install".to_owned(),
            "github.com/janpfeifer/gonb@latest".to_owned(),
        ],
    )
    .await
    .map_err(|cause| Error::KernelProvisionFailed {
        stage: "gonb_install",
        cause,
    })?;

    let gonb = gonb_binary_path()?;
    ensure_gonb_kernelspec_in_dir(spur_jupyter, &gonb).await
}

async fn ensure_deno_kernelspec_in_dir(spur_jupyter: &std::path::Path) -> Result<(), Error> {
    let kernelspec = spur_jupyter
        .join("kernels")
        .join("deno")
        .join("kernel.json");
    if kernelspec_is_valid(&kernelspec).await {
        return Ok(());
    }

    let deno = deno_binary_path()?;
    let Some(parent) = kernelspec.parent() else {
        return Err(Error::KernelProvisionFailed {
            stage: "prepare_deno_kernelspec_dir",
            cause: format!("{} has no parent directory", kernelspec.display()),
        });
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "prepare_deno_kernelspec_dir",
            cause: error.to_string(),
        })?;

    let payload = serde_json::json!({
        "argv": [
            path_to_string(&deno, "deno_kernelspec_write")?,
            "jupyter",
            "--kernel",
            "--conn",
            "{connection_file}"
        ],
        "display_name": "Deno",
        "language": "typescript"
    });

    tokio::fs::write(&kernelspec, payload.to_string())
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "deno_kernelspec_write",
            cause: error.to_string(),
        })?;

    if kernelspec_is_valid(&kernelspec).await {
        Ok(())
    } else {
        Err(Error::KernelProvisionFailed {
            stage: "kernelspec_validate",
            cause: format!(
                "{} was not created with a valid deno argv",
                kernelspec.display()
            ),
        })
    }
}

async fn ensure_evcxr_kernelspec_in_dir(
    spur_jupyter: &std::path::Path,
    evcxr_jupyter: &std::path::Path,
) -> Result<(), Error> {
    let kernelspec = spur_jupyter
        .join("kernels")
        .join("evcxr")
        .join("kernel.json");
    if kernelspec_is_valid(&kernelspec).await {
        return Ok(());
    }

    let Some(parent) = kernelspec.parent() else {
        return Err(Error::KernelProvisionFailed {
            stage: "prepare_evcxr_kernelspec_dir",
            cause: format!("{} has no parent directory", kernelspec.display()),
        });
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "prepare_evcxr_kernelspec_dir",
            cause: error.to_string(),
        })?;

    let payload = serde_json::json!({
        "argv": [
            path_to_string(evcxr_jupyter, "evcxr_kernelspec_write")?,
            "--control_file",
            "{connection_file}"
        ],
        "display_name": "Rust (evcxr)",
        "language": "rust"
    });

    tokio::fs::write(&kernelspec, payload.to_string())
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "evcxr_kernelspec_write",
            cause: error.to_string(),
        })?;

    if kernelspec_is_valid(&kernelspec).await {
        Ok(())
    } else {
        Err(Error::KernelProvisionFailed {
            stage: "kernelspec_validate",
            cause: format!(
                "{} was not created with a valid evcxr argv",
                kernelspec.display()
            ),
        })
    }
}

async fn ensure_gonb_kernelspec_in_dir(
    spur_jupyter: &std::path::Path,
    gonb: &std::path::Path,
) -> Result<(), Error> {
    let kernelspec = spur_jupyter
        .join("kernels")
        .join("gonb")
        .join("kernel.json");
    if kernelspec_is_valid(&kernelspec).await {
        return Ok(());
    }

    let Some(parent) = kernelspec.parent() else {
        return Err(Error::KernelProvisionFailed {
            stage: "prepare_gonb_kernelspec_dir",
            cause: format!("{} has no parent directory", kernelspec.display()),
        });
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "prepare_gonb_kernelspec_dir",
            cause: error.to_string(),
        })?;

    let payload = serde_json::json!({
        "argv": [
            path_to_string(gonb, "gonb_kernelspec_write")?,
            "--kernel",
            "{connection_file}"
        ],
        "display_name": "Go (gonb)",
        "language": "go"
    });

    tokio::fs::write(&kernelspec, payload.to_string())
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "gonb_kernelspec_write",
            cause: error.to_string(),
        })?;

    if kernelspec_is_valid(&kernelspec).await {
        Ok(())
    } else {
        Err(Error::KernelProvisionFailed {
            stage: "kernelspec_validate",
            cause: format!(
                "{} was not created with a valid gonb argv",
                kernelspec.display()
            ),
        })
    }
}

async fn ensure_python3_kernelspec_in_dir<R>(
    spur_jupyter: &std::path::Path,
    runner: &R,
) -> Result<(), Error>
where
    R: ProvisionRunner,
{
    let kernelspec = python3_kernelspec_path(spur_jupyter);
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
            "pip".to_owned(),
            "install".to_owned(),
            "--python".to_owned(),
            path_to_string(&python, "ipykernel_install")?,
            "ipykernel".to_owned(),
            format!("duckdb=={MANAGED_KERNEL_DUCKDB_VERSION}"),
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
                "-m".to_owned(),
                "ipykernel".to_owned(),
                "install".to_owned(),
                "--prefix".to_owned(),
                path_to_string(spur_jupyter, "ipykernel_install_kernelspec")?,
                "--name".to_owned(),
                "python3".to_owned(),
                "--display-name".to_owned(),
                "Python 3 (SPUR)".to_owned(),
            ],
        )
        .await
        .map_err(|cause| Error::KernelProvisionFailed {
            stage: "ipykernel_install_kernelspec",
            cause,
        })?;

    // `ipykernel install --prefix X` writes to `X/share/jupyter/kernels/<name>/`,
    // but kernel discovery in this crate (`environment::list_kernels`) walks
    // `<data_dir>/kernels/<name>/` directly. Relocate the produced kernelspec
    // so it lands at the validated path.
    let installed = spur_jupyter
        .join("share")
        .join("jupyter")
        .join("kernels")
        .join("python3");
    let destination = spur_jupyter.join("kernels").join("python3");
    relocate_kernelspec(&installed, &destination).await?;

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

#[cfg(test)]
async fn python3_kernelspec_is_valid_in_dir(spur_jupyter: &std::path::Path) -> bool {
    kernelspec_is_valid(&python3_kernelspec_path(spur_jupyter)).await
}

fn python3_kernelspec_path(spur_jupyter: &std::path::Path) -> PathBuf {
    spur_jupyter
        .join("kernels")
        .join("python3")
        .join("kernel.json")
}

async fn relocate_kernelspec(
    installed: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(), Error> {
    if !installed.exists() {
        return Err(Error::KernelProvisionFailed {
            stage: "kernelspec_relocate",
            cause: format!(
                "ipykernel did not produce a kernelspec at {}",
                installed.display()
            ),
        });
    }

    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| Error::KernelProvisionFailed {
                stage: "kernelspec_relocate",
                cause: error.to_string(),
            })?;
    }

    if destination.exists() {
        tokio::fs::remove_dir_all(destination)
            .await
            .map_err(|error| Error::KernelProvisionFailed {
                stage: "kernelspec_relocate",
                cause: error.to_string(),
            })?;
    }

    copy_dir_recursive(installed, destination)
        .await
        .map_err(|error| Error::KernelProvisionFailed {
            stage: "kernelspec_relocate",
            cause: error.to_string(),
        })
}

async fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            Box::pin(copy_dir_recursive(&entry.path(), &target)).await?;
        } else {
            tokio::fs::copy(entry.path(), target).await?;
        }
    }
    Ok(())
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
                "venv".to_owned(),
                "--no-project".to_owned(),
                "--seed".to_owned(),
                "--python".to_owned(),
                version.to_owned(),
                "--python-preference".to_owned(),
                "managed".to_owned(),
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

fn deno_binary_path() -> Result<PathBuf, Error> {
    resolve_binary_from_env_or_path(
        "deno",
        "DENO_PATH",
        "deno_path",
        "install Deno from https://deno.com/ or set DENO_PATH to the deno binary",
    )
}

fn cargo_binary_path() -> Result<PathBuf, Error> {
    resolve_binary_from_env_or_path(
        "cargo",
        "CARGO_PATH",
        "cargo_path",
        "install Rust via rustup (https://rustup.rs/) or set CARGO_PATH to the cargo binary",
    )
}

fn go_binary_path() -> Result<PathBuf, Error> {
    resolve_binary_from_env_or_path(
        "go",
        "GO_PATH",
        "go_path",
        "install Go from https://go.dev/ or set GO_PATH to the go binary",
    )
}

fn resolve_binary_from_env_or_path(
    binary: &str,
    env_var: &str,
    stage: &'static str,
    missing_hint: &str,
) -> Result<PathBuf, Error> {
    if let Some(path) = env::var_os(env_var) {
        return existing_absolute_binary(PathBuf::from(path), stage).map_err(|error| {
            let Error::KernelProvisionFailed { cause, .. } = error else {
                return error;
            };
            Error::KernelProvisionFailed {
                stage,
                cause: format!("{env_var} is set but unusable: {cause}; {missing_hint}"),
            }
        });
    }

    find_binary_on_path(binary, stage).ok_or_else(|| Error::KernelProvisionFailed {
        stage,
        cause: format!("could not resolve {binary} from PATH; {missing_hint}"),
    })
}

fn find_binary_on_path(binary: &str, stage: &'static str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        for name in binary_path_names(binary) {
            let candidate = dir.join(name);
            if let Ok(candidate) = existing_absolute_binary(candidate, stage) {
                return Some(candidate);
            }
        }
    }
    None
}

fn evcxr_jupyter_binary_path() -> Result<PathBuf, Error> {
    let binary = if cfg!(windows) {
        "evcxr_jupyter.exe"
    } else {
        "evcxr_jupyter"
    };
    let mut candidates = Vec::new();
    if let Some(cargo_home) = env::var_os("CARGO_HOME") {
        candidates.push(PathBuf::from(cargo_home).join("bin").join(binary));
    }
    if let Some(home) = environment::default_home_dir() {
        candidates.push(home.join(".cargo").join("bin").join(binary));
    }

    find_binary_in_candidates_or_path(candidates, "evcxr_jupyter", "evcxr_path")
}

fn gonb_binary_path() -> Result<PathBuf, Error> {
    let binary = if cfg!(windows) { "gonb.exe" } else { "gonb" };
    let mut candidates = Vec::new();
    if let Some(gobin) = env::var_os("GOBIN") {
        candidates.push(PathBuf::from(gobin).join(binary));
    }
    if let Some(gopath) = env::var_os("GOPATH") {
        for path in env::split_paths(&gopath) {
            candidates.push(path.join("bin").join(binary));
        }
    }
    if let Some(home) = environment::default_home_dir() {
        candidates.push(home.join("go").join("bin").join(binary));
    }

    find_binary_in_candidates_or_path(candidates, "gonb", "gonb_path")
}

fn find_binary_in_candidates_or_path(
    candidates: Vec<PathBuf>,
    binary: &str,
    stage: &'static str,
) -> Result<PathBuf, Error> {
    for candidate in candidates {
        if let Ok(candidate) = existing_absolute_binary(candidate, stage) {
            return Ok(candidate);
        }
    }

    find_binary_on_path(binary, stage).ok_or_else(|| Error::KernelProvisionFailed {
        stage,
        cause: format!("could not locate {binary} after installation"),
    })
}

fn binary_path_names(binary: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{binary}.exe"), binary.to_owned()]
    } else {
        vec![binary.to_owned()]
    }
}

fn existing_absolute_binary(path: PathBuf, stage: &'static str) -> Result<PathBuf, Error> {
    let path = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map_err(|error| Error::KernelProvisionFailed {
                stage,
                cause: error.to_string(),
            })?
            .join(path)
    };

    if path.exists() {
        Ok(path)
    } else {
        Err(Error::KernelProvisionFailed {
            stage,
            cause: format!("{} does not exist", path.display()),
        })
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

async fn run_process(binary: PathBuf, args: Vec<String>) -> Result<(), String> {
    let output = tokio::process::Command::new(&binary)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("{}: {error}", binary.display()))?;

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
        None => "process terminated by signal".to_owned(),
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
    use std::{future::Future, path::PathBuf, pin::Pin, sync::Mutex};

    use uuid::Uuid;

    use super::*;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

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

    #[derive(Default)]
    struct RecordingRunner {
        uv_args: Mutex<Vec<Vec<String>>>,
    }

    impl ProvisionRunner for RecordingRunner {
        fn run_uv(
            &self,
            args: Vec<String>,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            self.uv_args.lock().unwrap().push(args.clone());
            Box::pin(async move {
                if args.first().map(String::as_str) == Some("venv") {
                    let venv = args.last().ok_or_else(|| "missing venv path".to_string())?;
                    let python = venv_python_path(&PathBuf::from(venv));
                    let parent = python
                        .parent()
                        .ok_or_else(|| format!("{} has no parent", python.display()))?;
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|error| error.to_string())?;
                    tokio::fs::write(&python, b"")
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
        }

        fn run_python(
            &self,
            python: PathBuf,
            args: Vec<String>,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async move {
                let prefix = args
                    .windows(2)
                    .find_map(|pair| {
                        (pair[0] == "--prefix").then(|| PathBuf::from(pair[1].clone()))
                    })
                    .ok_or_else(|| "missing kernelspec prefix".to_string())?;
                let installed = prefix
                    .join("share")
                    .join("jupyter")
                    .join("kernels")
                    .join("python3");
                tokio::fs::create_dir_all(&installed)
                    .await
                    .map_err(|error| error.to_string())?;
                tokio::fs::write(
                    installed.join("kernel.json"),
                    serde_json::json!({
                        "argv": [
                            python.to_string_lossy().to_string(),
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
                .map_err(|error| error.to_string())
            })
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

    #[tokio::test]
    async fn python3_kernelspec_validity_probe_reads_bundled_path() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let kernelspec = python3_kernelspec_path(&root);
        let python = venv_python_path(&root.join("venv"));

        assert!(!python3_kernelspec_is_valid_in_dir(&root).await);

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

        assert!(python3_kernelspec_is_valid_in_dir(&root).await);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn python3_kernelspec_validity_probe_honors_jupyter_path() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-path-{}", Uuid::new_v4()));
        let home = root.join("home");
        let jupyter_root = root.join("jupyter");
        let kernelspec = jupyter_root
            .join("kernels")
            .join("python3")
            .join("kernel.json");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::create_dir_all(kernelspec.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &kernelspec,
            serde_json::json!({
                "argv": [
                    "/usr/bin/python3",
                    "-m",
                    "ipykernel_launcher",
                    "-f",
                    "{connection_file}"
                ],
                "display_name": "Python 3",
                "language": "python"
            })
            .to_string(),
        )
        .await
        .unwrap();

        let _home = EnvVarGuard::set("HOME", home.as_os_str());
        let _jupyter_path = EnvVarGuard::set("JUPYTER_PATH", jupyter_root.as_os_str());

        assert!(python3_kernelspec_is_valid().await);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn provisioning_installs_pinned_duckdb_package() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let runner = RecordingRunner::default();

        ensure_python3_kernelspec_in_dir(&root, &runner)
            .await
            .unwrap();

        let uv_args = runner.uv_args.lock().unwrap();
        let pip_install_args = uv_args
            .iter()
            .find(|args| {
                args.first().map(String::as_str) == Some("pip")
                    && args.get(1).map(String::as_str) == Some("install")
            })
            .expect("expected uv pip install command");

        assert!(pip_install_args.contains(&"ipykernel".to_string()));
        assert!(pip_install_args.contains(&format!("duckdb=={MANAGED_KERNEL_DUCKDB_VERSION}")));

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn ensure_deno_kernelspec_writes_template_from_env_override() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let deno = root
            .join("bin")
            .join(if cfg!(windows) { "deno.exe" } else { "deno" });
        tokio::fs::create_dir_all(deno.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&deno, b"").await.unwrap();

        let previous_deno_path = std::env::var_os("DENO_PATH");
        std::env::set_var("DENO_PATH", &deno);

        ensure_deno_kernelspec_in_dir(&root).await.unwrap();

        let kernelspec = root.join("kernels").join("deno").join("kernel.json");
        let contents = tokio::fs::read(&kernelspec).await.unwrap();
        let spec = serde_json::from_slice::<serde_json::Value>(&contents).unwrap();
        assert_eq!(
            spec,
            serde_json::json!({
                "argv": [
                    deno.to_string_lossy(),
                    "jupyter",
                    "--kernel",
                    "--conn",
                    "{connection_file}"
                ],
                "display_name": "Deno",
                "language": "typescript"
            })
        );
        assert!(kernelspec_is_valid(&kernelspec).await);

        match previous_deno_path {
            Some(value) => std::env::set_var("DENO_PATH", value),
            None => std::env::remove_var("DENO_PATH"),
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn ensure_evcxr_kernelspec_writes_template_from_injected_binary() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let evcxr = root.join("bin").join(if cfg!(windows) {
            "evcxr_jupyter.exe"
        } else {
            "evcxr_jupyter"
        });
        tokio::fs::create_dir_all(evcxr.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&evcxr, b"").await.unwrap();

        ensure_evcxr_kernelspec_in_dir(&root, &evcxr).await.unwrap();

        let kernelspec = root.join("kernels").join("evcxr").join("kernel.json");
        let contents = tokio::fs::read(&kernelspec).await.unwrap();
        let spec = serde_json::from_slice::<serde_json::Value>(&contents).unwrap();
        assert_eq!(
            spec,
            serde_json::json!({
                "argv": [
                    evcxr.to_string_lossy(),
                    "--control_file",
                    "{connection_file}"
                ],
                "display_name": "Rust (evcxr)",
                "language": "rust"
            })
        );
        assert!(kernelspec_is_valid(&kernelspec).await);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn ensure_gonb_kernelspec_writes_template_from_injected_binary() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let gonb = root
            .join("bin")
            .join(if cfg!(windows) { "gonb.exe" } else { "gonb" });
        tokio::fs::create_dir_all(gonb.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&gonb, b"").await.unwrap();

        ensure_gonb_kernelspec_in_dir(&root, &gonb).await.unwrap();

        let kernelspec = root.join("kernels").join("gonb").join("kernel.json");
        let contents = tokio::fs::read(&kernelspec).await.unwrap();
        let spec = serde_json::from_slice::<serde_json::Value>(&contents).unwrap();
        assert_eq!(
            spec,
            serde_json::json!({
                "argv": [
                    gonb.to_string_lossy(),
                    "--kernel",
                    "{connection_file}"
                ],
                "display_name": "Go (gonb)",
                "language": "go"
            })
        );
        assert!(kernelspec_is_valid(&kernelspec).await);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn valid_evcxr_kernelspec_skips_injected_binary_validation() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let kernelspec = root.join("kernels").join("evcxr").join("kernel.json");
        let evcxr = root.join("bin").join(if cfg!(windows) {
            "evcxr_jupyter.exe"
        } else {
            "evcxr_jupyter"
        });
        tokio::fs::create_dir_all(evcxr.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&evcxr, b"").await.unwrap();
        tokio::fs::create_dir_all(kernelspec.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &kernelspec,
            serde_json::json!({
                "argv": [
                    evcxr.to_string_lossy(),
                    "--control_file",
                    "{connection_file}"
                ],
                "display_name": "Rust (evcxr)",
                "language": "rust"
            })
            .to_string(),
        )
        .await
        .unwrap();

        ensure_evcxr_kernelspec_in_dir(&root, &root.join("missing-evcxr"))
            .await
            .unwrap();

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn relocate_moves_share_jupyter_kernels_to_discovered_path() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let installed = root
            .join("share")
            .join("jupyter")
            .join("kernels")
            .join("python3");
        let destination = root.join("kernels").join("python3");

        tokio::fs::create_dir_all(&installed).await.unwrap();
        let spec_payload = serde_json::json!({
            "argv": ["/usr/bin/python3", "-m", "ipykernel_launcher", "-f", "{connection_file}"],
            "display_name": "Python 3 (SPUR)",
            "language": "python"
        })
        .to_string();
        tokio::fs::write(installed.join("kernel.json"), &spec_payload)
            .await
            .unwrap();
        tokio::fs::write(installed.join("logo-32x32.png"), b"png-bytes")
            .await
            .unwrap();

        relocate_kernelspec(&installed, &destination).await.unwrap();

        let copied_spec = tokio::fs::read(destination.join("kernel.json"))
            .await
            .unwrap();
        assert_eq!(copied_spec, spec_payload.as_bytes());
        let copied_logo = tokio::fs::read(destination.join("logo-32x32.png"))
            .await
            .unwrap();
        assert_eq!(copied_logo, b"png-bytes");

        // Idempotent: relocating again overwrites a stale destination.
        tokio::fs::write(destination.join("kernel.json"), b"stale")
            .await
            .unwrap();
        relocate_kernelspec(&installed, &destination).await.unwrap();
        let refreshed = tokio::fs::read(destination.join("kernel.json"))
            .await
            .unwrap();
        assert_eq!(refreshed, spec_payload.as_bytes());

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn relocate_errors_when_ipykernel_did_not_produce_spec() {
        let root = std::env::temp_dir().join(format!("spur-jupyter-{}", Uuid::new_v4()));
        let installed = root
            .join("share")
            .join("jupyter")
            .join("kernels")
            .join("python3");
        let destination = root.join("kernels").join("python3");

        let result = relocate_kernelspec(&installed, &destination).await;
        assert!(matches!(
            result,
            Err(Error::KernelProvisionFailed {
                stage: "kernelspec_relocate",
                ..
            })
        ));
    }
}
