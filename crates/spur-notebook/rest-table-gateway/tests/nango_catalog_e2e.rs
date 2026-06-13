use serde_json::{json, Map, Value};
use spur_rest_table_gateway::adapter::manifest::Manifest;
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
zz-candidate:
  display_name: ZZ Candidate
  categories: [tests]
  auth_mode: API_KEY
  proxy:
    base_url: https://candidate.example.com
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
  },
  "metadata-only": {
    "preferred": "1.0.0",
    "versions": {
      "1.0.0": {
        "info": {"title": "Metadata Only API"},
        "swaggerUrl": "https://api.apis.guru/v2/specs/metadata-only/1.0.0/openapi.json",
        "openapiVer": "3.0.0"
      }
    }
  },
  "zz-candidate": {
    "preferred": "1.0.0",
    "versions": {
      "1.0.0": {
        "info": {"title": "ZZ Candidate API"},
        "swaggerUrl": "https://api.apis.guru/v2/specs/zz-candidate/1.0.0/openapi.json",
        "openapiVer": "3.0.0"
      }
    }
  }
}"#;

const GITHUB_PROVIDER_FIXTURE: &str = r#"
github:
  display_name: GitHub
  categories: [dev-tools]
  auth_mode: API_KEY
  proxy:
    base_url: https://api.github.com
    headers:
      Authorization: Bearer ${apiKey}
"#;

