use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write as _};
use std::path::{Component, Path};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::StreamExt as _;
use reqwest::header::{HeaderMap, LOCATION, USER_AGENT};
use reqwest::{Client, StatusCode, Url};
use sha2::{Digest as _, Sha256};
use spur_context_source::{
    resolve_and_check_dns, validate, AbuseError, SourceKind, ValidateOptions,
};
use tar::{Archive, Builder, Header};
use thiserror::Error;
use tokio::io::AsyncWriteExt as _;

const USER_AGENT_VALUE: &str = "spur-context-fetcher/1.0";
const MAX_REDIRECTS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMetadata {
    pub content_sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status_success: bool,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&mut self, spec: CommandSpec) -> Result<CommandOutput, std::io::Error>;
    fn remove_dir_all(&mut self, path: &Path) -> Result<(), std::io::Error>;
}

#[derive(Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&mut self, spec: CommandSpec) -> Result<CommandOutput, std::io::Error> {
        let output = std::process::Command::new(&spec.program)
            .args(&spec.args)
            .envs(&spec.env)
            .output()?;
        Ok(CommandOutput {
            status_success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    fn remove_dir_all(&mut self, path: &Path) -> Result<(), std::io::Error> {
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("{0}")]
    Validation(String),
    #[error("source_too_large: {0}")]
    SourceTooLarge(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("git failed: {0}")]
    Git(String),
    #[error("archive normalization failed: {0}")]
    Archive(String),
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
}

pub fn normalize_source_kind(value: &str) -> Result<SourceKind, FetchError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "git" => Ok(SourceKind::Git),
        "tarball" => Ok(SourceKind::Tarball),
        other => Err(FetchError::Validation(format!(
            "unsupported source_kind `{other}`"
        ))),
    }
}

pub fn validate_public_fetch_url(
    source_url: &str,
    opts: &ValidateOptions,
) -> Result<(), FetchError> {
    let lower = source_url.trim_start().to_ascii_lowercase();
    if lower.starts_with("git+ssh://") {
        return Err(FetchError::Validation(
            "git+ssh sources are not supported by the public fetcher".to_owned(),
        ));
    }
    let parsed = validate(source_url, opts).map_err(validation_error)?;
    resolve_and_check_dns(&parsed).map_err(validation_error)?;
    Ok(())
}

pub async fn download_and_normalize_tarball(
    client: &Client,
    source_url: &str,
    download_path: &Path,
    output_archive: &Path,
    max_bytes: u64,
    opts: &ValidateOptions,
) -> Result<ArchiveMetadata, FetchError> {
    let final_url =
        download_https_with_redirect_validation(client, source_url, download_path, max_bytes, opts)
            .await?;

    let archive_kind = match (
        archive_kind_from_url(source_url),
        archive_kind_from_url(final_url.as_str()),
    ) {
        (ArchiveKind::Zip, _) | (_, ArchiveKind::Zip) => ArchiveKind::Zip,
        (ArchiveKind::TarGz, ArchiveKind::TarGz) => ArchiveKind::TarGz,
    };

    match archive_kind {
        ArchiveKind::Zip => normalize_zip_to_tar_gz(download_path, output_archive, max_bytes),
        ArchiveKind::TarGz => {
            validate_tar_gz_archive(download_path, max_bytes)?;
            if download_path != output_archive {
                if let Some(parent) = output_archive.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(download_path, output_archive)?;
            }
            archive_metadata(output_archive)
        }
    }
}

pub async fn download_https_with_redirect_validation(
    client: &Client,
    source_url: &str,
    output_path: &Path,
    max_bytes: u64,
    opts: &ValidateOptions,
) -> Result<Url, FetchError> {
    validate_public_fetch_url(source_url, opts)?;
    let mut url = Url::parse(source_url)
        .map_err(|error| FetchError::Validation(format!("invalid source_url: {error}")))?;
    if url.scheme() == "git+https" {
        url.set_scheme("https").map_err(|()| {
            FetchError::Validation("failed to normalize git+https source_url".to_owned())
        })?;
    }
    if url.scheme() != "https" {
        return Err(FetchError::Validation(
            "only public HTTPS downloads are supported".to_owned(),
        ));
    }

    for _ in 0..=MAX_REDIRECTS {
        let response = client
            .get(url.clone())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .send()
            .await
            .map_err(|error| FetchError::Download(error.to_string()))?;

        if is_redirect(response.status()) {
            let location = redirect_location(response.headers())?;
            let next = url
                .join(location)
                .map_err(|error| FetchError::Validation(format!("invalid redirect: {error}")))?;
            validate_public_fetch_url(next.as_str(), opts)?;
            if next.scheme() != "https" {
                return Err(FetchError::Validation(
                    "redirect target must use HTTPS".to_owned(),
                ));
            }
            url = next;
            continue;
        }

        if !response.status().is_success() {
            return Err(FetchError::Download(format!(
                "HTTP {} from source_url",
                response.status()
            )));
        }

        if response
            .content_length()
            .is_some_and(|content_length| content_length > max_bytes)
        {
            return Err(FetchError::SourceTooLarge(format!(
                "content-length exceeds cap: {} > {max_bytes}",
                response.content_length().unwrap_or_default()
            )));
        }

        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = tokio::fs::File::create(output_path).await?;
        let mut bytes = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| FetchError::Download(error.to_string()))?;
            bytes = bytes
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| FetchError::SourceTooLarge("download size overflow".to_owned()))?;
            if bytes > max_bytes {
                return Err(FetchError::SourceTooLarge(format!(
                    "download exceeded cap: {bytes} > {max_bytes}"
                )));
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        return Ok(url);
    }

    Err(FetchError::Validation(format!(
        "too many redirects; maximum is {MAX_REDIRECTS}"
    )))
}

