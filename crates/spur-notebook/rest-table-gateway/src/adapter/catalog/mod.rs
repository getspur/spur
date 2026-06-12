pub mod apis_guru;
pub mod crosswalk;
pub mod generate;
pub mod provider;

pub const APIS_GURU_CROSSWALK_CSV: &str = "apis_guru_crosswalk.csv";
pub const COVERAGE_SUMMARY_JSON: &str = "coverage_summary.json";
pub const PROVIDER_HARVEST_CANDIDATES_CSV: &str = "provider_harvest_candidates.csv";
pub const PROVIDER_SPEC_CROSSWALK_JSON: &str = "provider_spec_crosswalk.json";
pub const TABLE_SEED_CLASSES_CSV: &str = "table_seed_classes.csv";

pub use apis_guru::{
    ApiSpecSource, ApisGuruSnapshot, LicenseStatus, MatchConfidence, SpecFormat, SpecSourceKind,
};
pub use crosswalk::{
    build_crosswalk, build_crosswalk_report, CrosswalkDiagnostics, CrosswalkOptions,
    CrosswalkReport, ProviderSpecCrosswalk,
};
pub use generate::{generate_reviewed_manifest, GeneratedReviewedManifest};
pub use provider::{provider_catalog_from_yaml, ProviderCatalogEntry, ProviderSeedClass};
