use arrow_array::{BooleanArray, Int64Array, StringArray};
use spur_rest_table_gateway::adapter::manifest::{AuthCfg, Manifest};
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::{
    Adapter, Predicate, PredicateOp, ResolvedAuth, ScalarValue, ScanRequest,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn scan_request(table: &str, predicates: Vec<Predicate>) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates,
        projection: None,
        tvf_args: Vec::new(),
        auth: ResolvedAuth::None,
    }
}

#[tokio::test]
async fn github_supported_manifest_scans_advisories_with_bearer_auth() {
    let server = MockServer::start().await;
    let _token = EnvGuard::set("GITHUB_TOKEN", "ghp_mock_token");

    Mock::given(method("GET"))
        .and(path("/advisories"))
        .and(header("authorization", "Bearer ghp_mock_token"))
        .and(header("x-github-api-version", "2022-11-28"))
        .and(query_param("severity", "high"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "ghsa_id": "GHSA-xxxx-yyyy-zzzz",
                "cve_id": "CVE-2026-0001",
                "summary": "Mock advisory",
                "severity": "high",
                "published_at": "2026-06-01T00:00:00Z",
                "updated_at": "2026-06-02T00:00:00Z"
            }
        ])))
        .mount(&server)
        .await;

    let manifest_toml = include_str!("../connections/supported/github.connection.toml")
        .replace("https://api.github.com", &server.uri());
    let manifest = Manifest::from_toml(&manifest_toml).expect("github manifest parses");

    assert_eq!(manifest.source.name, "github");
    assert!(matches!(manifest.source.auth, AuthCfg::Bearer { ref env } if env == "GITHUB_TOKEN"));
    assert!(manifest
        .tables
        .iter()
        .any(|table| table.name == "security_advisories"));

    let adapter = ManifestAdapter::new(manifest);
    let batches = adapter
        .scan(scan_request(
            "security_advisories",
            vec![Predicate {
                column: "severity".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("high".to_string()),
            }],
        ))
        .await
        .expect("github advisories scan succeeds");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);

    let ghsa_ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("ghsa_id should be Utf8");
    let severities = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("severity should be Utf8");

    assert_eq!(ghsa_ids.value(0), "GHSA-xxxx-yyyy-zzzz");
    assert_eq!(severities.value(0), "high");
}

#[tokio::test]
async fn github_supported_manifest_scans_authenticated_repos() {
    let server = MockServer::start().await;
    let _token = EnvGuard::set("GITHUB_TOKEN", "ghp_mock_token");

    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .and(header("authorization", "Bearer ghp_mock_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 42,
                "name": "spur",
                "full_name": "acme/spur",
                "private": true,
                "html_url": "https://github.com/acme/spur",
                "default_branch": "main",
                "updated_at": "2026-06-12T00:00:00Z"
            }
        ])))
        .mount(&server)
        .await;

    let manifest_toml = include_str!("../connections/supported/github.connection.toml")
        .replace("https://api.github.com", &server.uri());
    let manifest = Manifest::from_toml(&manifest_toml).expect("github manifest parses");
    let adapter = ManifestAdapter::new(manifest);

    let batches = adapter
        .scan(scan_request(
            "authenticated_repos",
            vec![Predicate {
                column: "visibility".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("private".to_string()),
            }],
        ))
        .await
        .expect("github repos scan succeeds");

    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("id should be Int64");
    let names = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name should be Utf8");
    let private = batch
        .column(3)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("private should be Boolean");

    assert_eq!(ids.value(0), 42);
    assert_eq!(names.value(0), "spur");
    assert!(private.value(0));
}