pub fn fetch_git_archive(
    runner: &mut dyn CommandRunner,
    source_url: &str,
    revision: &str,
    repo_dir: &Path,
    output_archive: &Path,
    max_unpacked_bytes: u64,
) -> Result<ArchiveMetadata, FetchError> {
    validate_git_revision(revision)?;
    let clone_url = git_clone_url(source_url)?;
    runner.remove_dir_all(repo_dir)?;
    if let Some(parent) = repo_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = output_archive.parent() {
        fs::create_dir_all(parent)?;
    }

    let result = fetch_git_archive_inner(
        runner,
        clone_url,
        revision,
        repo_dir,
        output_archive,
        max_unpacked_bytes,
    );
    let cleanup = runner.remove_dir_all(repo_dir);
    match (result, cleanup) {
        (Ok(metadata), Ok(())) => Ok(metadata),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(FetchError::Io(error)),
        (Err(error), Err(_cleanup_error)) => Err(error),
    }
}

fn fetch_git_archive_inner(
    runner: &mut dyn CommandRunner,
    clone_url: String,
    revision: &str,
    repo_dir: &Path,
    output_archive: &Path,
    max_unpacked_bytes: u64,
) -> Result<ArchiveMetadata, FetchError> {
    let clone = runner.run(CommandSpec {
        program: "git".to_owned(),
        args: vec![
            "-c".to_owned(),
            "protocol.file.allow=never".to_owned(),
            "-c".to_owned(),
            "protocol.ext.allow=never".to_owned(),
            "clone".to_owned(),
            "--filter=blob:none".to_owned(),
            "--no-recurse-submodules".to_owned(),
            clone_url,
            path_arg(repo_dir),
        ],
        env: git_env(),
    })?;
    ensure_command_success("git clone", clone)?;

    let checkout = runner.run(CommandSpec {
        program: "git".to_owned(),
        args: vec![
            "-c".to_owned(),
            "protocol.file.allow=never".to_owned(),
            "-c".to_owned(),
            "protocol.ext.allow=never".to_owned(),
            "-C".to_owned(),
            path_arg(repo_dir),
            "checkout".to_owned(),
            // Plain `checkout <revision>`. Option-injection is already prevented
            // by `validate_git_revision` (rejects leading `-` and restricts the
            // charset), so no separator is needed. `--end-of-options` is NOT
            // portable to `git checkout` in the pinned git — it is parsed as a
            // pathspec ("pathspec '--end-of-options' did not match") — and a
            // trailing `--` after it yields a phantom second reference.
            revision.to_owned(),
        ],
        env: git_env(),
    })?;
    ensure_command_success("git checkout", checkout)?;

    enforce_source_tree_cap(repo_dir, max_unpacked_bytes)?;

    let archive = runner.run(CommandSpec {
        program: "tar".to_owned(),
        args: vec![
            "-czf".to_owned(),
            path_arg(output_archive),
            "--exclude=.git".to_owned(),
            "-C".to_owned(),
            path_arg(repo_dir),
            ".".to_owned(),
        ],
        env: BTreeMap::new(),
    })?;
    ensure_command_success("tar archive", archive)?;

    let metadata = archive_metadata(output_archive)?;
    Ok(metadata)
}