const OPENAPI_COLLECTION_FIXTURE: &str = r#"{
  "openapi": "3.0.0",
  "info": {"title": "GitHub REST API", "version": "1.0.0"},
  "paths": {
    "/user/repos": {
      "get": {
        "operationId": "list_repos",
        "responses": {
          "200": {
            "description": "repositories",
            "content": {
              "application/json": {
                "schema": {
                  "type": "array",
                  "items": {
                    "type": "object",
                    "properties": {
                      "id": {"type": "integer"},
                      "name": {"type": "string"}
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}"#;

const OPENAPI_EMPTY_FIXTURE: &str = r#"{
  "openapi": "3.0.0",
  "info": {"title": "Empty API", "version": "1.0.0"},
  "paths": {}
}"#;

const OPENAPI_UNSAFE_ENDPOINT_FIXTURE: &str = r#"{
  "openapi": "3.0.0",
  "info": {"title": "Endpoint API", "version": "1.0.0"},
  "paths": {
    "/account": {
      "get": {
        "operationId": "get_account",
        "responses": {
          "200": {
            "description": "account",
            "content": {
              "application/json": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "id": {"type": "string"},
                    "name": {"type": "string"}
                  }
                }
              }
            }
          }
        }
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

fn api_guru_coverage_fixture_with_spec_url(
    provider_count: usize,
    spec_row_count: usize,
    spec_url: Option<&str>,
) -> (String, String) {
    assert!(provider_count > 0);
    assert!(spec_row_count >= provider_count);

    let mut providers_yaml = String::new();
    let mut apis = Map::new();
    let extra_versions = spec_row_count - provider_count;

    for provider_index in 0..provider_count {
        let provider_key = format!("fixture-provider-{provider_index:03}");
        providers_yaml.push_str(&format!(
            r#"{provider_key}:
  display_name: Fixture Provider {provider_index:03}
  categories: [tests]
  auth_mode: API_KEY
  proxy:
    base_url: https://{provider_key}.example.com
"#
        ));

        let version_count = if provider_index == 0 {
            extra_versions + 1
        } else {
            1
        };
        let mut versions = Map::new();
        for version_index in 0..version_count {
            let version = format!("v{version_index:03}");
            versions.insert(
                version.clone(),
                json!({
                    "info": {"title": format!("Fixture Provider {provider_index:03}")},
                    "swaggerUrl": spec_url
                        .map(str::to_string)
                        .unwrap_or_else(|| format!(
                            "https://api.apis.guru/v2/specs/{provider_key}/{version}/openapi.json"
                        )),
                    "openapiVer": "3.0.0"
                }),
            );
        }
        apis.insert(
            provider_key,
            json!({
                "preferred": "v000",
                "versions": versions
            }),
        );
    }

    (providers_yaml, Value::Object(apis).to_string())
}

fn local_apis_guru_fixture(spec_urls: &[(&str, String)]) -> String {
    let mut apis = Map::new();

    for (provider, spec_url) in spec_urls {
        apis.insert(
            (*provider).to_string(),
            json!({
                "preferred": "1.0.0",
                "versions": {
                    "1.0.0": {
                        "info": {"title": format!("{provider} API")},
                        "swaggerUrl": spec_url,
                        "openapiVer": "3.0.0"
                    }
                }
            }),
        );
    }

    Value::Object(apis).to_string()
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
        "api_guru_fulfillment_matrix.json",
        "provider_spec_crosswalk.json",
        "coverage_summary.json",
    ] {
        assert!(out.join(file).exists(), "{file} should be written");
    }
    assert!(
        !out.join("connections").exists(),
        "unreviewed crosswalk rows should not generate manifests"
    );

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
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["provider"], "github-pat");
    assert_eq!(rows[1]["provider"], "metadata-only");
    assert_eq!(rows[2]["provider"], "stripe-api-key");
    assert_eq!(rows[3]["provider"], "zz-candidate");

    let summary: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("coverage_summary.json"))
            .expect("coverage summary should be readable"),
    )
    .expect("coverage summary should parse");
    assert_eq!(summary["provider_count"], 4);
    assert_eq!(summary["apis_guru_total_entries"], 4);
    assert_eq!(summary["crosswalk_row_count"], 4);
    assert_eq!(summary["matched_provider_count"], 4);

    let matrix: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("api_guru_fulfillment_matrix.json"))
            .expect("fulfillment matrix should be readable"),
    )
    .expect("fulfillment matrix should parse");
    let matrix_rows = matrix["rows"]
        .as_array()
        .expect("matrix rows should be an array");
    assert_eq!(matrix["provider_count"], 4);
    assert_eq!(matrix["spec_row_count"], 4);
    assert_eq!(matrix_rows.len(), 4);
    assert_eq!(matrix_rows[0]["provider_key"], "github-pat");
    assert_eq!(matrix_rows[0]["spec_source_key"], "github.com");
    assert_eq!(
        matrix_rows[0]["spec_url"],
        "https://api.apis.guru/v2/specs/github.com/1.1.4/openapi.json"
    );
    assert_eq!(matrix_rows[0]["status"], "Ready");
    assert_eq!(matrix_rows[0]["blocked_reason"], Value::Null);
    assert_eq!(
        matrix_rows[0]["supported_manifest"],
        "connections/supported/github.connection.toml"
    );
    assert_eq!(matrix_rows[0]["candidate_manifest"], Value::Null);
    assert_eq!(matrix_rows[0]["table_count"], 2);
    assert_eq!(matrix_rows[0]["action_count"], 0);
    assert_eq!(matrix_rows[1]["provider_key"], "metadata-only");
    assert_eq!(matrix_rows[1]["status"], "Blocked");
    assert_eq!(matrix_rows[1]["blocked_reason"], "missing_base_url");
    assert_eq!(matrix_rows[1]["candidate_manifest"], Value::Null);
    assert_eq!(matrix_rows[3]["provider_key"], "zz-candidate");
    assert_eq!(matrix_rows[3]["status"], "Candidate");
    assert_eq!(matrix_rows[3]["blocked_reason"], Value::Null);
    assert_eq!(matrix_rows[3]["supported_manifest"], Value::Null);
    assert_eq!(
        matrix_rows[3]["candidate_manifest"],
        "connections/experimental/zz-candidate--zz-candidate.connection.toml"
    );
    assert_eq!(matrix_rows[3]["table_count"], 0);
    assert_eq!(matrix_rows[3]["action_count"], 0);

    let harvest = std::fs::read_to_string(out.join("provider_harvest_candidates.csv"))
        .expect("harvest csv should be readable");
    assert_eq!(harvest.lines().count(), 5);
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
fn nango_catalog_cli_can_write_experimental_crosswalk_manifests() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = unique_temp_dir("nango-catalog-experimental");
    let providers = temp.join("providers.yaml");
    let apis = temp.join("list.json");
    let spec = temp.join("collection.openapi.json");
    let out = temp.join("out");

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    let local_spec_url = format!("file://{}", spec.display());
    let apis_guru_fixture = APIS_GURU_FIXTURE
        .replace(
            "https://api.apis.guru/v2/specs/github.com/1.1.4/openapi.json",
            &local_spec_url,
        )
        .replace(
            "https://api.apis.guru/v2/specs/stripe.com/2020-08-27/openapi.json",
            &local_spec_url,
        )
        .replace(
            "https://api.apis.guru/v2/specs/zz-candidate/1.0.0/openapi.json",
            &local_spec_url,
        );
    std::fs::write(&providers, PROVIDERS_FIXTURE).expect("providers should be written");
    std::fs::write(&apis, apis_guru_fixture).expect("apis guru list should be written");
    std::fs::write(&spec, OPENAPI_COLLECTION_FIXTURE).expect("spec should be written");

    let output = Command::new(bin)
        .arg(&providers)
        .arg(&apis)
        .arg(&out)
        .arg("--nango-commit")
        .arg("988efd014")
        .arg("--apis-guru-fetched-at")
        .arg("2026-06-12T00:00:00Z")
        .arg("--experimental-crosswalk-manifests")
        .output()
        .expect("nango-catalog should run");

    assert!(
        output.status.success(),
        "nango-catalog failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let experimental_dir = out.join("connections").join("experimental");
    let github_manifest_path = experimental_dir.join("github-pat--github.com.connection.toml");
    let stripe_manifest_path = experimental_dir.join("stripe-api-key--stripe.com.connection.toml");
    assert!(
        github_manifest_path.exists(),
        "github experimental manifest should be written"
    );
    assert!(
        stripe_manifest_path.exists(),
        "stripe experimental manifest should be written"
    );

    let github_manifest =
        std::fs::read_to_string(&github_manifest_path).expect("manifest should be readable");
    assert!(github_manifest.contains("Experimental crosswalk candidate"));
    assert!(github_manifest.contains("# support_level: experimental_crosswalk"));
    assert!(github_manifest.contains("# nango_provider: github-pat"));
    assert!(github_manifest.contains("# spec_source_key: github.com"));
    assert!(github_manifest.contains(&format!("# spec_url: {local_spec_url}")));
    assert!(github_manifest.contains("# status: Candidate"));
    assert!(
        !github_manifest.contains("\nsupport_level ="),
        "experimental manifests should stay production-shaped and keep metadata out of TOML keys"
    );
    assert!(
        !github_manifest.contains("\nspec_url ="),
        "experimental manifests should keep spec provenance in comments and the sidecar index"
    );

    let parsed = Manifest::from_toml(&github_manifest).expect("experimental manifest should parse");
    assert_eq!(parsed.source.name, "github-pat");
    assert_eq!(parsed.tables.len(), 1);

    let index_path = out.join("experimental_manifest_index.json");
    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(index_path).expect("experimental index should be readable"),
    )
    .expect("experimental index should parse");
    assert_eq!(index["experimental"], true);
    assert_eq!(index["crosswalk_row_count"], 4);
    assert_eq!(index["manifest_count"], 3);
    assert_eq!(index["manifests"][0]["provider"], "github-pat");
    assert_eq!(
        index["manifests"][0]["path"],
        "connections/experimental/github-pat--github.com.connection.toml"
    );

    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn nango_catalog_cli_generates_candidates_and_blocks_non_generatable_specs() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = unique_temp_dir("nango-catalog-candidate-blocked");
    let providers = temp.join("providers.yaml");
    let apis = temp.join("list.json");
    let safe_spec = temp.join("safe.openapi.json");
    let parse_fail_spec = temp.join("parse-fail.openapi.json");
    let zero_spec = temp.join("zero.openapi.json");
    let unsafe_spec = temp.join("unsafe.openapi.json");
    let out = temp.join("out");

    let providers_yaml = r#"
safe-candidate:
  display_name: Safe Candidate
  categories: [tests]
  auth_mode: API_KEY
  proxy:
    base_url: https://safe.example.com
missing-base:
  display_name: Missing Base
  categories: [tests]
  auth_mode: API_KEY
unsupported-auth:
  display_name: Unsupported Auth
  categories: [tests]
  auth_mode: OAUTH1
  proxy:
    base_url: https://unsupported.example.com
parse-fail:
  display_name: Parse Fail
  categories: [tests]
  auth_mode: API_KEY
  proxy:
    base_url: https://parse-fail.example.com
zero-tables:
  display_name: Zero Tables
  categories: [tests]
  auth_mode: API_KEY
  proxy:
    base_url: https://zero.example.com
unsafe-endpoint:
  display_name: Unsafe Endpoint
  categories: [tests]
  auth_mode: API_KEY
  proxy:
    base_url: https://unsafe.example.com
"#;
    let apis_guru_json = local_apis_guru_fixture(&[
        ("safe-candidate", format!("file://{}", safe_spec.display())),
        ("missing-base", format!("file://{}", safe_spec.display())),
        (
            "unsupported-auth",
            format!("file://{}", safe_spec.display()),
        ),
        (
            "parse-fail",
            format!("file://{}", parse_fail_spec.display()),
        ),
        ("zero-tables", format!("file://{}", zero_spec.display())),
        (
            "unsafe-endpoint",
            format!("file://{}", unsafe_spec.display()),
        ),
    ]);

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    std::fs::write(&providers, providers_yaml).expect("providers should be written");
    std::fs::write(&apis, apis_guru_json).expect("apis guru list should be written");
    std::fs::write(&safe_spec, OPENAPI_COLLECTION_FIXTURE).expect("safe spec should be written");
    std::fs::write(&parse_fail_spec, "{").expect("bad spec should be written");
    std::fs::write(&zero_spec, OPENAPI_EMPTY_FIXTURE).expect("zero spec should be written");
    std::fs::write(&unsafe_spec, OPENAPI_UNSAFE_ENDPOINT_FIXTURE)
        .expect("unsafe spec should be written");

    let output = Command::new(bin)
        .arg(&providers)
        .arg(&apis)
        .arg(&out)
        .arg("--nango-commit")
        .arg("988efd014")
        .arg("--apis-guru-fetched-at")
        .arg("2026-06-12T00:00:00Z")
        .arg("--experimental-crosswalk-manifests")
        .output()
        .expect("nango-catalog should run");

    assert!(
        output.status.success(),
        "nango-catalog failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let matrix: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("api_guru_fulfillment_matrix.json"))
            .expect("fulfillment matrix should be readable"),
    )
    .expect("fulfillment matrix should parse");
    let rows = matrix["rows"]
        .as_array()
        .expect("matrix rows should be an array");
    let row_by_provider = |provider: &str| {
        rows.iter()
            .find(|row| row["provider_key"] == provider)
            .unwrap_or_else(|| panic!("{provider} row should be present"))
    };

    let safe = row_by_provider("safe-candidate");
    assert_eq!(safe["status"], "Candidate");
    assert_eq!(safe["blocked_reason"], Value::Null);
    assert_eq!(safe["table_count"], 1);
    assert_eq!(
        safe["candidate_manifest"],
        "connections/experimental/safe-candidate--safe-candidate.connection.toml"
    );

    for (provider, reason) in [
        ("missing-base", "missing_base_url"),
        ("unsupported-auth", "unsupported_auth"),
        ("parse-fail", "parse_failure"),
        ("zero-tables", "zero_tables"),
        ("unsafe-endpoint", "unsafe_endpoint_only_specs"),
    ] {
        let row = row_by_provider(provider);
        assert_eq!(row["status"], "Blocked");
        assert_eq!(row["blocked_reason"], reason);
        assert_eq!(row["candidate_manifest"], Value::Null);
        assert_eq!(row["table_count"], 0);
    }

    let manifest_path = out
        .join("connections")
        .join("experimental")
        .join("safe-candidate--safe-candidate.connection.toml");
    let manifest_toml =
        std::fs::read_to_string(&manifest_path).expect("candidate manifest should be readable");
    assert!(manifest_toml.contains("# nango_provider: safe-candidate"));
    assert!(manifest_toml.contains("# spec_source_key: safe-candidate"));
    assert!(manifest_toml.contains(&format!("# spec_url: file://{}", safe_spec.display())));
    assert!(manifest_toml.contains("# status: Candidate"));
    assert!(manifest_toml.contains("[[table]]"));

    let parsed = Manifest::from_toml(&manifest_toml).expect("candidate manifest should parse");
    assert_eq!(parsed.source.name, "safe-candidate");
    assert_eq!(parsed.tables.len(), 1);
    assert_eq!(parsed.tables[0].name, "list_repos");

    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("experimental_manifest_index.json"))
            .expect("experimental index should be readable"),
    )
    .expect("experimental index should parse");
    assert_eq!(index["crosswalk_row_count"], 6);
    assert_eq!(index["manifest_count"], 1);

    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn nango_catalog_cli_marks_committed_supported_manifests_ready() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = unique_temp_dir("nango-catalog-ready");
    let providers = temp.join("providers.yaml");
    let apis = temp.join("list.json");
    let out = temp.join("out");

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    std::fs::write(&providers, GITHUB_PROVIDER_FIXTURE).expect("providers should be written");
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

    let matrix: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("api_guru_fulfillment_matrix.json"))
            .expect("fulfillment matrix should be readable"),
    )
    .expect("fulfillment matrix should parse");
    let rows = matrix["rows"]
        .as_array()
        .expect("matrix rows should be an array");

    assert_eq!(matrix["provider_count"], 1);
    assert_eq!(matrix["spec_row_count"], 1);
    assert_eq!(rows[0]["provider_key"], "github");
    assert_eq!(rows[0]["status"], "Ready");
    assert_eq!(rows[0]["blocked_reason"], Value::Null);
    assert_eq!(
        rows[0]["supported_manifest"],
        "connections/supported/github.connection.toml"
    );
    assert_eq!(rows[0]["candidate_manifest"], Value::Null);
    assert_eq!(rows[0]["table_count"], 2);
    assert_eq!(rows[0]["action_count"], 0);

    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn nango_catalog_cli_covers_full_fulfillment_matrix_dimensions() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = unique_temp_dir("nango-catalog-coverage");
    let providers = temp.join("providers.yaml");
    let apis = temp.join("list.json");
    let spec = temp.join("collection.openapi.json");
    let out = temp.join("out");
    let local_spec_url = format!("file://{}", spec.display());
    let (providers_yaml, apis_guru_json) =
        api_guru_coverage_fixture_with_spec_url(87, 295, Some(&local_spec_url));

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    std::fs::write(&providers, providers_yaml).expect("providers should be written");
    std::fs::write(&apis, apis_guru_json).expect("apis guru list should be written");
    std::fs::write(&spec, OPENAPI_COLLECTION_FIXTURE).expect("spec should be written");

    let output = Command::new(bin)
        .arg(&providers)
        .arg(&apis)
        .arg(&out)
        .arg("--nango-commit")
        .arg("988efd014")
        .arg("--apis-guru-fetched-at")
        .arg("2026-06-12T00:00:00Z")
        .arg("--experimental-crosswalk-manifests")
        .output()
        .expect("nango-catalog should run");

    assert!(
        output.status.success(),
        "nango-catalog failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let matrix: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("api_guru_fulfillment_matrix.json"))
            .expect("fulfillment matrix should be readable"),
    )
    .expect("fulfillment matrix should parse");
    let rows = matrix["rows"]
        .as_array()
        .expect("matrix rows should be an array");

    assert_eq!(matrix["provider_count"], 87);
    assert_eq!(matrix["spec_row_count"], 295);
    assert_eq!(rows.len(), 295);
    assert_eq!(rows[0]["provider_key"], "fixture-provider-000");
    assert_eq!(rows[0]["status"], "Candidate");
    assert_eq!(
        rows[0]["candidate_manifest"],
        "connections/experimental/fixture-provider-000--fixture-provider-000.connection.toml"
    );
    assert_eq!(
        rows[1]["candidate_manifest"],
        "connections/experimental/fixture-provider-000--fixture-provider-000-2.connection.toml"
    );
    assert!(rows.iter().all(|row| row["status"] == "Candidate"));
    assert!(rows.iter().all(|row| row["table_count"] == 1));

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

