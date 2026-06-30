use std::collections::BTreeMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use flate2::read::GzDecoder;
use spur_context_fetcher::fetch::{
    fetch_git_archive, normalize_zip_to_tar_gz, CommandOutput, CommandRunner, CommandSpec,
};
use spur_context_fetcher::store::{
    build_archive_key, build_archive_metadata, idempotency_metadata_matches,
};
use spur_context_fetcher::store::{ArchiveStore, StoreError, StoredArchive};
use spur_context_fetcher::{handle_request, http_client, FetchConfig, FetchLimits, FetchRequest};
use tar::Archive;
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

#[derive(Default)]
struct RecordingRunner {
    commands: Vec<CommandSpec>,
    removals: Vec<PathBuf>,
    fail_checkout: bool,
}

impl CommandRunner for RecordingRunner {
    fn run(&mut self, spec: CommandSpec) -> Result<CommandOutput, std::io::Error> {
        if spec.program == "git" && spec.args.iter().any(|arg| arg == "clone") {
            let repo_dir = spec
                .args
                .last()
                .map(PathBuf::from)
                .expect("clone destination argument");
            fs::create_dir_all(repo_dir.join("src"))?;
            fs::write(repo_dir.join("src/lib.rs"), "pub fn fetched() {}\n")?;
        }
        let status_success = !(self.fail_checkout
            && spec.program == "git"
            && spec.args.iter().any(|arg| arg == "checkout"));
        if spec.program == "tar" && status_success {
            let archive_path = spec
                .args
                .get(1)
                .map(PathBuf::from)
                .expect("tar output argument");
            fs::File::create(archive_path)?;
        }
        self.commands.push(spec);
        Ok(CommandOutput {
            status_success,
            stderr: if status_success {
                String::new()
            } else {
                "checkout failed".to_owned()
            },
        })
    }

    fn remove_dir_all(&mut self, path: &Path) -> Result<(), std::io::Error> {
        self.removals.push(path.to_path_buf());
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }
}

#[test]
fn git_fetch_uses_hardened_clone_checkout_archive_and_cleanup() {
    let temp = TempDir::new().unwrap();
    let repo_dir = temp.path().join("repo");
    let archive = temp.path().join("source.tar.gz");
    let mut runner = RecordingRunner::default();

    let metadata = fetch_git_archive(
        &mut runner,
        "git+https://github.com/getspur/spur.git",
        "abc123",
        &repo_dir,
        &archive,
        1024 * 1024,
    )
    .unwrap();

    assert_eq!(metadata.bytes, 0);
    assert_eq!(runner.commands.len(), 3);
    assert_eq!(runner.commands[0].program, "git");
    assert_eq!(
        runner.commands[0].args,
        vec![
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.ext.allow=never",
            "clone",
            "--filter=blob:none",
            "--no-recurse-submodules",
            "https://github.com/getspur/spur.git",
            repo_dir.to_str().unwrap(),
        ]
    );
    assert_eq!(
        runner.commands[0]
            .env
            .get("GIT_TERMINAL_PROMPT")
            .map(String::as_str),
        Some("0")
    );
    assert_eq!(runner.commands[1].program, "git");
    assert_eq!(
        runner.commands[1].args,
        vec![
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.ext.allow=never",
            "-C",
            repo_dir.to_str().unwrap(),
            "checkout",
            "abc123",
        ]
    );
    assert_eq!(runner.commands[2].program, "tar");
    assert_eq!(
        runner.commands[2].args,
        vec![
            "-czf",
            archive.to_str().unwrap(),
            "--exclude=.git",
            "-C",
            repo_dir.to_str().unwrap(),
            ".",
        ]
    );
    assert_eq!(runner.removals, vec![repo_dir.clone(), repo_dir]);
}

#[test]
fn git_fetch_rejects_option_like_revision_before_commands() {
    let temp = TempDir::new().unwrap();
    let repo_dir = temp.path().join("repo");
    let archive = temp.path().join("source.tar.gz");
    let mut runner = RecordingRunner::default();

    let error = fetch_git_archive(
        &mut runner,
        "git+https://github.com/getspur/spur.git",
        "--no-checkout",
        &repo_dir,
        &archive,
        1024 * 1024,
    )
    .unwrap_err();

    assert!(error.to_string().contains("invalid git revision"));
    assert!(runner.commands.is_empty());
    assert!(runner.removals.is_empty());
}

#[test]
fn git_fetch_cleans_repo_dir_when_checkout_fails() {
    let temp = TempDir::new().unwrap();
    let repo_dir = temp.path().join("repo");
    let archive = temp.path().join("source.tar.gz");
    let mut runner = RecordingRunner {
        fail_checkout: true,
        ..RecordingRunner::default()
    };

    let error = fetch_git_archive(
        &mut runner,
        "git+https://github.com/getspur/spur.git",
        "abc123",
        &repo_dir,
        &archive,
        1024 * 1024,
    )
    .unwrap_err();

    assert!(error.to_string().contains("git checkout failed"));
    assert_eq!(runner.removals, vec![repo_dir.clone(), repo_dir]);
}