pub fn normalize_zip_to_tar_gz(
    zip_path: &Path,
    output_archive: &Path,
    max_unpacked_bytes: u64,
) -> Result<ArchiveMetadata, FetchError> {
    if let Some(parent) = output_archive.parent() {
        fs::create_dir_all(parent)?;
    }
    let zip_file = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(zip_file)
        .map_err(|error| FetchError::Archive(format!("failed to read zip: {error}")))?;
    let output = fs::File::create(output_archive)?;
    let encoder = GzEncoder::new(output, Compression::default());
    let mut tar = Builder::new(encoder);
    let mut unpacked_bytes = 0_u64;

    for index in 0..zip.len() {
        let mut file = zip
            .by_index(index)
            .map_err(|error| FetchError::Archive(format!("failed to read zip entry: {error}")))?;
        if file.is_dir() {
            continue;
        }
        let enclosed = file
            .enclosed_name()
            .ok_or_else(|| FetchError::Archive(format!("unsafe zip path `{}`", file.name())))?;
        validate_relative_archive_path(&enclosed)?;
        let temp_entry_path =
            output_archive.with_file_name(format!(".spur-context-fetcher-entry-{index}.tmp"));
        let entry_bytes = copy_zip_entry_to_temp_with_cap(
            &mut file,
            &temp_entry_path,
            unpacked_bytes,
            max_unpacked_bytes,
        )?;
        unpacked_bytes = unpacked_bytes
            .checked_add(entry_bytes)
            .ok_or_else(|| FetchError::SourceTooLarge("zip entry size overflow".to_owned()))?;

        let mut header = Header::new_gnu();
        header
            .set_path(&enclosed)
            .map_err(|error| FetchError::Archive(error.to_string()))?;
        header.set_size(entry_bytes);
        header.set_mode(file.unix_mode().unwrap_or(0o644) & 0o777);
        header.set_cksum();
        let mut temp_entry = fs::File::open(&temp_entry_path)?;
        tar.append(&header, &mut temp_entry)
            .map_err(|error| FetchError::Archive(error.to_string()))?;
        fs::remove_file(&temp_entry_path)?;
    }

    let encoder = tar
        .into_inner()
        .map_err(|error| FetchError::Archive(error.to_string()))?;
    encoder
        .finish()
        .map_err(|error| FetchError::Archive(error.to_string()))?;
    let metadata = archive_metadata(output_archive)?;
    if metadata.bytes > max_unpacked_bytes {
        return Err(FetchError::SourceTooLarge(format!(
            "normalized archive exceeded cap: {} > {max_unpacked_bytes}",
            metadata.bytes
        )));
    }
    Ok(metadata)
}

pub fn archive_metadata(path: &Path) -> Result<ArchiveMetadata, FetchError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| FetchError::SourceTooLarge("archive size overflow".to_owned()))?;
        hasher.update(&buffer[..read]);
    }
    Ok(ArchiveMetadata {
        content_sha256: format!("{:x}", hasher.finalize()),
        bytes,
    })
}

fn validate_tar_gz_archive(path: &Path, max_unpacked_bytes: u64) -> Result<(), FetchError> {
    let file = fs::File::open(path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let mut unpacked_bytes = 0_u64;
    let entries = archive
        .entries()
        .map_err(|error| FetchError::Archive(format!("failed to read tar: {error}")))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|error| FetchError::Archive(format!("failed to read tar entry: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| FetchError::Archive(format!("failed to read tar path: {error}")))?;
        validate_relative_archive_path(&path)?;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = entry.read(&mut buffer).map_err(|error| {
                FetchError::Archive(format!("failed to read tar entry bytes: {error}"))
            })?;
            if read == 0 {
                break;
            }
            unpacked_bytes = unpacked_bytes
                .checked_add(read as u64)
                .ok_or_else(|| FetchError::SourceTooLarge("tar entry size overflow".to_owned()))?;
            if unpacked_bytes > max_unpacked_bytes {
                return Err(FetchError::SourceTooLarge(format!(
                    "unpacked tar exceeded cap: {unpacked_bytes} > {max_unpacked_bytes}"
                )));
            }
        }
    }
    Ok(())
}

fn copy_zip_entry_to_temp_with_cap<R: Read>(
    reader: &mut R,
    temp_entry_path: &Path,
    already_unpacked_bytes: u64,
    max_unpacked_bytes: u64,
) -> Result<u64, FetchError> {
    let mut temp_entry = fs::File::create(temp_entry_path)?;
    let mut entry_bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            FetchError::Archive(format!("failed to read zip entry bytes: {error}"))
        })?;
        if read == 0 {
            break;
        }
        entry_bytes = entry_bytes
            .checked_add(read as u64)
            .ok_or_else(|| FetchError::SourceTooLarge("zip entry size overflow".to_owned()))?;
        let total = already_unpacked_bytes
            .checked_add(entry_bytes)
            .ok_or_else(|| FetchError::SourceTooLarge("zip entry size overflow".to_owned()))?;
        if total > max_unpacked_bytes {
            let _ = fs::remove_file(temp_entry_path);
            return Err(FetchError::SourceTooLarge(format!(
                "unpacked zip exceeded cap: {total} > {max_unpacked_bytes}"
            )));
        }
        temp_entry.write_all(&buffer[..read])?;
    }
    temp_entry.flush()?;
    Ok(entry_bytes)
}

