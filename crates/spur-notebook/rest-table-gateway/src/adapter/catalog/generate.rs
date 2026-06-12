use crate::adapter::manifest::Manifest;
use crate::adapter::nango::{manifest_to_toml, provider_to_manifest_stub, ProviderEntry};
use crate::adapter::openapi::{parse_spec, spec_to_tables, tables_to_toml};
use openapiv3::{OpenAPI, PathItem, ReferenceOr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedReviewedManifest {
    pub toml: String,
    pub table_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedCandidateManifest {
    pub toml: String,
    pub table_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateBlockedReason {
    MissingBaseUrl,
    UnsupportedAuth,
    ParseFailure,
    ZeroTables,
    UnsafeEndpointOnlySpecs,
    CandidateManifestParseFailed,
}

impl CandidateBlockedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingBaseUrl => "missing_base_url",
            Self::UnsupportedAuth => "unsupported_auth",
            Self::ParseFailure => "parse_failure",
            Self::ZeroTables => "zero_tables",
            Self::UnsafeEndpointOnlySpecs => "unsafe_endpoint_only_specs",
            Self::CandidateManifestParseFailed => "candidate_manifest_parse_failed",
        }
    }
}

pub fn generate_reviewed_manifest(
    provider: &str,
    provider_entry: &ProviderEntry,
    spec_text: &str,
) -> Result<GeneratedReviewedManifest, String> {
    let spec = parse_spec(spec_text)?;
    let tables = spec_to_tables(&spec);
    if tables.is_empty() {
        return Err(format!(
            "reviewed OpenAPI source for {provider} produced no supported tables"
        ));
    }

    let table_count = tables.len();
    let manifest = provider_to_manifest_stub(provider, provider_entry);

    let mut toml = manifest_to_toml(&manifest).replace(
        "# TODO: add [[table]] blocks (path/columns/filters)\n",
        "# Table blocks generated from an explicit reviewed local OpenAPI source.\n",
    );
    toml.push_str(&tables_to_toml(&tables));
    Manifest::from_toml(&toml).map_err(|err| {
        format!("generated reviewed manifest for {provider} failed to reparse: {err}")
    })?;

    Ok(GeneratedReviewedManifest { table_count, toml })
}

pub fn generate_candidate_manifest(
    provider: &str,
    provider_entry: &ProviderEntry,
    spec_source_key: &str,
    spec_url: &str,
    status: &str,
    spec_text: &str,
) -> Result<GeneratedCandidateManifest, CandidateBlockedReason> {
    if let Some(reason) = candidate_provider_blocked_reason(provider_entry) {
        return Err(reason);
    }

    let spec = parse_spec(spec_text).map_err(|_| CandidateBlockedReason::ParseFailure)?;
    let has_operations = spec_has_operations(&spec);
    let tables = spec_to_tables(&spec);
    if tables.is_empty() {
        return Err(if has_operations {
            CandidateBlockedReason::UnsafeEndpointOnlySpecs
        } else {
            CandidateBlockedReason::ZeroTables
        });
    }

    let table_count = tables.len();
    let manifest = provider_to_manifest_stub(provider, provider_entry);
    let mut toml = manifest_to_toml(&manifest).replace(
        "# TODO: add [[table]] blocks (path/columns/filters)\n",
        "# Experimental crosswalk candidate. This file is not supported until reviewed with provider E2E coverage.\n",
    );
    let provenance = candidate_provenance_comments(provider, spec_source_key, spec_url, status);
    toml = toml.replacen("\n[source]\n", &format!("\n{provenance}\n[source]\n"), 1);
    toml.push_str(&tables_to_toml(&tables));

    Manifest::from_toml(&toml).map_err(|_| CandidateBlockedReason::CandidateManifestParseFailed)?;

    Ok(GeneratedCandidateManifest { table_count, toml })
}

pub fn candidate_provider_blocked_reason(
    provider_entry: &ProviderEntry,
) -> Option<CandidateBlockedReason> {
    if provider_entry
        .proxy
        .as_ref()
        .and_then(|proxy| proxy.base_url.as_deref())
        .is_none_or(|base_url| base_url.trim().is_empty())
    {
        return Some(CandidateBlockedReason::MissingBaseUrl);
    }

    if !candidate_auth_supported(provider_entry.auth_mode.as_deref()) {
        return Some(CandidateBlockedReason::UnsupportedAuth);
    }

    None
}

fn candidate_auth_supported(auth_mode: Option<&str>) -> bool {
    matches!(
        auth_mode.map(normalize_auth_mode).as_deref(),
        None | Some("API_KEY" | "BASIC" | "NONE" | "OAUTH2")
    )
}

fn normalize_auth_mode(mode: &str) -> String {
    mode.trim().to_ascii_uppercase().replace('-', "_")
}

fn spec_has_operations(spec: &OpenAPI) -> bool {
    spec.paths.paths.values().any(|path_item| match path_item {
        ReferenceOr::Item(path_item) => path_item_has_operation(path_item),
        ReferenceOr::Reference { .. } => false,
    })
}

fn path_item_has_operation(path_item: &PathItem) -> bool {
    path_item.get.is_some()
        || path_item.put.is_some()
        || path_item.post.is_some()
        || path_item.delete.is_some()
        || path_item.options.is_some()
        || path_item.head.is_some()
        || path_item.patch.is_some()
        || path_item.trace.is_some()
}

fn candidate_provenance_comments(
    provider: &str,
    spec_source_key: &str,
    spec_url: &str,
    status: &str,
) -> String {
    [
        "# support_level: experimental_crosswalk".to_string(),
        format!("# nango_provider: {provider}"),
        format!("# spec_source_key: {spec_source_key}"),
        format!("# spec_url: {spec_url}"),
        format!("# status: {status}"),
    ]
    .join("\n")
}