#[test]
fn zip_normalization_writes_readable_tar_gz_without_zip_container() {
    let temp = TempDir::new().unwrap();
    let zip_path = temp.path().join("source.zip");
    let tar_path = temp.path().join("source.tar.gz");

    {
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.add_directory("pkg/", SimpleFileOptions::default())
            .unwrap();
        zip.start_file("pkg/Cargo.toml", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"[package]\nname = \"pkg\"\n").unwrap();
        zip.start_file("pkg/src/lib.rs", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"pub fn ok() {}\n").unwrap();
        zip.finish().unwrap();
    }

    let metadata = normalize_zip_to_tar_gz(&zip_path, &tar_path, 1024 * 1024).unwrap();

    assert!(metadata.bytes > 0);
    assert_eq!(metadata.bytes, fs::metadata(&tar_path).unwrap().len());
    let decoder = GzDecoder::new(fs::File::open(&tar_path).unwrap());
    let mut archive = Archive::new(decoder);
    let mut entries = archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut body = String::new();
            if entry.header().entry_type().is_file() {
                entry.read_to_string(&mut body).unwrap();
            }
            (path, body)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    assert_eq!(
        entries,
        vec![
            (
                "pkg/Cargo.toml".to_owned(),
                "[package]\nname = \"pkg\"\n".to_owned()
            ),
            ("pkg/src/lib.rs".to_owned(), "pub fn ok() {}\n".to_owned()),
        ]
    );
}

#[test]
fn zip_normalization_rejects_underreported_unpacked_size() {
    let temp = TempDir::new().unwrap();
    let zip_path = temp.path().join("source.zip");
    let tar_path = temp.path().join("source.tar.gz");

    write_underreported_zip(&zip_path, b"0123456789abcdef");

    let error = normalize_zip_to_tar_gz(&zip_path, &tar_path, 8).unwrap_err();

    assert!(error.to_string().contains("unpacked zip exceeded cap"));
}

#[test]
fn s3_key_and_metadata_are_deterministic_and_idempotency_checked() {
    let metadata = build_archive_metadata(
        "https://github.com/getspur/spur.git",
        "abc123",
        "git",
        "content-hash",
        42,
    );

    assert_eq!(
        build_archive_key("/fetch/", "job-123").unwrap(),
        "fetch/job-123/source.tar.gz"
    );
    assert_eq!(metadata.get("revision").map(String::as_str), Some("abc123"));
    assert_eq!(metadata.get("source-kind").map(String::as_str), Some("git"));
    assert_eq!(
        metadata.get("content-sha256").map(String::as_str),
        Some("content-hash")
    );
    assert_eq!(metadata.get("bytes").map(String::as_str), Some("42"));
    assert_eq!(
        metadata
            .get("original-source-url-sha256")
            .map(String::as_str),
        Some("44d5cffc74fb7e9cf43d2be7ed9f6ecebae8ae3468b954c6ca13875d3d25a608")
    );

    let existing = metadata.into_iter().collect::<BTreeMap<_, _>>();
    assert!(idempotency_metadata_matches(
        &existing,
        "https://github.com/getspur/spur.git",
        "abc123",
        "git"
    ));
    assert!(!idempotency_metadata_matches(
        &existing,
        "https://github.com/getspur/other.git",
        "abc123",
        "git"
    ));
}

#[test]
fn s3_key_rejects_path_traversal_job_id() {
    let error = build_archive_key("fetch", "../other-job").unwrap_err();

    assert!(error.to_string().contains("invalid job_id"));
}

#[tokio::test]
async fn handle_request_cleans_workspace_when_store_put_fails() {
    let temp = TempDir::new().unwrap();
    let config = FetchConfig {
        bucket: "bucket".to_owned(),
        prefix: "fetch".to_owned(),
        presign_seconds: 60,
        validate_options: spur_context_source::ValidateOptions {
            tarball_size_cap_bytes: 1024 * 1024,
            git_size_cap_bytes: 1024 * 1024,
            allowed_domains: vec!["github.com".to_owned()],
        },
        tmp_root: temp.path().to_path_buf(),
    };
    let request = FetchRequest {
        job_id: "job-cleanup".to_owned(),
        package: "getspur/spur".to_owned(),
        revision: "abc123".to_owned(),
        source: "github".to_owned(),
        source_url: "git+https://github.com/getspur/spur.git".to_owned(),
        source_kind: "git".to_owned(),
        limits: Some(FetchLimits {
            max_source_bytes: Some(1024 * 1024),
            max_build_seconds: None,
        }),
    };
    let store = FailingPutStore::default();
    let client = http_client().unwrap();
    let mut runner = RecordingRunner::default();

    let error = handle_request(request, &config, &store, &client, &mut runner)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("put failed"));
    assert!(temp.path().read_dir().unwrap().next().is_none());
}

#[derive(Default)]
struct FailingPutStore {
    put_keys: Mutex<Vec<String>>,
}

#[async_trait]
impl ArchiveStore for FailingPutStore {
    async fn head_archive(&self, _key: &str) -> Result<Option<StoredArchive>, StoreError> {
        Ok(None)
    }

    async fn put_archive(
        &self,
        key: &str,
        _archive_path: &Path,
        _metadata: BTreeMap<String, String>,
    ) -> Result<(), StoreError> {
        self.put_keys.lock().unwrap().push(key.to_owned());
        Err(StoreError::S3("put failed".to_owned()))
    }

    async fn presign_archive(&self, _key: &str, _ttl: Duration) -> Result<String, StoreError> {
        Ok("https://example.com/presigned".to_owned())
    }
}

fn write_underreported_zip(path: &Path, body: &[u8]) {
    {
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "pkg/big.txt",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(body).unwrap();
        zip.finish().unwrap();
    }

    let mut bytes = fs::read(path).unwrap();
    patch_zip_uncompressed_sizes(&mut bytes, 1);
    fs::write(path, bytes).unwrap();
}

fn patch_zip_uncompressed_sizes(bytes: &mut [u8], size: u32) {
    let size = size.to_le_bytes();
    let mut index = 0;
    while index + 28 <= bytes.len() {
        if bytes[index..].starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
            bytes[index + 22..index + 26].copy_from_slice(&size);
        } else if bytes[index..].starts_with(&[0x50, 0x4b, 0x01, 0x02]) {
            bytes[index + 24..index + 28].copy_from_slice(&size);
        }
        index += 1;
    }
}
