use crate::adapter::manifest::Manifest;
use crate::adapter::nango::{manifest_to_toml, provider_to_manifest_stub, ProviderEntry};
use crate::adapter::openapi::{parse_spec, spec_to_tables, tables_to_toml};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedReviewedManifest {
    pub toml: String,
    pub table_count: usize,
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
