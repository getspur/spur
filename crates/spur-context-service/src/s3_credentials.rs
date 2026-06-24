use std::env;
use std::thread;

use anyhow::{anyhow, Context as _, Result};
use aws_config::BehaviorVersion;
use aws_credential_types::provider::ProvideCredentials as _;
use aws_sdk_s3::config::Region;
use duckdb::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DuckDbS3Credentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    region: String,
}

#[expect(
    clippy::future_not_send,
    reason = "DuckDB connections are synchronous and not Sync; credentials resolve before use"
)]
pub(crate) async fn resolve_and_set_s3_credentials(conn: &Connection) -> Result<()> {
    let credentials = resolve_s3_credentials().await?;
    set_s3_credentials(conn, &credentials)
}

pub(crate) fn resolve_and_set_s3_credentials_blocking(conn: &Connection) -> Result<()> {
    if tokio::runtime::Handle::try_current().is_ok() {
        let credentials = thread::spawn(block_on_resolve_s3_credentials)
            .join()
            .map_err(|_panic| anyhow!("AWS credential resolver thread panicked"))?;
        return set_s3_credentials(conn, &credentials?);
    }

    tokio::runtime::Runtime::new()
        .context("failed to create tokio runtime for AWS credential resolution")?
        .block_on(resolve_and_set_s3_credentials(conn))
}

fn block_on_resolve_s3_credentials() -> Result<DuckDbS3Credentials> {
    tokio::runtime::Runtime::new()
        .context("failed to create tokio runtime for AWS credential resolution")?
        .block_on(resolve_s3_credentials())
}

async fn resolve_s3_credentials() -> Result<DuckDbS3Credentials> {
    let region = aws_region();
    match resolve_s3_credentials_from_sdk(&region).await {
        Ok(credentials) => Ok(credentials),
        Err(error) => credentials_from_env(&region).ok_or(error),
    }
    .context("failed to resolve AWS credentials for DuckDB S3")
}

async fn resolve_s3_credentials_from_sdk(region: &str) -> Result<DuckDbS3Credentials> {
    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region.to_owned()))
        .load()
        .await;
    let provider = config
        .credentials_provider()
        .ok_or_else(|| anyhow!("AWS default credential chain did not configure a provider"))?;
    let credentials = provider
        .provide_credentials()
        .await
        .context("AWS default credential chain did not return credentials")?;
    Ok(DuckDbS3Credentials {
        access_key_id: credentials.access_key_id().to_owned(),
        secret_access_key: credentials.secret_access_key().to_owned(),
        session_token: credentials.session_token().map(ToOwned::to_owned),
        region: region.to_owned(),
    })
}

fn credentials_from_env(region: &str) -> Option<DuckDbS3Credentials> {
    Some(DuckDbS3Credentials {
        access_key_id: optional_env("AWS_ACCESS_KEY_ID")?,
        secret_access_key: optional_env("AWS_SECRET_ACCESS_KEY")?,
        session_token: optional_env("AWS_SESSION_TOKEN"),
        region: region.to_owned(),
    })
}

fn aws_region() -> String {
    optional_env("AWS_REGION")
        .or_else(|| optional_env("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|| "us-east-1".to_owned())
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn set_s3_credentials(conn: &Connection, credentials: &DuckDbS3Credentials) -> Result<()> {
    set_s3_setting(conn, "s3_access_key_id", &credentials.access_key_id)?;
    set_s3_setting(conn, "s3_secret_access_key", &credentials.secret_access_key)?;
    set_s3_setting(
        conn,
        "s3_session_token",
        credentials.session_token.as_deref().unwrap_or(""),
    )?;
    set_s3_setting(conn, "s3_region", &credentials.region)?;
    Ok(())
}

fn set_s3_setting(conn: &Connection, name: &str, value: &str) -> Result<()> {
    conn.execute_batch(&format!("SET {name} = '{}';", escape_sql_literal(value)))
        .with_context(|| format!("failed to set {name}"))
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn resolves_env_credentials_through_default_chain_and_sets_duckdb_s3_settings() -> Result<()> {
        let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");
        let env_guard = EnvGuard::set(&[
            ("AWS_ACCESS_KEY_ID", Some("test-access")),
            ("AWS_SECRET_ACCESS_KEY", Some("test-secret")),
            ("AWS_SESSION_TOKEN", Some("test-token")),
            ("AWS_REGION", Some("ap-southeast-5")),
            ("AWS_DEFAULT_REGION", Some("us-east-2")),
        ]);

        let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
        conn.execute_batch("INSTALL httpfs; LOAD httpfs;")
            .context("load httpfs extension")?;
        tokio::runtime::Runtime::new()
            .context("create tokio runtime")?
            .block_on(super::resolve_and_set_s3_credentials(&conn))?;

        assert_eq!(duckdb_setting(&conn, "s3_access_key_id")?, "test-access");
        assert_eq!(
            duckdb_setting(&conn, "s3_secret_access_key")?,
            "test-secret"
        );
        assert_eq!(duckdb_setting(&conn, "s3_session_token")?, "test-token");
        assert_eq!(duckdb_setting(&conn, "s3_region")?, "ap-southeast-5");

        drop(env_guard);
        Ok(())
    }

    fn duckdb_setting(conn: &Connection, name: &str) -> Result<String> {
        conn.query_row(
            "SELECT value FROM duckdb_settings() WHERE name = ?",
            [name],
            |row| row.get(0),
        )
        .with_context(|| format!("read DuckDB setting `{name}`"))
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, Option<&str>)]) -> Self {
            let saved = vars
                .iter()
                .map(|(name, _)| (*name, env::var(name).ok()))
                .collect::<Vec<_>>();
            for (name, value) in vars {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.drain(..).rev() {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }
}
