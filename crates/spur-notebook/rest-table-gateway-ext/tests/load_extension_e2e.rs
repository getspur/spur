use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir should be created");
    dir
}

fn write_temp_manifest(prefix: &str, contents: &str) -> PathBuf {
    let dir = unique_temp_dir(prefix);
    let path = dir.join("manifest.toml");
    std::fs::write(&path, contents).expect("manifest should be written");
    path
}

fn build_extension() -> PathBuf {
    let crate_dir = crate_dir();
    let output = Command::new(crate_dir.join("scripts/build.sh"))
        .current_dir(&crate_dir)
        .output()
        .expect("build script should run");

    assert!(
        output.status.success(),
        "build script failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("build stdout should be utf8");
    stdout
        .lines()
        .last()
        .map(PathBuf::from)
        .expect("build script should print extension artifact path")
}

fn run_load_harness(extension_path: &Path) -> String {
    let crate_dir = crate_dir();
    let output = Command::new("cargo")
        .arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(crate_dir.join("tests/load-harness/Cargo.toml"))
        .arg("--")
        .arg(extension_path)
        .env(
            "CARGO_TARGET_DIR",
            std::env::temp_dir().join("spur-rest-table-gateway-ext-harness-target"),
        )
        .output()
        .expect("load harness should run");

    assert!(
        output.status.success(),
        "load harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).expect("harness stdout should be utf8")
}

const ACTION_HARNESS_SOURCE: &str = r#"
use std::env;
use std::path::Path;

use duckdb::{Config, Connection};

fn sql_string(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn connect(extension_path: &Path) -> duckdb::Result<Connection> {
    let config = Config::default().allow_unsigned_extensions()?;
    let conn = Connection::open_in_memory_with_flags(config)?;
    conn.execute(&format!("LOAD '{}'", sql_string(extension_path)), [])?;
    Ok(conn)
}

fn query_action(conn: &Connection, sql: &str) -> duckdb::Result<(i64, Option<String>)> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    assert_eq!(rows.len(), 1, "{sql} should return exactly one row");
    Ok(rows.into_iter().next().unwrap())
}

fn assert_ok_body(body: Option<String>, verb: &str) {
    let body = body.expect("action body should be non-null");
    let value: serde_json::Value =
        serde_json::from_str(&body).expect("action body should be JSON");
    assert_eq!(value["ok"], true);
    assert_eq!(value["verb"], verb);
}

fn assert_write_actions(conn: &Connection) -> duckdb::Result<()> {
    let cases = [
        (
            "POST",
            "SELECT http_status, body FROM svc_create(price := 0.5)",
        ),
        (
            "PUT",
            "SELECT http_status, body FROM svc_replace(id := '1', price := 0.6)",
        ),
        (
            "PATCH",
            "SELECT http_status, body FROM svc_modify(id := '1', price := 0.7)",
        ),
        (
            "DELETE",
            "SELECT http_status, body FROM svc_remove(id := '1')",
        ),
    ];

    for (verb, sql) in cases {
        let (status, body) = query_action(conn, sql)?;
        assert_eq!(status, 200, "{verb} should return HTTP 200");
        assert_ok_body(body, verb);
    }

    println!("write-actions ok");
    Ok(())
}

fn assert_action_missing(conn: &Connection) -> duckdb::Result<()> {
    let result: duckdb::Result<()> = (|| {
        let mut stmt = conn.prepare("SELECT http_status FROM svc_create(price := 0.5)")?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    })();

    assert!(
        result.is_err(),
        "svc_create should not be callable when allow_writes is omitted"
    );
    println!("gate ok: {:?}", result.err().unwrap());
    Ok(())
}

fn assert_dry_run(conn: &Connection) -> duckdb::Result<()> {
    let (status, body) = query_action(
        conn,
        "SELECT http_status, body FROM svc_create(price := 0.5, dry_run := true)",
    )?;
    assert_eq!(status, 0, "dry run should use the composed-request status");

    let body = body.expect("dry-run body should be non-null");
    let request: serde_json::Value =
        serde_json::from_str(&body).expect("dry-run body should be JSON");
    assert_eq!(request["dry_run"], true);
    assert_eq!(request["method"], "POST");
    assert!(
        request["url"]
            .as_str()
            .expect("dry-run url should be a string")
            .ends_with("/orders")
    );
    assert_eq!(request["body"]["price"], 0.5);

    println!("dry-run ok");
    Ok(())
}

fn assert_typed_action(conn: &Connection) -> duckdb::Result<()> {
    let mut stmt = conn.prepare("SELECT id, score FROM demo_search(q := 'needle') ORDER BY id")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<duckdb::Result<Vec<_>>>()?;

    assert_eq!(
        rows,
        vec![("1".to_string(), 7), ("2".to_string(), 11)],
        "typed action table function should return declared columns"
    );
    println!("typed-action ok");
    Ok(())
}

fn main() -> duckdb::Result<()> {
    let extension_path = env::args()
        .nth(1)
        .expect("usage: action-harness <extension-path> <scenario>");
    let scenario = env::args()
        .nth(2)
        .expect("usage: action-harness <extension-path> <scenario>");

    let conn = connect(Path::new(&extension_path))?;
    match scenario.as_str() {
        "write-actions" => assert_write_actions(&conn)?,
        "gate" => assert_action_missing(&conn)?,
        "dry-run" => assert_dry_run(&conn)?,
        "typed-action" => assert_typed_action(&conn)?,
        other => panic!("unknown action harness scenario: {other}"),
    }

    Ok(())
}
"#;

fn run_action_harness(extension_path: &Path, scenario: &str) -> String {
    let dir = unique_temp_dir(&format!("spur-rest-action-harness-{scenario}"));
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir).expect("harness src dir should be created");
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "spur-rest-table-gateway-action-harness"
version = "0.1.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
duckdb = { version = "=1.10502.0", features = ["bundled"] }
serde_json = "1"
"#,
    )
    .expect("harness Cargo.toml should be written");
    std::fs::write(src_dir.join("main.rs"), ACTION_HARNESS_SOURCE)
        .expect("harness main should be written");

    let output = Command::new("cargo")
        .arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(dir.join("Cargo.toml"))
        .arg("--")
        .arg(extension_path)
        .arg(scenario)
        .env(
            "CARGO_TARGET_DIR",
            std::env::temp_dir().join("spur-rest-table-gateway-ext-action-harness-target"),
        )
        .output()
        .expect("action harness should run");

    assert!(
        output.status.success(),
        "action harness failed\nscenario: {scenario}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).expect("action harness stdout should be utf8")
}

#[test]
fn registers_manifest_dir_connections() {
    let _guard = ENV_LOCK.lock().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("limit", "500"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "m1",
                    "question": "Will this loadable extension pass?",
                    "active": true,
                    "volume": "782375.55"
                },
                {
                    "id": "m2",
                    "question": "Will string volume stay non-null?",
                    "active": true,
                    "volume": "12.25"
                }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/book"))
            .and(query_param("token_id", "0xabc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bids": [
                    { "price": "0.51", "size": "120" },
                    { "price": "0.50", "size": "80" }
                ]
            })))
            .mount(&server)
            .await;

        let dir = unique_temp_dir("spur-rest-manifest-dir");
        std::fs::write(
            dir.join("custom.toml"),
            "[source]\nname = \"custom\"\nbase_url = \"http://127.0.0.1:1\"\n\
             [[table]]\nname = \"items\"\npath = \"/items\"\n\
             [table.columns]\nid = { json = \"$.id\", type = \"Utf8\" }\n",
        )
        .unwrap();

        let _gamma = EnvGuard::set("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        let _clob = EnvGuard::set("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        let _manifest_dir = EnvGuard::set("SPUR_REST_MANIFEST_DIR", dir.as_os_str());
        let _manifest = EnvGuard::remove("SPUR_REST_MANIFEST");
        let _expected = EnvGuard::set("SPUR_REST_EXPECT_FUNCTION", "custom_items");
        let _install_dir = EnvGuard::set(
            "SPUR_EXT_INSTALL_DIR",
            std::env::temp_dir().join(format!(
                "spur-rest-table-gateway-ext-install-manifest-dir-{}",
                std::process::id()
            )),
        );

        let extension_path = build_extension();
        let output = run_load_harness(&extension_path);
        println!("{output}");
        assert!(output.contains("custom_items registered"));
    });
}

