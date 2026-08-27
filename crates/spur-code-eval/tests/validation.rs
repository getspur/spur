use std::io::Write;

use flate2::{write::GzEncoder, Compression};
use sha2::{Digest as _, Sha256};
use spur_code_eval::{validate_bytes, SourceError, SourceManifest, Suite};
use tar::{Builder, Header};
use xz2::write::XzEncoder;

const JSON_LINES: &[u8] = b"{\"id\":1}\n{\"id\":2}\n";
const JSON_LINES_SHA256: &str = "c63f6dd68b68601e7315ea40d28bc34e55379e4fa65f82b1d32228429aeafcde";
const IMMUTABLE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const ADAPTER_CONTRACT_VERSION: &str = "code-eval-source-v1";

fn source_block(
    revision: &str,
    licenses: &str,
    adapter_contract_version: &str,
    expected_record_count: u64,
) -> String {
    format!(
        r#"
[[sources]]
suite = "repo_qa"
uri = "https://example.invalid/source.jsonl"
revision = "{revision}"
sha256 = "{JSON_LINES_SHA256}"
licenses = {licenses}
expected_record_count = {expected_record_count}
format = "json_lines"
adapter_contract_version = "{adapter_contract_version}"

[[sources.languages]]
language = "python"
supported = true
expected_record_count = {expected_record_count}

[[sources.evidence]]
external_package = "local-fixture"
external_source = "local:test"
revision = "{IMMUTABLE_REVISION}"
selectors = ["fixture::record"]
observation = "two newline-delimited JSON records"
"#
    )
}

fn manifest_with_source(source: &str) -> String {
    format!("manifest_version = 1\n{source}")
}

fn valid_manifest() -> SourceManifest {
    SourceManifest::from_toml(&manifest_with_source(&source_block(
        IMMUTABLE_REVISION,
        r#"["Apache-2.0"]"#,
        ADAPTER_CONTRACT_VERSION,
        2,
    )))
    .unwrap()
}

fn compressed_manifest(
    suite: &str,
    format: &str,
    language: &str,
    expected_record_count: u64,
    bytes: &[u8],
) -> SourceManifest {
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    SourceManifest::from_toml(&format!(
        r#"
manifest_version = 1

[[sources]]
suite = "{suite}"
uri = "https://example.invalid/source.archive"
revision = "{IMMUTABLE_REVISION}"
sha256 = "{sha256}"
licenses = ["Apache-2.0"]
expected_record_count = {expected_record_count}
format = "{format}"
adapter_contract_version = "{ADAPTER_CONTRACT_VERSION}"

[[sources.languages]]
language = "{language}"
supported = true
expected_record_count = {expected_record_count}

[[sources.evidence]]
external_package = "local-fixture"
external_source = "local:test"
revision = "{IMMUTABLE_REVISION}"
selectors = ["fixture::record"]
observation = "generated compressed fixture"
"#
    ))
    .unwrap()
}

fn append_tar_file<W: Write>(builder: &mut Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(u64::try_from(bytes.len()).unwrap());
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes).unwrap();
}

#[test]
fn altered_source_byte_is_a_fatal_hash_mismatch() {
    let manifest = valid_manifest();
    let mut altered = JSON_LINES.to_vec();
    altered[0] ^= 1;

    let error = validate_bytes(&manifest.sources()[0], &altered).unwrap_err();

    assert!(matches!(error, SourceError::HashMismatch { .. }));
}

#[test]
fn valid_hash_with_wrong_record_count_is_fatal() {
    let manifest = SourceManifest::from_toml(&manifest_with_source(&source_block(
        IMMUTABLE_REVISION,
        r#"["Apache-2.0"]"#,
        ADAPTER_CONTRACT_VERSION,
        3,
    )))
    .unwrap();

    let error = validate_bytes(&manifest.sources()[0], JSON_LINES).unwrap_err();

    assert_eq!(
        error,
        SourceError::RecordCountMismatch {
            expected: 3,
            actual: 2,
        }
    );
}

#[test]
fn missing_license_metadata_is_fatal() {
    let error = SourceManifest::from_toml(&manifest_with_source(&source_block(
        IMMUTABLE_REVISION,
        "[]",
        ADAPTER_CONTRACT_VERSION,
        2,
    )))
    .unwrap_err();

    assert_eq!(
        error,
        SourceError::MissingLicenseMetadata { source_index: 0 }
    );
}

