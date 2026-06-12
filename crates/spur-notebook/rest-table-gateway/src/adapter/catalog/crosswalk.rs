use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::adapter::catalog::{
    ApiSpecSource, LicenseStatus, MatchConfidence, ProviderCatalogEntry, ProviderSeedClass,
    SpecSourceKind,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrosswalkOptions {
    pub apis_guru_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSpecCrosswalk {
    pub provider: String,
    pub spec_source_key: String,
    pub source_kind: SpecSourceKind,
    pub url: String,
    pub confidence: MatchConfidence,
    pub match_reasons: Vec<String>,
    pub license_status: LicenseStatus,
    pub nango_commit: String,
    pub apis_guru_hash: Option<String>,
    pub generation_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrosswalkReport {
    pub rows: Vec<ProviderSpecCrosswalk>,
    pub diagnostics: CrosswalkDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrosswalkDiagnostics {
    pub providers_by_seed_class: BTreeMap<ProviderSeedClass, usize>,
    pub total_spec_candidates: usize,
    pub distinct_matched_providers: usize,
    pub rejected_ambiguous_candidates: usize,
}

pub fn build_crosswalk(
    providers: &[ProviderCatalogEntry],
    sources: &[ApiSpecSource],
    options: CrosswalkOptions,
) -> Vec<ProviderSpecCrosswalk> {
    build_crosswalk_report(providers, sources, options).rows
}

pub fn build_crosswalk_report(
    providers: &[ProviderCatalogEntry],
    sources: &[ApiSpecSource],
    options: CrosswalkOptions,
) -> CrosswalkReport {
    let mut rows = Vec::new();

    let mut sorted_providers = providers.iter().collect::<Vec<_>>();
    sorted_providers.sort_by(|left, right| left.provider.cmp(&right.provider));

    let mut sorted_sources = sources.iter().collect::<Vec<_>>();
    sorted_sources.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.url.cmp(&right.url))
    });

    for provider in sorted_providers {
        let mut provider_rows = sorted_sources
            .iter()
            .filter_map(|source| match_source(provider, source, &options))
            .collect::<Vec<_>>();

        let best_confidence = provider_rows
            .iter()
            .map(|row| row.confidence)
            .min_by_key(|confidence| confidence_rank(*confidence));

        if let Some(best_confidence) = best_confidence {
            provider_rows.retain(|row| row.confidence == best_confidence);
            if best_confidence == MatchConfidence::Candidate && provider_rows.len() > 1 {
                for row in &mut provider_rows {
                    row.confidence = MatchConfidence::Rejected;
                    row.generation_eligible = false;
                    push_reason(&mut row.match_reasons, "ambiguous_candidate");
                }
            }
        }

        rows.extend(provider_rows);
    }

    rows.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| confidence_rank(left.confidence).cmp(&confidence_rank(right.confidence)))
            .then_with(|| left.spec_source_key.cmp(&right.spec_source_key))
            .then_with(|| left.url.cmp(&right.url))
    });

    let diagnostics = diagnostics(providers, sources, &rows);

    CrosswalkReport { rows, diagnostics }
}

fn match_source(
    provider: &ProviderCatalogEntry,
    source: &ApiSpecSource,
    options: &CrosswalkOptions,
) -> Option<ProviderSpecCrosswalk> {
    let mut reasons = Vec::new();
    let provider_key = normalize_key(&provider.provider);
    let source_key = normalize_key(&source.provider);
    let display_name = normalize_key(&provider.display_name);
    let source_title = source.title.as_deref().map(normalize_key);

    if provider.provider == source.provider {
        push_reason(&mut reasons, "exact_provider_key");
    }

    if manual_alias_target(&provider.provider)
        .is_some_and(|alias| alias_matches(alias, &source.provider))
    {
        push_reason(&mut reasons, "manual_alias");
    }

    if normalized_display_match(&display_name, &source_key)
        || source_title
            .as_deref()
            .is_some_and(|source_title| normalized_display_match(&display_name, source_title))
    {
        push_reason(&mut reasons, "normalized_display_name");
    }

    if host_overlap(provider.base_url.as_deref(), &source.provider, &source.url) {
        push_reason(&mut reasons, "host_overlap");
    }

    if docs_url_overlap(&provider.docs_endpoints, &source.provider, &source.url) {
        push_reason(&mut reasons, "docs_url_overlap");
    }

    if reasons.is_empty() || provider_key.is_empty() || source_key.is_empty() {
        return None;
    }

    reasons.sort();

    let confidence = confidence_for_reasons(&reasons);
    let generation_eligible = source.license_status == LicenseStatus::Redistributable
        && matches!(confidence, MatchConfidence::Exact | MatchConfidence::Strong);

    Some(ProviderSpecCrosswalk {
        provider: provider.provider.clone(),
        spec_source_key: source.provider.clone(),
        source_kind: source.source_kind,
        url: source.url.clone(),
        confidence,
        match_reasons: reasons,
        license_status: source.license_status,
        nango_commit: provider.nango_commit.clone(),
        apis_guru_hash: options.apis_guru_hash.clone(),
        generation_eligible,
    })
}

