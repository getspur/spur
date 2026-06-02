use std::fs;
use std::path::PathBuf;
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

#[ignore = "requires python+duckdb via uv; run explicitly as the live capstone"]
#[test]
fn python_kernel_recipe_loads_spur_rest_for_bare_duckdb_sql() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;
        let items_server = MockServer::start().await;

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

        Mock::given(method("GET"))
            .and(path("/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "item-1"
                },
                {
                    "id": "item-2"
                }
            ])))
            .mount(&items_server)
            .await;

        let manifest_dir = std::env::temp_dir().join(format!(
            "spur-rest-pykernel-manifests-{}",
            std::process::id()
        ));
        fs::create_dir_all(&manifest_dir).expect("manifest dir should be created");
        fs::write(
            manifest_dir.join("custom.toml"),
            format!(
                "[source]\nname = \"custom\"\nbase_url = \"{}\"\n\
                 [[table]]\nname = \"items\"\npath = \"/items\"\n\
                 [table.columns]\nid = {{ json = \"$.id\", type = \"Utf8\" }}\n",
                items_server.uri()
            ),
        )
        .expect("saved manifest should be written");

        std::env::set_var("SPUR_POLYMARKET_GAMMA_BASE", server.uri());
        std::env::set_var("SPUR_POLYMARKET_CLOB_BASE", server.uri());
        std::env::set_var("SPUR_REST_MANIFEST_DIR", &manifest_dir);
        std::env::remove_var("SPUR_REST_MANIFEST");
        std::env::set_var(
            "SPUR_EXT_INSTALL_DIR",
            std::env::temp_dir().join(format!(
                "spur-rest-pykernel-install-{}",
                std::process::id()
            )),
        );

        let extension_path = build_extension();
        assert!(
            extension_path.exists(),
            "extension artifact should exist at {}",
            extension_path.display()
        );

        // DuckDB derives the C-API init symbol from the loaded file stem, so
        // the installed/LOADed extension must use the bare spur_rest stem.
        let bare_extension_dir = std::env::temp_dir().join(format!(
            "spur-rest-pykernel-load-{}",
            std::process::id()
        ));
        fs::create_dir_all(&bare_extension_dir).expect("bare extension dir should be created");
        let bare_extension_path = bare_extension_dir.join("spur_rest.duckdb_extension");
        fs::copy(&extension_path, &bare_extension_path)
            .expect("bare-named extension copy should be written");

        let script_path = std::env::temp_dir().join(format!(
            "spur-rest-python-kernel-recipe-{}.py",
            std::process::id()
        ));
        let artifact_path = bare_extension_path.to_string_lossy().replace('"', "\\\"");
        let script = format!(
            r#"# MIRROR of api_tables_setup_bootstrap_preamble in crates/spur-notebook/src/mcp/mod.rs - keep in sync
import duckdb
_p = r"{artifact_path}"
con = duckdb.connect(database=":memory:", config={{"allow_unsigned_extensions": "true"}})
duckdb.set_default_connection(con)
duckdb.sql("LOAD '" + _p.replace("'", "''") + "'")
print("polymarket_markets rows:", duckdb.sql("SELECT id, volume FROM polymarket_markets() ORDER BY id").fetchall())
print("polymarket_orderbook rows:", duckdb.sql("SELECT price, size FROM polymarket_orderbook(token_id := '0xabc', depth := 1) ORDER BY price DESC").fetchall())
print("custom_items rows:", duckdb.sql("SELECT id FROM custom_items() ORDER BY id").fetchall())
"#
        );
        fs::write(&script_path, script).expect("python recipe script should be written");

        let output = Command::new("uv")
            .args(["run", "--with", "duckdb==1.5.3", "python"])
            .arg(&script_path)
            .output()
            .expect("uv python recipe should run");

        assert!(
            output.status.success(),
            "python recipe failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8(output.stdout).expect("python stdout should be utf8");
        println!("{stdout}");
        assert!(stdout.contains("m1"));
        assert!(stdout.contains("782375.55"));
        assert!(stdout.contains("0.51"));
        assert!(stdout.contains("custom_items rows:"));
        assert!(stdout.contains("item-1"));
    });
}
