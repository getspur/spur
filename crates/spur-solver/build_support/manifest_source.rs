use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use crate::manifest_format::{
    validate_family_manifest, validate_manifest_bundle, validate_rule_manifest, FamilyManifestV1,
    ManifestBundleV1, ManifestValidationError, NativeHandlerV1, RuleManifestV1, SchemaVersionV1,
};

const APPROVED_FAMILIES: [&str; 6] = [
    "accessibility",
    "configuration",
    "design",
    "policy",
    "resource",
    "scheduling",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedManifestSources {
    pub bundle: ManifestBundleV1,
    pub source_paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct ManifestSourceError {
    source_path: PathBuf,
    owner: String,
    message: String,
}

impl ManifestSourceError {
    fn new(
        source_path: impl Into<PathBuf>,
        owner: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            owner: owner.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ManifestSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({}): {}",
            self.source_path.display(),
            self.owner,
            self.message
        )
    }
}

impl std::error::Error for ManifestSourceError {}

#[derive(Clone, Debug)]
enum SourceKind {
    Family { directory_owner: String },
    Rule { directory_owner: String },
}

#[derive(Clone, Debug)]
struct ManifestSource {
    path: PathBuf,
    kind: SourceKind,
}

#[derive(Clone, Debug)]
struct FamilySource {
    manifest: FamilyManifestV1,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct RuleSource {
    manifest: RuleManifestV1,
    path: PathBuf,
    directory_owner: String,
}

pub fn load_manifest_sources(root: &Path) -> Result<LoadedManifestSources, ManifestSourceError> {
    let sources = discover_manifest_sources(root)?;
    let source_paths = sources.iter().map(|source| source.path.clone()).collect();
    let mut families = Vec::with_capacity(APPROVED_FAMILIES.len());
    let mut rules = Vec::new();

    for source in sources {
        let fallback_owner = source.fallback_owner();
        let contents = fs::read_to_string(&source.path).map_err(|error| {
            ManifestSourceError::new(
                &source.path,
                &fallback_owner,
                format!("failed to read YAML source: {error}"),
            )
        })?;
        match source.kind {
            SourceKind::Family { directory_owner } => {
                let mut family: FamilyManifestV1 =
                    serde_yml::from_str(&contents).map_err(|error| {
                        ManifestSourceError::new(
                            &source.path,
                            format!("family `{directory_owner}`"),
                            format!("failed to parse strict YAML: {error}"),
                        )
                    })?;
                validate_family_manifest(&family).map_err(|error| {
                    ManifestSourceError::new(
                        &source.path,
                        format!("family `{}`", family.id),
                        error.to_string(),
                    )
                })?;
                if family.id != directory_owner {
                    return Err(ManifestSourceError::new(
                        &source.path,
                        format!("family `{}`", family.id),
                        format!(
                            "family id must match approved source directory `{directory_owner}`"
                        ),
                    ));
                }
                family
                    .profiles
                    .sort_by(|left, right| left.id.cmp(&right.id));
                families.push(FamilySource {
                    manifest: family,
                    path: source.path,
                });
            }
            SourceKind::Rule { directory_owner } => {
                let rule: RuleManifestV1 = serde_yml::from_str(&contents).map_err(|error| {
                    ManifestSourceError::new(
                        &source.path,
                        &fallback_owner,
                        format!("failed to parse strict YAML: {error}"),
                    )
                })?;
                validate_rule_manifest(&rule).map_err(|error| {
                    ManifestSourceError::new(
                        &source.path,
                        format!("rule `{}`", rule.id),
                        error.to_string(),
                    )
                })?;
                rules.push(RuleSource {
                    manifest: rule,
                    path: source.path,
                    directory_owner,
                });
            }
        }
    }

    families.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
    rules.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));

    let bundle = ManifestBundleV1 {
        schema_version: SchemaVersionV1,
        families: families
            .iter()
            .map(|source| source.manifest.clone())
            .collect(),
        rules: rules.iter().map(|source| source.manifest.clone()).collect(),
    };
    validate_manifest_bundle(&bundle)
        .map_err(|error| contextual_bundle_error(root, error, &families, &rules))?;
    validate_rule_source_ownership(&rules)?;
    validate_handler_bijection(root, &rules)?;

    Ok(LoadedManifestSources {
        bundle,
        source_paths,
    })
}

impl ManifestSource {
    fn fallback_owner(&self) -> String {
        match &self.kind {
            SourceKind::Family { directory_owner } => format!("family `{directory_owner}`"),
            SourceKind::Rule { .. } => {
                let stem = self
                    .path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown");
                format!("rule `{stem}`")
            }
        }
    }
}