#[test]
fn nango_catalog_cli_generates_parseable_reviewed_manifest() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = unique_temp_dir("nango-catalog-reviewed-source");
    let providers = temp.join("providers.yaml");
    let apis = temp.join("list.json");
    let spec = temp.join("github.openapi.json");
    let out = temp.join("out");

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    std::fs::write(&providers, GITHUB_PROVIDER_FIXTURE).expect("providers should be written");
    std::fs::write(&apis, APIS_GURU_FIXTURE).expect("apis guru list should be written");
    std::fs::write(&spec, OPENAPI_COLLECTION_FIXTURE).expect("openapi spec should be written");

    let output = Command::new(bin)
        .arg(&providers)
        .arg(&apis)
        .arg(&out)
        .arg("--nango-commit")
        .arg("988efd014")
        .arg("--apis-guru-fetched-at")
        .arg("2026-06-12T00:00:00Z")
        .arg("--reviewed-source")
        .arg(format!("github={}", spec.display()))
        .output()
        .expect("nango-catalog should run");

    assert!(
        output.status.success(),
        "nango-catalog failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest_path = out.join("connections").join("github.connection.toml");
    let manifest_toml =
        std::fs::read_to_string(&manifest_path).expect("generated manifest should be readable");
    let parsed = Manifest::from_toml(&manifest_toml).expect("generated manifest should parse");

    assert_eq!(parsed.source.name, "github");
    assert_eq!(parsed.tables.len(), 1);
    assert_eq!(parsed.tables[0].name, "list_repos");
    assert_eq!(manifest_toml.matches("[[table]]").count(), 1);

    std::fs::remove_dir_all(temp).ok();
}
