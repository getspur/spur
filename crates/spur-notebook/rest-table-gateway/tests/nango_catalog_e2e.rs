use serde_json::Value;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const PROVIDERS_FIXTURE: &str = r#"
github-pat:
  display_name: GitHub
  categories: [dev-tools]
  auth_mode: API_KEY
  docs:
    - https://docs.github.com/rest/repos/repos
    - /user/repos
  proxy:
    base_url: https://api.github.com
    headers:
      Authorization: Bearer ${apiKey}
    verification:
      method: GET
      endpoint: /user
stripe-api-key:
  display_name: Stripe
  categories: [payments]
  auth_mode: API_KEY
  proxy:
    base_url: https://api.stripe.com
metadata-only:
  display_name: Metadata Only
  categories: [tests]
  auth_mode: NONE
"#;

const APIS_GURU_FIXTURE: &str = r#"{
  "stripe.com": {
    "preferred": "2020-08-27",
    "versions": {
      "2020-08-27": {
        "info": {"title": "Stripe API"},
        "swaggerUrl": "https://api.apis.guru/v2/specs/stripe.com/2020-08-27/openapi.json",
        "openapiVer": "3.0.1"
      }
    }
  },
  "github.com": {
    "preferred": "1.1.4",
    "versions": {
      "1.1.4": {
        "info": {"title": "GitHub v3 REST API"},
        "swaggerUrl": "https://api.apis.guru/v2/specs/github.com/1.1.4/openapi.json",
        "openapiVer": "3.0.0"
      }
    }
  }
}"#;

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after UNIX epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "spur-rest-table-gateway-{name}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn nango_catalog_cli_writes_deterministic_crosswalk_outputs() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = unique_temp_dir("nango-catalog");
    let providers = temp.join("providers.yaml");
    let apis = temp.join("list.json");
    let out = temp.join("out");

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    std::fs::write(&providers, PROVIDERS_FIXTURE).expect("providers should be written");
    std::fs::write(&apis, APIS_GURU_FIXTURE).expect("apis guru list should be written");

    let output = Command::new(bin)
        .arg(&providers)
        .arg(&apis)
        .arg(&out)
        .arg("--nango-commit")
        .arg("988efd014")
        .arg("--apis-guru-fetched-at")
        .arg("2026-06-12T00:00:00Z")
        .output()
        .expect("nango-catalog should run");

    assert!(
        output.status.success(),
        "nango-catalog failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    for file in [
        "provider_harvest_candidates.csv",
        "table_seed_classes.csv",
        "apis_guru_crosswalk.csv",
        "provider_spec_crosswalk.json",
        "coverage_summary.json",
    ] {
        assert!(out.join(file).exists(), "{file} should be written");
    }

    let crosswalk: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("provider_spec_crosswalk.json"))
            .expect("crosswalk json should be readable"),
    )
    .expect("crosswalk json should parse");
    assert_eq!(
        crosswalk["metadata"]["nango_license"],
        "Elastic License 2.0"
    );
    assert_eq!(crosswalk["metadata"]["nango_commit"], "988efd014");
    assert_eq!(
        crosswalk["metadata"]["apis_guru_retrieved_at"],
        "2026-06-12T00:00:00Z"
    );
    assert_eq!(
        crosswalk["metadata"]["apis_guru_sha256"]
            .as_str()
            .expect("hash should be a string")
            .len(),
        64
    );

    let rows = crosswalk["rows"]
        .as_array()
        .expect("rows should be an array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["provider"], "github-pat");
    assert_eq!(rows[1]["provider"], "stripe-api-key");

    let summary: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("coverage_summary.json"))
            .expect("coverage summary should be readable"),
    )
    .expect("coverage summary should parse");
    assert_eq!(summary["provider_count"], 3);
    assert_eq!(summary["apis_guru_total_entries"], 2);
    assert_eq!(summary["crosswalk_row_count"], 2);
    assert_eq!(summary["matched_provider_count"], 2);

    let harvest = std::fs::read_to_string(out.join("provider_harvest_candidates.csv"))
        .expect("harvest csv should be readable");
    assert_eq!(harvest.lines().count(), 4);
    assert!(harvest.contains("github-pat,GitHub,API_KEY,https://api.github.com"));

    let seeds = std::fs::read_to_string(out.join("table_seed_classes.csv"))
        .expect("seed csv should be readable");
    assert!(seeds.lines().any(|line| line == "MetadataOnly,1"));

    let apis_crosswalk = std::fs::read_to_string(out.join("apis_guru_crosswalk.csv"))
        .expect("apis crosswalk csv should be readable");
    assert!(apis_crosswalk
        .lines()
        .nth(1)
        .is_some_and(|line| line.starts_with("github-pat,github.com,")));

    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn nango_catalog_cli_requires_pinned_upstream_metadata() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = unique_temp_dir("nango-catalog-missing-metadata");
    let providers = temp.join("providers.yaml");
    let apis = temp.join("list.json");
    let out = temp.join("out");

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    std::fs::write(&providers, PROVIDERS_FIXTURE).expect("providers should be written");
    std::fs::write(&apis, APIS_GURU_FIXTURE).expect("apis guru list should be written");

    let output = Command::new(bin)
        .arg(&providers)
        .arg(&apis)
        .arg(&out)
        .arg("--nango-commit")
        .arg("988efd014")
        .output()
        .expect("nango-catalog should run");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--apis-guru-fetched-at is required"));

    std::fs::remove_dir_all(temp).ok();
}
