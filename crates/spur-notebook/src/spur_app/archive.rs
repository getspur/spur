use std::collections::BTreeSet;
use std::io::{Read, Seek, Write};

use thiserror::Error;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use super::{is_safe_archive_path, SpurAppManifest, SPUR_APP_MANIFEST};

#[derive(Debug, Error)]
pub enum SpurAppArchiveError {
    #[error("zip archive error: {0}")]
    Zip(#[from] ZipError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsafe archive path: {0}")]
    UnsafePath(String),
    #[error("duplicate archive path: {0}")]
    DuplicatePath(String),
    #[error("missing SpurApp manifest")]
    MissingManifest,
    #[error("invalid SpurApp manifest JSON: {0}")]
    InvalidManifestJson(serde_json::Error),
}

pub fn write_entries<W, I>(writer: W, entries: I) -> Result<(), SpurAppArchiveError>
where
    W: Write + Seek,
    I: IntoIterator<Item = (String, Vec<u8>)>,
{
    let mut entries = entries.into_iter().collect::<Vec<_>>();
    validate_entry_names(entries.iter().map(|(name, _)| name.as_str()))?;
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut archive = ZipWriter::new(writer);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default_for_write());

    for (name, contents) in entries {
        archive.start_file(name, options)?;
        archive.write_all(&contents)?;
    }

    archive.finish()?;
    Ok(())
}

pub fn read_entry<R>(reader: R, path: &str) -> Result<Vec<u8>, SpurAppArchiveError>
where
    R: Read + Seek,
{
    if !is_safe_archive_path(path) {
        return Err(SpurAppArchiveError::UnsafePath(path.to_string()));
    }

    let mut archive = ZipArchive::new(reader)?;
    validate_archive_entry_names(&mut archive)?;

    let mut entry = archive.by_name(path).map_err(|err| match err {
        ZipError::FileNotFound if path == SPUR_APP_MANIFEST => SpurAppArchiveError::MissingManifest,
        err => SpurAppArchiveError::Zip(err),
    })?;
    let mut contents = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut contents)?;
    Ok(contents)
}

pub fn read_manifest<R>(reader: R) -> Result<SpurAppManifest, SpurAppArchiveError>
where
    R: Read + Seek,
{
    let contents = read_entry(reader, SPUR_APP_MANIFEST)?;
    serde_json::from_slice(&contents).map_err(SpurAppArchiveError::InvalidManifestJson)
}

fn validate_archive_entry_names<R>(archive: &mut ZipArchive<R>) -> Result<(), SpurAppArchiveError>
where
    R: Read + Seek,
{
    let mut names = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        names.push(entry.name().to_string());
    }

    validate_entry_names(names.iter().map(String::as_str))
}

fn validate_entry_names<'a, I>(names: I) -> Result<(), SpurAppArchiveError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = BTreeSet::new();

    for name in names {
        if !is_safe_archive_path(name) {
            return Err(SpurAppArchiveError::UnsafePath(name.to_string()));
        }

        if !seen.insert(name.to_string()) {
            return Err(SpurAppArchiveError::DuplicatePath(name.to_string()));
        }
    }

    Ok(())
}
