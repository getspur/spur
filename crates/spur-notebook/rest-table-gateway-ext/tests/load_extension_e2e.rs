use std::path::{Path, PathBuf};
use std::process::Command;

use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

#[test]
fn load_extension_queries_polymarket_markets_from_mock_rest_api() {
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

        std::env::set_var("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        std::env::set_var("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        std::env::set_var(
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
        assert!(output.contains("m1"));
        assert!(output.contains("782375.55"));
        assert!(output.contains("0.51"));
    });
}