#[test]
fn write_actions_all_verbs_e2e() {
    let _guard = ENV_LOCK.lock().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;

        for (verb, endpoint) in [
            ("POST", "/orders"),
            ("PUT", "/orders/1"),
            ("PATCH", "/orders/1"),
            ("DELETE", "/orders/1"),
        ] {
            Mock::given(method(verb))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "verb": verb
                })))
                .mount(&server)
                .await;
        }

        let manifest = write_temp_manifest(
            "spur-rest-write-actions",
            &format!(
                r#"[source]
name = "svc"
base_url = "{base}"
allow_writes = true

[[action]]
name = "create"
method = "POST"
path = "/orders"
[action.args]
price = {{ in = "body", type = "Float64", required = true }}

[[action]]
name = "replace"
method = "PUT"
path = "/orders/{{id}}"
[action.args]
id = {{ in = "path", type = "Utf8", required = true }}
price = {{ in = "body", type = "Float64", required = true }}

[[action]]
name = "modify"
method = "PATCH"
path = "/orders/{{id}}"
[action.args]
id = {{ in = "path", type = "Utf8", required = true }}
price = {{ in = "body", type = "Float64", required = true }}

[[action]]
name = "remove"
method = "DELETE"
path = "/orders/{{id}}"
[action.args]
id = {{ in = "path", type = "Utf8", required = true }}
"#,
                base = server.uri()
            ),
        );

        let empty_manifest_dir = unique_temp_dir("spur-rest-empty-manifest-dir");
        let _gamma = EnvGuard::set("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        let _clob = EnvGuard::set("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        let _manifest = EnvGuard::set("SPUR_REST_MANIFEST", manifest.as_os_str());
        let _manifest_dir = EnvGuard::set("SPUR_REST_MANIFEST_DIR", empty_manifest_dir.as_os_str());
        let _allow_writes = EnvGuard::remove("SPUR_REST_ALLOW_WRITES");
        let _expected = EnvGuard::remove("SPUR_REST_EXPECT_FUNCTION");
        let _install_dir = EnvGuard::set(
            "SPUR_EXT_INSTALL_DIR",
            std::env::temp_dir().join(format!(
                "spur-rest-table-gateway-ext-install-write-actions-{}",
                std::process::id()
            )),
        );

        let extension_path = build_extension();
        let output = run_action_harness(&extension_path, "write-actions");
        println!("{output}");
        assert!(output.contains("write-actions ok"));

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 4);
        for (verb, endpoint) in [
            ("POST", "/orders"),
            ("PUT", "/orders/1"),
            ("PATCH", "/orders/1"),
            ("DELETE", "/orders/1"),
        ] {
            let request = requests
                .iter()
                .find(|request| request.method.as_str() == verb && request.url.path() == endpoint)
                .unwrap_or_else(|| panic!("{verb} {endpoint} should be sent"));
            if verb != "DELETE" {
                let body: serde_json::Value = request.body_json().expect("request body JSON");
                assert!(
                    body.get("price").is_some(),
                    "{verb} should send price in body"
                );
            }
        }
    });
}