fn confidence_for_reasons(reasons: &[String]) -> MatchConfidence {
    if reasons.iter().any(|reason| reason == "exact_provider_key") {
        MatchConfidence::Exact
    } else if reasons.iter().any(|reason| {
        matches!(
            reason.as_str(),
            "manual_alias" | "host_overlap" | "docs_url_overlap"
        )
    }) {
        MatchConfidence::Strong
    } else {
        MatchConfidence::Candidate
    }
}

fn diagnostics(
    providers: &[ProviderCatalogEntry],
    sources: &[ApiSpecSource],
    rows: &[ProviderSpecCrosswalk],
) -> CrosswalkDiagnostics {
    let mut providers_by_seed_class = BTreeMap::new();
    for provider in providers {
        *providers_by_seed_class
            .entry(provider.seed_class)
            .or_insert(0) += 1;
    }

    let distinct_matched_providers = rows
        .iter()
        .filter(|row| row.confidence != MatchConfidence::Rejected)
        .map(|row| row.provider.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let rejected_ambiguous_candidates = rows
        .iter()
        .filter(|row| {
            row.confidence == MatchConfidence::Rejected
                && row
                    .match_reasons
                    .iter()
                    .any(|reason| reason == "ambiguous_candidate")
        })
        .count();

    CrosswalkDiagnostics {
        providers_by_seed_class,
        total_spec_candidates: sources.len(),
        distinct_matched_providers,
        rejected_ambiguous_candidates,
    }
}

fn confidence_rank(confidence: MatchConfidence) -> usize {
    match confidence {
        MatchConfidence::Exact => 0,
        MatchConfidence::Strong => 1,
        MatchConfidence::Candidate => 2,
        MatchConfidence::Rejected => 3,
    }
}

fn manual_alias_target(provider: &str) -> Option<&'static str> {
    match provider {
        "github-pat" => Some("github"),
        "stripe-api-key" => Some("stripe"),
        "sendgrid-api-key" => Some("sendgrid"),
        "twilio" => Some("twilio.com"),
        _ => None,
    }
}

fn alias_matches(alias: &str, source_provider: &str) -> bool {
    let alias = alias.to_ascii_lowercase();
    let source_provider = source_provider.to_ascii_lowercase();
    alias == source_provider
        || source_provider
            .strip_suffix(".com")
            .is_some_and(|source_root| source_root == alias)
}

fn normalized_display_match(display_name: &str, source_value: &str) -> bool {
    !display_name.is_empty()
        && (display_name == source_value
            || source_value == format!("{display_name}api")
            || source_value == format!("{display_name}restapi"))
}

fn host_overlap(base_url: Option<&str>, source_provider: &str, source_url: &str) -> bool {
    let Some(base_host) = base_url.and_then(host_from_url) else {
        return false;
    };
    let source_host = host_from_url(source_url);
    let source_provider = source_provider.to_ascii_lowercase();

    domain_overlap(&base_host, &source_provider)
        || source_host
            .as_deref()
            .is_some_and(|source_host| domain_overlap(&base_host, source_host))
}

fn docs_url_overlap(docs_endpoints: &[String], source_provider: &str, source_url: &str) -> bool {
    docs_endpoints.iter().any(|docs_endpoint| {
        let Some(docs_host) = host_from_url(docs_endpoint) else {
            return false;
        };
        domain_overlap(&docs_host, source_provider)
            || host_from_url(source_url)
                .as_deref()
                .is_some_and(|source_host| domain_overlap(&docs_host, source_host))
    })
}

fn domain_overlap(left: &str, right: &str) -> bool {
    let left = left.trim_start_matches("www.");
    let right = right.trim_start_matches("www.");
    left == right || left.ends_with(&format!(".{right}")) || right.ends_with(&format!(".{left}"))
}