#[test]
fn mutable_source_revision_is_fatal() {
    let error = SourceManifest::from_toml(&manifest_with_source(&source_block(
        "main",
        r#"["Apache-2.0"]"#,
        ADAPTER_CONTRACT_VERSION,
        2,
    )))
    .unwrap_err();

    assert!(matches!(error, SourceError::MutableRevision { .. }));
}

#[test]
fn duplicate_source_identity_is_fatal() {
    let source = source_block(
        IMMUTABLE_REVISION,
        r#"["Apache-2.0"]"#,
        ADAPTER_CONTRACT_VERSION,
        2,
    );
    let manifest = format!("manifest_version = 1\n{source}{source}");

    let error = SourceManifest::from_toml(&manifest).unwrap_err();

    assert!(matches!(error, SourceError::DuplicateSourceIdentity { .. }));
}

#[test]
fn incompatible_adapter_contract_version_is_fatal() {
    let error = SourceManifest::from_toml(&manifest_with_source(&source_block(
        IMMUTABLE_REVISION,
        r#"["Apache-2.0"]"#,
        "code-eval-source-v2",
        2,
    )))
    .unwrap_err();

    assert!(matches!(
        error,
        SourceError::IncompatibleAdapterContractVersion { .. }
    ));
}

#[test]
fn pinned_manifest_preserves_unsupported_languages() {
    let manifest = SourceManifest::from_toml(include_str!("../benchmarks/code_eval.toml")).unwrap();

    assert_eq!(manifest.sources().len(), 3);
    assert!(manifest.sources().iter().all(|source| {
        source.revision().len() == 40
            && source.sha256().len() == 64
            && !source.licenses().is_empty()
            && source.expected_record_count() > 0
            && !source.evidence().is_empty()
    }));

    let cross_code_eval = manifest
        .sources()
        .iter()
        .find(|source| source.suite() == Suite::CrossCodeEval)
        .unwrap();
    assert!(cross_code_eval.languages().iter().any(|capability| {
        capability.language() == "java" && !capability.supported() && capability.reason().is_some()
    }));
}

#[test]
fn repoqa_gzip_counts_nested_needles() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(br#"{"python":[{"needles":[{"id":1},{"id":2}]}]}"#)
        .unwrap();
    let bytes = encoder.finish().unwrap();
    let manifest = compressed_manifest("repo_qa", "gzip_repo_qa_json", "python", 2, &bytes);

    let validated = validate_bytes(&manifest.sources()[0], &bytes).unwrap();

    assert_eq!(validated.record_count(), 2);
}

#[test]
fn cross_code_eval_xz_tar_counts_only_canonical_json_lines() {
    let encoder = XzEncoder::new(Vec::new(), 6);
    let mut archive = Builder::new(encoder);
    append_tar_file(
        &mut archive,
        "python/line_completion.jsonl",
        b"{\"metadata\":{\"task_id\":\"a\"}}\n{\"metadata\":{\"task_id\":\"b\"}}\n",
    );
    append_tar_file(
        &mut archive,
        "python/line_completion_oracle_bm25.jsonl",
        b"{\"metadata\":{\"task_id\":\"decoy\"}}\n",
    );
    archive.finish().unwrap();
    let encoder = archive.into_inner().unwrap();
    let bytes = encoder.finish().unwrap();
    let manifest = compressed_manifest(
        "cross_code_eval",
        "tar_xz_cross_code_eval",
        "python",
        2,
        &bytes,
    );

    let validated = validate_bytes(&manifest.sources()[0], &bytes).unwrap();

    assert_eq!(validated.record_count(), 2);
}

#[test]
fn jcg_gzip_tar_counts_language_testcase_sections() {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = Builder::new(encoder);
    append_tar_file(
        &mut archive,
        "JCG-revision/jcg_testcases/src/main/resources/js/Containers.md",
        b"# Containers\n\n## CO1\n```json\n{}\n```\n\n## CO2\n```json\n{}\n```\n",
    );
    append_tar_file(
        &mut archive,
        "JCG-revision/README.md",
        b"## This heading is not a testcase\n",
    );
    archive.finish().unwrap();
    let encoder = archive.into_inner().unwrap();
    let bytes = encoder.finish().unwrap();
    let manifest = compressed_manifest("jcg", "tar_gzip_jcg_markdown", "javascript", 2, &bytes);

    let validated = validate_bytes(&manifest.sources()[0], &bytes).unwrap();

    assert_eq!(validated.record_count(), 2);
}
