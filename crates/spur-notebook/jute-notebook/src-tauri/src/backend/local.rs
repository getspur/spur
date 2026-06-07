//! Code that starts local kernels to be `jupyter-server` compatible.
//!
//! This is currently unused while Jute relies on `jupyter-server`, but in the
//! future it could replace the Jupyter installation by directly invoking
//! kernels, or introduce new APIs for developer experience.

use std::{collections::BTreeMap, process::Stdio};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::fs;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use ts_rs::TS;
use uuid::Uuid;

use self::environment::KernelSpec;
use super::{create_zeromq_connection, KernelConnection};
use crate::Error;

pub mod environment;

/// Contains information about the CPU and memory usage of a kernel.
#[derive(Serialize, Deserialize, Debug, Clone, TS)]
pub struct KernelUsageInfo {
    /// Number of CPUs used.
    pub cpu_consumed: f32,

    /// Number of CPUs available.
    pub cpu_available: f32,

    /// Memory consumed in KB.
    pub memory_consumed: f32,

    /// Memory available in KB.
    pub memory_available: f32,
}

/// Represents a connection to an active kernel.
pub struct LocalKernel {
    child: tokio::process::Child,
    kernel_id: String,

    spec: KernelSpec,
    conn: KernelConnection,
}

impl LocalKernel {
    /// Start a new kernel based on a spec, and connect to it.
    pub async fn start(spec: &KernelSpec) -> Result<Self, Error> {
        let (control_port, shell_port, iopub_port, stdin_port, heartbeat_port) = tokio::try_join!(
            get_available_port(),
            get_available_port(),
            get_available_port(),
            get_available_port(),
            get_available_port(),
        )?;
        let signing_key = Uuid::new_v4().to_string();
        let connection_file = json!({
            "control_port": control_port,
            "shell_port": shell_port,
            "iopub_port": iopub_port,
            "stdin_port": stdin_port,
            "hb_port": heartbeat_port,
            "transport": "tcp",
            "ip": "127.0.0.1",
            "signature_scheme": "hmac-sha256",
            "key": signing_key,
        });

        let kernel_id = Uuid::new_v4().to_string();
        let runtime_dir = environment::runtime_dir();
        fs::create_dir_all(&runtime_dir)
            .await
            .map_err(|err| Error::KernelConnect(format!("could not create runtime dir: {err}")))?;
        let connection_filename = format!("{runtime_dir}jute-{kernel_id}.json");
        fs::write(&connection_filename, connection_file.to_string())
            .await
            .map_err(|err| {
                Error::KernelConnect(format!("could not write connection file: {err}"))
            })?;

        if spec.argv.is_empty() {
            return Err(Error::KernelConnect("kernel spec has no argv".into()));
        }
        let argv: Vec<String> = spec
            .argv
            .iter()
            .map(|s| s.replace("{connection_file}", &connection_filename))
            .collect();
        // Capture the kernel's stderr (fd 2) rather than discarding it: some
        // kernels (notably evcxr) emit compile progress like
        // `Compiling <crate> v<ver>` directly on stderr instead of over IOPub.
        let mut child = kernel_command(&argv, &spec.env)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(Error::Subprocess)?;

        let (process_stderr_tx, _) = broadcast::channel::<String>(256);

        if let Some(stderr) = child.stderr.take() {
            let tx = process_stderr_tx.clone();
            // Detached reader: ends when stderr closes (process exit). It never
            // blocks shutdown — `kill_on_drop` reaps the child, closing stderr.
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Ignore SendError: no subscribers yet just drops the line.
                    let _ = tx.send(line);
                }
            });
        }

        let conn = create_zeromq_connection(
            shell_port,
            control_port,
            iopub_port,
            stdin_port,
            heartbeat_port,
            &signing_key,
            process_stderr_tx,
        )
        .await?;

        Ok(Self {
            child,
            kernel_id,
            spec: spec.clone(),
            conn,
        })
    }

    /// Get the kernel ID.
    pub fn id(&self) -> &str {
        &self.kernel_id
    }

    /// Get the kernel connection object.
    pub fn conn(&self) -> &KernelConnection {
        &self.conn
    }

    /// Return the spec used to start the kernel.
    pub fn spec(&self) -> &KernelSpec {
        &self.spec
    }

    /// Check if the kernel is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the kernel by sending a SIGKILL signal.
    pub async fn kill(&mut self) -> Result<(), Error> {
        self.child.kill().await.map_err(Error::Subprocess)
    }

    /// Get the pid of the kernel process.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

async fn get_available_port() -> Result<u16, Error> {
    let addr = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| Error::KernelConnect(format!("could not get available port: {err}")))?
        .local_addr()
        .map_err(|e| Error::KernelConnect(format!("tcp listener has no local address: {e}")))?;
    Ok(addr.port())
}

fn kernel_command(argv: &[String], env: &BTreeMap<String, String>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .envs(env)
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
pub(crate) fn kernel_command_for_test(
    argv: &[String],
    env: &BTreeMap<String, String>,
) -> tokio::process::Command {
    kernel_command(argv, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    struct EnvVarGuard {
        key: String,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: String, value: &str) -> Self {
            let previous = std::env::var_os(&key);
            std::env::set_var(&key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(&self.key, previous);
            } else {
                std::env::remove_var(&self.key);
            }
        }
    }

    #[tokio::test]
    async fn kernel_command_applies_spec_env_and_preserves_parent_env() {
        let unique = Uuid::new_v4().to_string().replace('-', "_");
        let added_key = format!("SPUR_ENV_ADDED_{unique}");
        let override_key = format!("SPUR_ENV_OVERRIDE_{unique}");
        let parent_key = format!("SPUR_ENV_PARENT_{unique}");
        let _parent_guard = EnvVarGuard::set(parent_key.clone(), "parent");
        let _override_guard = EnvVarGuard::set(override_key.clone(), "parent");

        let spec_env = BTreeMap::from([
            (added_key.clone(), "added".to_string()),
            (override_key.clone(), "spec".to_string()),
        ]);
        let output_file = tempfile::NamedTempFile::new().expect("create temp output file");
        let output_path = output_file.path().to_owned();
        let script = format!(
            "printf '%s|%s|%s' \"${{{added_key}}}\" \"${{{override_key}}}\" \"${{{parent_key}}}\" > \"$1\""
        );
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            script,
            "kernel-env-test".to_string(),
            output_path.to_string_lossy().into_owned(),
        ];

        let status = kernel_command(&argv, &spec_env)
            .spawn()
            .expect("spawn env echo child")
            .wait()
            .await
            .expect("wait for env echo child");

        assert!(status.success());
        let output = fs::read_to_string(output_path)
            .await
            .expect("read env echo output");
        assert_eq!(output, "added|spec|parent");
    }
}