fn discover_manifest_sources(root: &Path) -> Result<Vec<ManifestSource>, ManifestSourceError> {
    let mut sources = Vec::new();
    for family in APPROVED_FAMILIES {
        let family_dir = root.join(family);
        sources.push(ManifestSource {
            path: family_dir.join("family.yaml"),
            kind: SourceKind::Family {
                directory_owner: family.to_owned(),
            },
        });

        let rules_dir = family_dir.join("rules");
        let entries = fs::read_dir(&rules_dir).map_err(|error| {
            ManifestSourceError::new(
                &rules_dir,
                format!("family `{family}` rules"),
                format!("failed to discover approved rule sources: {error}"),
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                ManifestSourceError::new(
                    &rules_dir,
                    format!("family `{family}` rules"),
                    format!("failed to inspect rule source: {error}"),
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                ManifestSourceError::new(
                    entry.path(),
                    format!("family `{family}` rules"),
                    format!("failed to inspect rule source type: {error}"),
                )
            })?;
            let path = entry.path();
            if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("yaml")
            {
                sources.push(ManifestSource {
                    path,
                    kind: SourceKind::Rule {
                        directory_owner: family.to_owned(),
                    },
                });
            }
        }
    }
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(sources)
}

fn contextual_bundle_error(
    root: &Path,
    error: ManifestValidationError,
    families: &[FamilySource],
    rules: &[RuleSource],
) -> ManifestSourceError {
    let (path, owner) = match &error {
        ManifestValidationError::DuplicateId { kind: "family", id } => families
            .iter()
            .rev()
            .find(|source| source.manifest.id == *id)
            .map(|source| (source.path.clone(), format!("family `{id}`"))),
        ManifestValidationError::DuplicateId {
            kind: "profile",
            id,
        } => families
            .iter()
            .rev()
            .find(|source| {
                source
                    .manifest
                    .profiles
                    .iter()
                    .any(|profile| profile.id == *id)
            })
            .map(|source| (source.path.clone(), format!("profile `{id}`"))),
        ManifestValidationError::DuplicateId { kind: "rule", id } => rules
            .iter()
            .rev()
            .find(|source| source.manifest.id == *id)
            .map(|source| (source.path.clone(), format!("rule `{id}`"))),
        ManifestValidationError::DuplicateHandler { second_rule, .. }
        | ManifestValidationError::UnknownFamily {
            rule_id: second_rule,
            ..
        }
        | ManifestValidationError::UnknownProfile {
            rule_id: second_rule,
            ..
        }
        | ManifestValidationError::ProfileFamilyMismatch {
            rule_id: second_rule,
            ..
        }
        | ManifestValidationError::InvalidRouting {
            rule_id: second_rule,
            ..
        }
        | ManifestValidationError::InvalidNativeHandlerContract {
            rule_id: second_rule,
            ..
        } => rules
            .iter()
            .find(|source| source.manifest.id == *second_rule)
            .map(|source| {
                (
                    source.path.clone(),
                    format!("rule `{}`", source.manifest.id),
                )
            }),
        ManifestValidationError::DuplicateId { .. }
        | ManifestValidationError::InvalidField { .. } => None,
    }
    .unwrap_or_else(|| (root.to_path_buf(), "manifest bundle".to_owned()));

    ManifestSourceError::new(path, owner, error.to_string())
}

fn validate_handler_bijection(
    root: &Path,
    rules: &[RuleSource],
) -> Result<(), ManifestSourceError> {
    let owners = rules
        .iter()
        .filter_map(|source| {
            source
                .manifest
                .handler
                .map(|handler| (handler, source.manifest.id.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    let missing = NativeHandlerV1::ALL
        .iter()
        .filter(|handler| !owners.contains_key(handler))
        .map(handler_name)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(ManifestSourceError::new(
        root,
        "manifest bundle",
        format!(
            "implemented-handler bijection is missing NativeHandlerV1 handler(s): {}",
            missing.join(", ")
        ),
    ))
}

fn validate_rule_source_ownership(rules: &[RuleSource]) -> Result<(), ManifestSourceError> {
    for source in rules {
        if source.manifest.family != source.directory_owner {
            return Err(ManifestSourceError::new(
                &source.path,
                format!("rule `{}`", source.manifest.id),
                format!(
                    "declared family `{}` must match approved source directory `{}`",
                    source.manifest.family, source.directory_owner
                ),
            ));
        }
    }
    Ok(())
}

fn handler_name(handler: &NativeHandlerV1) -> String {
    serde_json::to_value(handler)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{handler:?}"))
}

pub fn canonical_manifest_json(bundle: &ManifestBundleV1) -> Result<String, ManifestSourceError> {
    serde_json::to_string(bundle).map_err(|error| {
        ManifestSourceError::new(
            "spur_rule_manifests_v1.json",
            "manifest bundle",
            format!("failed to serialize canonical JSON: {error}"),
        )
    })
}

pub fn write_canonical_manifest(
    output: &Path,
    bundle: &ManifestBundleV1,
) -> Result<(), ManifestSourceError> {
    let json = canonical_manifest_json(bundle)?;
    fs::write(output, json).map_err(|error| {
        ManifestSourceError::new(
            output,
            "manifest bundle",
            format!("failed to write canonical JSON: {error}"),
        )
    })
}

pub fn manifest_rerun_paths(root: &Path, source_paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = BTreeSet::from([root.to_path_buf()]);
    for family in APPROVED_FAMILIES {
        paths.insert(root.join(family));
        paths.insert(root.join(family).join("rules"));
    }
    paths.extend(source_paths.iter().cloned());
    paths.into_iter().collect()
}