#[test]
fn action_not_registered_without_allow_writes() {
    let _guard = ENV_LOCK.lock().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true
            })))
            .mount(&server)
            .await;

        let manifest = write_temp_manifest(
            "spur-rest-write-actions-gated",
            &format!(
                r#"[source]
name = "svc"
base_url = "{base}"

[[action]]
name = "create"
method = "POST"
path = "/orders"
[action.args]
price = {{ in = "body", type = "Float64", required = true }}
"#,
                base = server.uri()
            ),
        );

        let empty_manifest_dir = unique_temp_dir("spur-rest-empty-manifest-dir");
        let _gamma = EnvGuard::set("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        let _clob = EnvGuard::set("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        let _manifest = EnvGuard::set("SPUR_REST_MANIFEST", manifest.as_os_str());
        let _manifest_dir = EnvGuard::set("SPUR_REST_MANIFEST_DIR", empty_manifest_dir.as_os_str());
        let _allow_writes = EnvGuard::remove("SPUR_REST_ALLOW_WRITES");
        let _expected = EnvGuard::remove("SPUR_REST_EXPECT_FUNCTION");
        let _install_dir = EnvGuard::set(
            "SPUR_EXT_INSTALL_DIR",
            std::env::temp_dir().join(format!(
                "spur-rest-table-gateway-ext-install-gated-actions-{}",
                std::process::id()
            )),
        );

        let extension_path = build_extension();
        let output = run_action_harness(&extension_path, "gate");
        println!("{output}");
        assert!(output.contains("gate ok"));

        let requests = server.received_requests().await.expect("requests");
        assert!(
            requests.is_empty(),
            "unregistered action should not send HTTP"
        );
    });
}

#[test]
fn dry_run_sends_nothing() {
    let _guard = ENV_LOCK.lock().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true
            })))
            .mount(&server)
            .await;

        let manifest = write_temp_manifest(
            "spur-rest-write-actions-dry-run",
            &format!(
                r#"[source]
name = "svc"
base_url = "{base}"
allow_writes = true

[[action]]
name = "create"
method = "POST"
path = "/orders"
dry_run_arg = "dry_run"
[action.args]
price = {{ in = "body", type = "Float64", required = true }}
"#,
                base = server.uri()
            ),
        );

        let empty_manifest_dir = unique_temp_dir("spur-rest-empty-manifest-dir");
        let _gamma = EnvGuard::set("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        let _clob = EnvGuard::set("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        let _manifest = EnvGuard::set("SPUR_REST_MANIFEST", manifest.as_os_str());
        let _manifest_dir = EnvGuard::set("SPUR_REST_MANIFEST_DIR", empty_manifest_dir.as_os_str());
        let _allow_writes = EnvGuard::remove("SPUR_REST_ALLOW_WRITES");
        let _expected = EnvGuard::remove("SPUR_REST_EXPECT_FUNCTION");
        let _install_dir = EnvGuard::set(
            "SPUR_EXT_INSTALL_DIR",
            std::env::temp_dir().join(format!(
                "spur-rest-table-gateway-ext-install-dry-run-actions-{}",
                std::process::id()
            )),
        );

        let extension_path = build_extension();
        let output = run_action_harness(&extension_path, "dry-run");
        println!("{output}");
        assert!(output.contains("dry-run ok"));

        let requests = server.received_requests().await.expect("requests");
        assert!(
            requests.is_empty(),
            "dry_run := true should not send an HTTP request"
        );
    });
}

