use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApisGuruSnapshot {
    pub retrieved_at: String,
    pub sha256: String,
    pub total_entries: usize,
    pub sources: Vec<ApiSpecSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiSpecSource {
    pub provider: String,
    pub source_kind: SpecSourceKind,
    pub spec_format: SpecFormat,
    pub url: String,
    pub version: Option<String>,
    pub title: Option<String>,
    pub provenance: String,
    pub license_status: LicenseStatus,
    pub confidence: MatchConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecSourceKind {
    ApisGuru,
    OfficialRepo,
    OfficialUrl,
    GoogleDiscovery,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecFormat {
    OpenApi2,
    OpenApi3,
    GoogleDiscovery,
    GraphqlSdl,
    GraphqlIntrospection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseStatus {
    Redistributable,
    UrlOnly,
    NeedsReview,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchConfidence {
    Exact,
    Strong,
    Candidate,
    Rejected,
}

impl ApisGuruSnapshot {
    pub fn parse(json: &str, retrieved_at: &str) -> Result<Self, serde_json::Error> {
        let entries = serde_json::from_str::<BTreeMap<String, ApisGuruApiEntry>>(json)?;
        let sha256 = sha256_hex(json.as_bytes());
        let mut sources = Vec::new();

        for (provider, entry) in entries {
            let preferred = entry.preferred.as_deref();
            let mut version_rows = entry.versions.into_iter().collect::<Vec<_>>();
            version_rows.sort_by(|left, right| {
                preferred
                    .map(|preferred| {
                        let left_preferred = left.0 == preferred;
                        let right_preferred = right.0 == preferred;
                        right_preferred.cmp(&left_preferred)
                    })
                    .filter(|ordering| !ordering.is_eq())
                    .unwrap_or_else(|| left.0.cmp(&right.0))
            });

            for (version, spec) in version_rows {
                sources.push(ApiSpecSource {
                    provider: provider.clone(),
                    source_kind: SpecSourceKind::ApisGuru,
                    spec_format: spec.spec_format(),
                    url: spec.swagger_url.clone(),
                    version: Some(version),
                    title: spec.info.and_then(|info| info.title),
                    provenance: spec.swagger_url,
                    license_status: LicenseStatus::NeedsReview,
                    confidence: MatchConfidence::Candidate,
                });
            }
        }

        Ok(Self {
            retrieved_at: retrieved_at.to_string(),
            sha256,
            total_entries: sources.len(),
            sources,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ApisGuruApiEntry {
    preferred: Option<String>,
    versions: BTreeMap<String, ApisGuruVersionEntry>,
}

#[derive(Debug, Deserialize)]
struct ApisGuruVersionEntry {
    info: Option<ApisGuruInfo>,
    #[serde(rename = "swaggerUrl")]
    swagger_url: String,
    #[serde(rename = "openapiVer")]
    openapi_ver: Option<String>,
    #[serde(rename = "swaggerVersion")]
    swagger_version: Option<String>,
}

impl ApisGuruVersionEntry {
    fn spec_format(&self) -> SpecFormat {
        if self
            .openapi_ver
            .as_deref()
            .is_some_and(|version| version.starts_with('3'))
        {
            return SpecFormat::OpenApi3;
        }

        if self
            .swagger_version
            .as_deref()
            .is_some_and(|version| version.starts_with('2'))
        {
            return SpecFormat::OpenApi2;
        }

        if self.swagger_url.ends_with("/swagger.json") || self.swagger_url.ends_with("swagger.yaml")
        {
            return SpecFormat::OpenApi2;
        }

        SpecFormat::OpenApi3
    }
}

#[derive(Debug, Deserialize)]
struct ApisGuruInfo {
    title: Option<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to string cannot fail");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_apis_guru_snapshot_flattens_versions_and_hashes_input() {
        let json = r#"{
          "github.com": {
            "preferred": "1.1.4",
            "versions": {
              "1.1.4": {
                "info": {"title": "GitHub v3 REST API"},
                "swaggerUrl": "https://api.apis.guru/v2/specs/github.com/1.1.4/openapi.json",
                "openapiVer": "3.0.0"
              },
              "1.1.3": {
                "info": {"title": "GitHub v3 REST API"},
                "swaggerUrl": "https://api.apis.guru/v2/specs/github.com/1.1.3/swagger.json",
                "swaggerVersion": "2.0"
              }
            }
          }
        }"#;

        let snapshot =
            ApisGuruSnapshot::parse(json, "2026-06-12T00:00:00Z").expect("snapshot parses");

        assert_eq!(snapshot.retrieved_at, "2026-06-12T00:00:00Z");
        assert_eq!(snapshot.total_entries, 2);
        assert_eq!(snapshot.sources.len(), 2);
        assert_eq!(snapshot.sha256.len(), 64);
        assert!(snapshot
            .sources
            .iter()
            .any(|source| source.version.as_deref() == Some("1.1.4")));
    }

    #[test]
    fn parse_apis_guru_snapshot_defaults_missing_preferred_and_formats() {
        let json = r#"{
          "example.com": {
            "versions": {
              "v2": {
                "info": {"title": "Example v2"},
                "swaggerUrl": "https://api.apis.guru/v2/specs/example.com/v2/openapi.json",
                "openapiVer": "3.1.0"
              },
              "v1": {
                "info": {"title": "Example v1"},
                "swaggerUrl": "https://api.apis.guru/v2/specs/example.com/v1/swagger.json",
                "swaggerVersion": "2.0"
              }
            }
          }
        }"#;

        let snapshot =
            ApisGuruSnapshot::parse(json, "2026-06-12T00:00:00Z").expect("snapshot parses");

        assert_eq!(snapshot.total_entries, 2);
        assert_eq!(
            snapshot
                .sources
                .iter()
                .map(|source| (
                    &source.provider,
                    source.version.as_deref(),
                    source.spec_format
                ))
                .collect::<Vec<_>>(),
            [
                (&"example.com".to_string(), Some("v1"), SpecFormat::OpenApi2),
                (&"example.com".to_string(), Some("v2"), SpecFormat::OpenApi3),
            ]
        );
    }

    #[test]
    fn parse_apis_guru_snapshot_sets_provenance_license_and_confidence() {
        let json = r#"{
          "stripe.com": {
            "preferred": "2020-08-27",
            "versions": {
              "2020-08-27": {
                "info": {"title": "Stripe API"},
                "swaggerUrl": "https://api.apis.guru/v2/specs/stripe.com/2020-08-27/openapi.json",
                "openapiVer": "3.0.1"
              }
            }
          }
        }"#;

        let snapshot =
            ApisGuruSnapshot::parse(json, "2026-06-12T00:00:00Z").expect("snapshot parses");
        let source = &snapshot.sources[0];

        assert_eq!(source.provider, "stripe.com");
        assert_eq!(source.title.as_deref(), Some("Stripe API"));
        assert_eq!(
            source.url,
            "https://api.apis.guru/v2/specs/stripe.com/2020-08-27/openapi.json"
        );
        assert_eq!(source.source_kind, SpecSourceKind::ApisGuru);
        assert_eq!(source.spec_format, SpecFormat::OpenApi3);
        assert_eq!(source.provenance, source.url);
        assert_eq!(source.license_status, LicenseStatus::NeedsReview);
        assert_eq!(source.confidence, MatchConfidence::Candidate);
    }

    #[test]
    fn parse_apis_guru_snapshot_hashes_exact_input_bytes() {
        let compact = r#"{"empty.example":{"versions":{}}}"#;
        let pretty = r#"{
          "empty.example": {
            "versions": {}
          }
        }"#;

        let compact_snapshot =
            ApisGuruSnapshot::parse(compact, "2026-06-12T00:00:00Z").expect("compact parses");
        let pretty_snapshot =
            ApisGuruSnapshot::parse(pretty, "2026-06-12T00:00:00Z").expect("pretty parses");
        let compact_snapshot_again =
            ApisGuruSnapshot::parse(compact, "2026-06-12T00:00:00Z").expect("compact parses");

        assert_eq!(compact_snapshot.sha256, compact_snapshot_again.sha256);
        assert_ne!(compact_snapshot.sha256, pretty_snapshot.sha256);
    }
}
