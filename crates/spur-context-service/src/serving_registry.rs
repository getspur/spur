use std::collections::{BTreeMap, BTreeSet};

use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SERVING_REGISTRY_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub uri: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServingPackage {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub revision_kind: String,
    pub refs: Vec<String>,
    pub generation: i64,
    pub graph_prefix_uri: String,
    pub graph_manifest: ArtifactRef,
    pub source_sidecar: ArtifactRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServingRegistry {
    pub schema_version: u32,
    pub generation: i64,
    pub packages: Vec<ServingPackage>,
}

#[derive(Debug, Deserialize)]
struct ServingRegistryEnvelope {
    schema_version: u32,
}

#[derive(Debug, Deserialize)]
struct LegacyServingPackageV1 {
    source: String,
    package: String,
    revision: String,
    generation: i64,
    graph_prefix_uri: String,
    graph_manifest: ArtifactRef,
    source_sidecar: ArtifactRef,
}

#[derive(Debug, Deserialize)]
struct LegacyServingRegistryV1 {
    schema_version: u32,
    generation: i64,
    packages: Vec<LegacyServingPackageV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCatalogRow {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub revision_kind: String,
    pub refs: Vec<String>,
    pub generation: Option<i64>,
    pub index_status: String,
    pub graph_manifest_uri: Option<String>,
    pub graph_manifest_sha256: Option<String>,
    pub graph_manifest_bytes: Option<u64>,
    pub source_sidecar_uri: Option<String>,
    pub source_sidecar_sha256: Option<String>,
    pub source_sidecar_bytes: Option<u64>,
}

#[derive(Debug, Error)]
pub enum ServingRegistryError {
    #[error("unsupported serving-registry schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("package generation {actual} does not match registry generation {expected}")]
    GenerationMismatch { expected: i64, actual: i64 },
    #[error("package field {0} is empty")]
    EmptyPackageField(&'static str),
    #[error("package identity ({identity_source}, {package}, {revision}) is duplicated")]
    DuplicatePackageIdentity {
        identity_source: String,
        package: String,
        revision: String,
    },
    #[error("package ref ({identity_source}, {package}, {ref_name}) is duplicated")]
    DuplicatePackageRef {
        identity_source: String,
        package: String,
        ref_name: String,
    },
    #[error("package ref is empty")]
    EmptyPackageRef,
    #[error("artifact reference {0} is incomplete")]
    MissingArtifactRef(&'static str),
    #[error("artifact URI {0} is not an S3 object URI")]
    InvalidArtifactUri(&'static str),
    #[error("artifact SHA-256 {0} is not 64 ASCII hexadecimal characters")]
    InvalidSha256(&'static str),
    #[error("artifact {0} declares zero bytes")]
    ZeroByteArtifact(&'static str),
    #[error("serving generation {generation} is incomplete: {reason}")]
    IncompleteServingGeneration { generation: i64, reason: String },
    #[error("failed to serialize serving registry: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl ServingRegistryError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedSchemaVersion(_) => "unsupported_schema_version",
            Self::GenerationMismatch { .. } => "generation_mismatch",
            Self::EmptyPackageField(_) => "empty_package_field",
            Self::DuplicatePackageIdentity { .. } => "duplicate_package_identity",
            Self::DuplicatePackageRef { .. } => "duplicate_package_ref",
            Self::EmptyPackageRef => "empty_package_ref",
            Self::MissingArtifactRef(_) => "missing_artifact_ref",
            Self::InvalidArtifactUri(_) => "invalid_artifact_uri",
            Self::InvalidSha256(_) => "invalid_sha256",
            Self::ZeroByteArtifact(_) => "zero_byte_artifact",
            Self::IncompleteServingGeneration { .. } => "incomplete_serving_generation",
            Self::Serialization(_) => "serialization_error",
        }
    }
}

impl ServingRegistry {
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, ServingRegistryError> {
        let envelope: ServingRegistryEnvelope = serde_json::from_slice(bytes)?;
        let registry = match envelope.schema_version {
            SERVING_REGISTRY_SCHEMA_VERSION => serde_json::from_slice(bytes)?,
            1 => Self::upgrade_v1(serde_json::from_slice(bytes)?)?,
            unsupported => {
                return Err(ServingRegistryError::UnsupportedSchemaVersion(unsupported));
            }
        };
        registry.validate()?;
        Ok(registry)
    }

    fn upgrade_v1(legacy: LegacyServingRegistryV1) -> Result<Self, ServingRegistryError> {
        debug_assert_eq!(legacy.schema_version, 1);
        let mut packages = legacy
            .packages
            .into_iter()
            .map(|package| {
                let revision_kind = if parse_semver_revision(&package.revision).is_some() {
                    "semver"
                } else {
                    "git_sha"
                };
                ServingPackage {
                    source: package.source,
                    package: package.package,
                    revision: package.revision,
                    revision_kind: revision_kind.to_owned(),
                    refs: Vec::new(),
                    generation: package.generation,
                    graph_prefix_uri: package.graph_prefix_uri,
                    graph_manifest: package.graph_manifest,
                    source_sidecar: package.source_sidecar,
                }
            })
            .collect::<Vec<_>>();

        let mut package_groups = BTreeMap::<(String, String), Vec<usize>>::new();
        for (index, package) in packages.iter().enumerate() {
            package_groups
                .entry((package.source.clone(), package.package.clone()))
                .or_default()
                .push(index);
        }
        for indices in package_groups.values() {
            let semantic_latest = indices
                .iter()
                .filter_map(|index| {
                    let package = &packages[*index];
                    parse_semver_revision(&package.revision)
                        .map(|version| (version, package.revision.clone(), *index))
                })
                .max()
                .map(|(_, _, index)| index);
            let latest = semantic_latest.or_else(|| {
                indices
                    .iter()
                    .copied()
                    .max_by_key(|index| packages[*index].revision.clone())
            });
            if let Some(index) = latest {
                packages[index].refs.push("latest".to_owned());
            }
        }

        Ok(Self {
            schema_version: SERVING_REGISTRY_SCHEMA_VERSION,
            generation: legacy.generation,
            packages,
        })
    }

    pub fn from_current_rows(
        generation: i64,
        rows: impl IntoIterator<Item = ServingCatalogRow>,
    ) -> Result<Self, ServingRegistryError> {
        let rows = rows.into_iter().collect::<Vec<_>>();
        if generation <= 0 || rows.is_empty() {
            return Err(ServingRegistryError::IncompleteServingGeneration {
                generation,
                reason: "no current package rows for the serving view".to_owned(),
            });
        }

        let mut packages = Vec::with_capacity(rows.len());
        for row in rows {
            let row_generation = row.generation.ok_or_else(|| {
                ServingRegistryError::IncompleteServingGeneration {
                    generation,
                    reason: format!(
                        "{}/{}/{} has no generation",
                        row.source, row.package, row.revision
                    ),
                }
            })?;
            if row_generation <= 0 || row_generation > generation {
                return Err(ServingRegistryError::IncompleteServingGeneration {
                    generation,
                    reason: format!(
                        "{}/{}/{} has invalid row generation {row_generation}",
                        row.source, row.package, row.revision
                    ),
                });
            }
            if row.index_status != "complete" {
                return Err(ServingRegistryError::IncompleteServingGeneration {
                    generation,
                    reason: format!(
                        "{}/{}/{} has index_status `{}`",
                        row.source, row.package, row.revision, row.index_status
                    ),
                });
            }

            let graph_manifest = required_artifact(
                generation,
                "graph_manifest",
                row.graph_manifest_uri,
                row.graph_manifest_sha256,
                row.graph_manifest_bytes,
            )?;
            let source_sidecar = required_artifact(
                generation,
                "source_sidecar",
                row.source_sidecar_uri,
                row.source_sidecar_sha256,
                row.source_sidecar_bytes,
            )?;
            let graph_prefix_uri = graph_manifest
                .uri
                .rsplit_once('/')
                .map(|(prefix, _)| format!("{prefix}/"))
                .ok_or_else(|| ServingRegistryError::IncompleteServingGeneration {
                    generation,
                    reason: format!(
                        "graph manifest URI `{}` has no object key",
                        graph_manifest.uri
                    ),
                })?;

            packages.push(ServingPackage {
                source: row.source,
                package: row.package,
                revision: row.revision,
                revision_kind: row.revision_kind,
                refs: row.refs,
                generation,
                graph_prefix_uri,
                graph_manifest,
                source_sidecar,
            });
        }

        let registry = Self {
            schema_version: SERVING_REGISTRY_SCHEMA_VERSION,
            generation,
            packages,
        };
        registry.validate()?;
        Ok(registry)
    }

    pub fn validate(&self) -> Result<(), ServingRegistryError> {
        if self.schema_version != SERVING_REGISTRY_SCHEMA_VERSION {
            return Err(ServingRegistryError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }

        let mut identities = BTreeSet::new();
        let mut package_refs = BTreeSet::new();
        for package in &self.packages {
            if package.generation != self.generation {
                return Err(ServingRegistryError::GenerationMismatch {
                    expected: self.generation,
                    actual: package.generation,
                });
            }

            validate_package_field("source", &package.source)?;
            validate_package_field("package", &package.package)?;
            validate_package_field("revision", &package.revision)?;
            validate_package_field("revision_kind", &package.revision_kind)?;
            validate_s3_uri("graph_prefix", &package.graph_prefix_uri)?;
            validate_artifact("graph_manifest", &package.graph_manifest)?;
            validate_artifact("source_sidecar", &package.source_sidecar)?;

            let identity = (
                package.source.as_str(),
                package.package.as_str(),
                package.revision.as_str(),
            );
            if !identities.insert(identity) {
                return Err(ServingRegistryError::DuplicatePackageIdentity {
                    identity_source: package.source.clone(),
                    package: package.package.clone(),
                    revision: package.revision.clone(),
                });
            }

            let mut local_refs = BTreeSet::new();
            for reference in &package.refs {
                if reference.trim().is_empty() {
                    return Err(ServingRegistryError::EmptyPackageRef);
                }
                if !local_refs.insert(reference.as_str())
                    || !package_refs.insert((
                        package.source.as_str(),
                        package.package.as_str(),
                        reference.as_str(),
                    ))
                {
                    return Err(ServingRegistryError::DuplicatePackageRef {
                        identity_source: package.source.clone(),
                        package: package.package.clone(),
                        ref_name: reference.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    pub fn resolve(
        &self,
        source: &str,
        package: &str,
        revision: &str,
    ) -> Result<Option<&ServingPackage>, ServingRegistryError> {
        self.validate()?;
        Ok(self.packages.iter().find(|entry| {
            entry.source == source && entry.package == package && entry.revision == revision
        }))
    }

    pub fn resolve_revision_or_ref(
        &self,
        source: &str,
        package: &str,
        revision_or_ref: &str,
    ) -> Result<Option<&ServingPackage>, ServingRegistryError> {
        self.validate()?;
        Ok(self
            .packages
            .iter()
            .find(|entry| {
                entry.source == source
                    && entry.package == package
                    && entry
                        .refs
                        .iter()
                        .any(|reference| reference == revision_or_ref)
            })
            .or_else(|| {
                self.packages.iter().find(|entry| {
                    entry.source == source
                        && entry.package == package
                        && entry.revision == revision_or_ref
                })
            }))
    }

    pub fn to_canonical_json(&self) -> Result<Vec<u8>, ServingRegistryError> {
        self.validate()?;
        let mut canonical = self.clone();
        for package in &mut canonical.packages {
            package.refs.sort();
        }
        canonical.packages.sort_by(|left, right| {
            (&left.source, &left.package, &left.revision).cmp(&(
                &right.source,
                &right.package,
                &right.revision,
            ))
        });
        Ok(serde_json::to_vec(&canonical)?)
    }
}

pub(crate) fn parse_semver_revision(revision: &str) -> Option<Version> {
    Version::parse(revision.strip_prefix('v').unwrap_or(revision)).ok()
}

fn required_artifact(
    generation: i64,
    name: &'static str,
    uri: Option<String>,
    sha256: Option<String>,
    bytes: Option<u64>,
) -> Result<ArtifactRef, ServingRegistryError> {
    match (uri, sha256, bytes) {
        (Some(uri), Some(sha256), Some(bytes))
            if !uri.trim().is_empty() && !sha256.trim().is_empty() && bytes > 0 =>
        {
            Ok(ArtifactRef { uri, sha256, bytes })
        }
        _ => Err(ServingRegistryError::IncompleteServingGeneration {
            generation,
            reason: format!("missing {name} URI/SHA-256/byte metadata"),
        }),
    }
}

fn validate_package_field(name: &'static str, value: &str) -> Result<(), ServingRegistryError> {
    if value.trim().is_empty() {
        return Err(ServingRegistryError::EmptyPackageField(name));
    }
    Ok(())
}

fn validate_artifact(
    name: &'static str,
    artifact: &ArtifactRef,
) -> Result<(), ServingRegistryError> {
    if artifact.uri.is_empty() || artifact.sha256.is_empty() {
        return Err(ServingRegistryError::MissingArtifactRef(name));
    }
    validate_s3_uri(name, &artifact.uri)?;
    if artifact.sha256.len() != 64 || !artifact.sha256.as_bytes().iter().all(u8::is_ascii_hexdigit)
    {
        return Err(ServingRegistryError::InvalidSha256(name));
    }
    if artifact.bytes == 0 {
        return Err(ServingRegistryError::ZeroByteArtifact(name));
    }
    Ok(())
}

fn validate_s3_uri(name: &'static str, uri: &str) -> Result<(), ServingRegistryError> {
    let valid = uri
        .strip_prefix("s3://")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(bucket, key)| !bucket.is_empty() && !key.is_empty());
    if !valid {
        return Err(ServingRegistryError::InvalidArtifactUri(name));
    }
    Ok(())
}