#[test]
fn load_extension_queries_action_as_typed_table_function() {
    let _guard = ENV_LOCK.lock().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/q"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "v": { "id": "1", "score": 7 } },
                    { "v": { "id": "2", "score": 11 } }
                ]
            })))
            .mount(&server)
            .await;

        let manifest = write_temp_manifest(
            "spur-rest-typed-action",
            &format!(
                r#"[source]
name = "demo"
base_url = "{base}"
allow_writes = true

[[action]]
name = "search"
method = "POST"
path = "/q"
response_path = "$.results"

[action.args]
q = {{ in = "body", type = "Utf8", required = true }}

[action.columns]
id = {{ json = "$.v.id", type = "Utf8" }}
score = {{ json = "$.v.score", type = "Int64" }}
"#,
                base = server.uri()
            ),
        );

        let empty_manifest_dir = unique_temp_dir("spur-rest-empty-manifest-dir");
        let _gamma = EnvGuard::set("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        let _clob = EnvGuard::set("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        let _manifest = EnvGuard::set("SPUR_REST_MANIFEST", manifest.as_os_str());
        let _manifest_dir = EnvGuard::set("SPUR_REST_MANIFEST_DIR", empty_manifest_dir.as_os_str());
        let _allow_writes = EnvGuard::remove("SPUR_REST_ALLOW_WRITES");
        let _expected = EnvGuard::remove("SPUR_REST_EXPECT_FUNCTION");
        let _install_dir = EnvGuard::set(
            "SPUR_EXT_INSTALL_DIR",
            std::env::temp_dir().join(format!(
                "spur-rest-table-gateway-ext-install-typed-action-{}",
                std::process::id()
            )),
        );

        let extension_path = build_extension();
        let output = run_action_harness(&extension_path, "typed-action");
        println!("{output}");
        assert!(output.contains("typed-action ok"));

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method.as_str(), "POST");
        assert_eq!(requests[0].url.path(), "/q");
        let body: serde_json::Value = requests[0].body_json().expect("request body JSON");
        assert_eq!(body["q"], "needle");
    });
}

#[test]
fn load_extension_queries_polymarket_markets_from_mock_rest_api() {
    let _guard = ENV_LOCK.lock().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("limit", "500"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "m1",
                    "question": "Will this loadable extension pass?",
                    "active": true,
                    "volume": "782375.55"
                },
                {
                    "id": "m2",
                    "question": "Will string volume stay non-null?",
                    "active": true,
                    "volume": "12.25"
                }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/book"))
            .and(query_param("token_id", "0xabc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bids": [
                    { "price": "0.51", "size": "120" },
                    { "price": "0.50", "size": "80" }
                ]
            })))
            .mount(&server)
            .await;

        let _gamma = EnvGuard::set("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        let _clob = EnvGuard::set("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        let _linear_base_url = EnvGuard::set("SPUR_CONN_linear_base_url", server.uri());
        let _manifest_dir = EnvGuard::remove("SPUR_REST_MANIFEST_DIR");
        let _expected = EnvGuard::remove("SPUR_REST_EXPECT_FUNCTION");
        let _manifest = EnvGuard::set(
            "SPUR_REST_MANIFEST",
            crate_dir().join("tests/fixtures/linear_graphql_manifest.toml"),
        );
        let _install_dir = EnvGuard::set(
            "SPUR_EXT_INSTALL_DIR",
            std::env::temp_dir().join(format!(
                "spur-rest-table-gateway-ext-install-{}",
                std::process::id()
            )),
        );

        let extension_path = build_extension();
        assert!(
            extension_path.exists(),
            "extension artifact should exist at {}",
            extension_path.display()
        );

        let output = run_load_harness(&extension_path);
        println!("{output}");
        assert!(output.contains("polymarket_markets rows:"));
        assert!(output.contains("polymarket_orderbook rows:"));
        assert!(output.contains("linear_issues registered and bound"));
        assert!(output.contains("m1"));
        assert!(output.contains("782375.55"));
        assert!(output.contains("0.51"));
    });
}