fn host_from_url(value: &str) -> Option<String> {
    let value = value.trim();
    let after_scheme = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .split('@')
        .next_back()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if host.contains('.') {
        Some(host)
    } else {
        None
    }
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn push_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|existing| existing == reason) {
        reasons.push(reason.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::catalog::{
        ApiSpecSource, LicenseStatus, MatchConfidence, ProviderCatalogEntry, ProviderSeedClass,
        SpecFormat, SpecSourceKind,
    };
    use indexmap::IndexMap;

    fn provider_fixture(
        provider: &str,
        display_name: &str,
        base_url: Option<&str>,
    ) -> ProviderCatalogEntry {
        ProviderCatalogEntry {
            provider: provider.to_string(),
            display_name: display_name.to_string(),
            categories: Vec::new(),
            auth_mode: None,
            base_url: base_url.map(str::to_string),
            connection_config_keys: Vec::new(),
            credential_keys: Vec::new(),
            proxy_headers: IndexMap::new(),
            proxy_query: IndexMap::new(),
            proxy_body: IndexMap::new(),
            pagination: None,
            verification: Vec::new(),
            authorization_url: None,
            token_url: None,
            docs_endpoints: Vec::new(),
            seed_class: ProviderSeedClass::BaseUrlOnly,
            nango_license: "Elastic License 2.0".to_string(),
            nango_commit: "988efd014".to_string(),
        }
    }

    fn apis_guru_fixture(
        provider: &str,
        title: &str,
        url: &str,
        license_status: LicenseStatus,
    ) -> ApiSpecSource {
        ApiSpecSource {
            provider: provider.to_string(),
            source_kind: SpecSourceKind::ApisGuru,
            spec_format: SpecFormat::OpenApi3,
            url: url.to_string(),
            version: Some("1.0.0".to_string()),
            title: Some(title.to_string()),
            provenance: url.to_string(),
            license_status,
            confidence: MatchConfidence::Candidate,
        }
    }

    #[test]
    fn exact_match_is_generation_eligible_when_redistributable() {
        let provider = provider_fixture("github.com", "GitHub", Some("https://api.github.com"));
        let source = apis_guru_fixture(
            "github.com",
            "GitHub v3 REST API",
            "https://api.apis.guru/v2/specs/github.com/1.1.4/openapi.json",
            LicenseStatus::Redistributable,
        );

        let rows = build_crosswalk(&[provider], &[source], CrosswalkOptions::default());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "github.com");
        assert_eq!(rows[0].spec_source_key, "github.com");
        assert_eq!(rows[0].confidence, MatchConfidence::Exact);
        assert!(rows[0]
            .match_reasons
            .iter()
            .any(|reason| reason == "exact_provider_key"));
        assert!(rows[0].generation_eligible);
    }

    #[test]
    fn alias_match_can_be_strong_but_not_generated_without_redistributable_license() {
        let provider = provider_fixture("stripe-api-key", "Stripe", Some("https://api.stripe.com"));
        let source = apis_guru_fixture(
            "stripe.com",
            "Stripe",
            "https://api.apis.guru/v2/specs/stripe.com/openapi.json",
            LicenseStatus::NeedsReview,
        );

        let rows = build_crosswalk(&[provider], &[source], CrosswalkOptions::default());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].confidence, MatchConfidence::Strong);
        assert!(rows[0]
            .match_reasons
            .iter()
            .any(|reason| reason == "manual_alias"));
        assert!(!rows[0].generation_eligible);
    }

    #[test]
    fn host_overlap_match_is_strong_and_records_reason() {
        let provider = provider_fixture(
            "acme",
            "Acme",
            Some("https://api.us-west.acme.example.com/v1"),
        );
        let source = apis_guru_fixture(
            "acme.example.com",
            "Acme API",
            "https://api.apis.guru/v2/specs/acme.example.com/1.0/openapi.json",
            LicenseStatus::Redistributable,
        );

        let rows = build_crosswalk(&[provider], &[source], CrosswalkOptions::default());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].confidence, MatchConfidence::Strong);
        assert!(rows[0]
            .match_reasons
            .iter()
            .any(|reason| reason == "host_overlap"));
        assert!(rows[0].generation_eligible);
    }

    #[test]
    fn display_name_only_match_stays_candidate_and_not_generation_eligible() {
        let provider = provider_fixture("linear-oauth", "Linear", None);
        let source = apis_guru_fixture(
            "linear.app",
            "Linear",
            "https://api.apis.guru/v2/specs/linear.app/openapi.json",
            LicenseStatus::Redistributable,
        );

        let rows = build_crosswalk(&[provider], &[source], CrosswalkOptions::default());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].confidence, MatchConfidence::Candidate);
        assert!(rows[0]
            .match_reasons
            .iter()
            .any(|reason| reason == "normalized_display_name"));
        assert!(!rows[0].generation_eligible);
    }

    #[test]
    fn ambiguous_candidates_are_rejected_and_counted_in_diagnostics() {
        let provider = provider_fixture("notion", "Notion", None);
        let sources = [
            apis_guru_fixture(
                "notion-public",
                "Notion API",
                "https://api.apis.guru/v2/specs/notion-public/v1/openapi.json",
                LicenseStatus::Redistributable,
            ),
            apis_guru_fixture(
                "notion-private",
                "Notion API",
                "https://api.apis.guru/v2/specs/notion-private/v1/openapi.json",
                LicenseStatus::Redistributable,
            ),
        ];

        let report = build_crosswalk_report(&[provider], &sources, CrosswalkOptions::default());

        assert_eq!(report.rows.len(), 2);
        assert!(report
            .rows
            .iter()
            .all(|row| row.confidence == MatchConfidence::Rejected));
        assert!(report.rows.iter().all(|row| !row.generation_eligible));
        assert_eq!(report.diagnostics.total_spec_candidates, 2);
        assert_eq!(report.diagnostics.distinct_matched_providers, 0);
        assert_eq!(report.diagnostics.rejected_ambiguous_candidates, 2);
        assert_eq!(
            report
                .diagnostics
                .providers_by_seed_class
                .get(&ProviderSeedClass::BaseUrlOnly),
            Some(&1)
        );
    }
}