fn enforce_source_tree_cap(path: &Path, max_bytes: u64) -> Result<(), FetchError> {
    let bytes = source_tree_size_bytes(path, max_bytes)?;
    if bytes > max_bytes {
        return Err(FetchError::SourceTooLarge(format!(
            "source tree exceeded cap: {bytes} > {max_bytes}"
        )));
    }
    Ok(())
}

fn source_tree_size_bytes(path: &Path, max_bytes: u64) -> Result<u64, FetchError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        total = total
            .checked_add(source_tree_size_bytes(&entry.path(), max_bytes)?)
            .ok_or_else(|| FetchError::SourceTooLarge("source tree size overflow".to_owned()))?;
        if total > max_bytes {
            return Ok(total);
        }
    }
    Ok(total)
}

fn validate_relative_archive_path(path: &Path) -> Result<(), FetchError> {
    if path.is_absolute() {
        return Err(FetchError::Archive(format!(
            "archive path `{}` is absolute",
            path.display()
        )));
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(FetchError::Archive(format!(
            "archive path `{}` escapes the archive root",
            path.display()
        )));
    }
    Ok(())
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn redirect_location(headers: &HeaderMap) -> Result<&str, FetchError> {
    headers
        .get(LOCATION)
        .ok_or_else(|| FetchError::Validation("redirect missing Location header".to_owned()))?
        .to_str()
        .map_err(|error| FetchError::Validation(format!("invalid redirect Location: {error}")))
}

fn git_clone_url(source_url: &str) -> Result<String, FetchError> {
    let lower = source_url.to_ascii_lowercase();
    let clone_url = if lower.starts_with("git+ssh://") {
        return Err(FetchError::Validation(
            "git+ssh sources are not supported by the public fetcher".to_owned(),
        ));
    } else if lower.starts_with("git+https://") {
        format!("https://{}", &source_url["git+https://".len()..])
    } else if lower.starts_with("https://") {
        source_url.to_owned()
    } else {
        return Err(FetchError::Validation(
            "git sources must use https or git+https".to_owned(),
        ));
    };
    let parsed = Url::parse(&clone_url)
        .map_err(|error| FetchError::Validation(format!("invalid git source_url: {error}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(FetchError::Validation(
            "source_url must not contain embedded credentials".to_owned(),
        ));
    }
    Ok(clone_url)
}

fn ensure_command_success(name: &str, output: CommandOutput) -> Result<(), FetchError> {
    if output.status_success {
        Ok(())
    } else {
        Err(FetchError::Git(format!("{name} failed: {}", output.stderr)))
    }
}

fn git_env() -> BTreeMap<String, String> {
    BTreeMap::from([("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned())])
}

fn path_arg(path: &Path) -> String {
    path.as_os_str().to_string_lossy().into_owned()
}

fn validate_git_revision(revision: &str) -> Result<(), FetchError> {
    let valid = !revision.is_empty()
        && revision.len() <= 256
        && !revision.starts_with('-')
        && revision.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b':' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(FetchError::Validation(
            "invalid git revision: must match [A-Za-z0-9._/:-]{1,256} and not start with '-'"
                .to_owned(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    TarGz,
    Zip,
}

fn archive_kind_from_url(source_url: &str) -> ArchiveKind {
    let path = Url::parse(source_url)
        .ok()
        .map(|url| url.path().to_ascii_lowercase())
        .unwrap_or_else(|| source_url.to_ascii_lowercase());
    if path.ends_with(".zip") {
        ArchiveKind::Zip
    } else {
        ArchiveKind::TarGz
    }
}

fn validation_error(error: AbuseError) -> FetchError {
    FetchError::Validation(format!("source_url validation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tar_gz_validation_reads_entry_bodies_instead_of_trusting_headers() {
        let temp = tempfile::TempDir::new().unwrap();
        let archive_path = temp.path().join("truncated.tar.gz");
        write_truncated_tar_gz(&archive_path);

        let error = validate_tar_gz_archive(&archive_path, 1024).unwrap_err();

        assert!(
            error.to_string().contains("failed to read tar"),
            "unexpected error: {error}"
        );
    }

    fn write_truncated_tar_gz(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let mut encoder = GzEncoder::new(file, Compression::default());
        let mut header = Header::new_gnu();
        header.set_path("pkg/file.txt").unwrap();
        header.set_size(10);
        header.set_mode(0o644);
        header.set_cksum();
        encoder.write_all(header.as_bytes()).unwrap();
        encoder.write_all(b"short").unwrap();
        encoder.finish().unwrap();
    }
}
