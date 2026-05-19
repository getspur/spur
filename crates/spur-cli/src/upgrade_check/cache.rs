use std::io::{Error, ErrorKind};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CacheV1 {
    pub(crate) version: u32,
    pub(crate) checked_at: DateTime<Utc>,
    pub(crate) last_notified_at: DateTime<Utc>,
    pub(crate) current: String,
    pub(crate) latest: String,
}

pub(crate) fn read(path: &Path) -> Option<CacheV1> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            debug!(path = %path.display(), "upgrade check cache file missing");
            return None;
        }
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to read upgrade check cache file"
            );
            return None;
        }
    };

    let cache = match serde_json::from_str::<CacheV1>(&contents) {
        Ok(cache) => cache,
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to parse upgrade check cache file"
            );
            return None;
        }
    };

    if cache.version != CACHE_VERSION {
        return None;
    }

    Some(cache)
}

#[allow(dead_code)]
pub(crate) fn write(path: &Path, cache: &CacheV1) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        match std::fs::create_dir_all(parent) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }

    let tmp_path = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let contents = serde_json::to_vec(cache).map_err(Error::other)?;

    std::fs::write(&tmp_path, contents)?;
    std::fs::rename(&tmp_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    fn sample_cache(current: &str, latest: &str) -> CacheV1 {
        CacheV1 {
            version: 1,
            checked_at: Utc.timestamp_opt(1_700_000_000, 0).single().unwrap(),
            last_notified_at: Utc.timestamp_opt(1_700_000_100, 0).single().unwrap(),
            current: current.to_string(),
            latest: latest.to_string(),
        }
    }

    #[test]
    fn cache_roundtrips_through_json_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("upgrade-check.json");
        let cache = sample_cache("1.2.3", "1.2.4");

        write(&path, &cache).expect("write cache");

        assert_eq!(read(&path), Some(cache));
    }

    #[test]
    fn missing_cache_file_returns_none() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("missing.json");

        assert_eq!(read(&path), None);
    }

    #[test]
    fn malformed_json_returns_none() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("upgrade-check.json");
        std::fs::write(&path, "{").expect("write malformed cache");

        assert_eq!(read(&path), None);
    }

    #[test]
    fn wrong_cache_version_returns_none() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("upgrade-check.json");
        std::fs::write(
            &path,
            r#"{
                "version": 2,
                "checked_at": "2023-11-14T22:13:20Z",
                "last_notified_at": "2023-11-14T22:15:00Z",
                "current": "1.2.3",
                "latest": "1.2.4"
            }"#,
        )
        .expect("write wrong-version cache");

        assert_eq!(read(&path), None);
    }

    #[test]
    fn write_creates_missing_parent_directory() {
        let dir = tempdir().expect("tempdir");
        let path = dir
            .path()
            .join("missing")
            .join("parents")
            .join("upgrade-check.json");
        let cache = sample_cache("1.2.3", "1.2.4");

        write(&path, &cache).expect("write cache");

        assert_eq!(read(&path), Some(cache));
    }

    #[test]
    fn rapid_successive_writes_leave_valid_cache_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("upgrade-check.json");

        for i in 0..100 {
            let cache = sample_cache(&format!("1.2.{i}"), &format!("1.3.{i}"));
            write(&path, &cache).expect("write cache");
        }

        assert_eq!(read(&path), Some(sample_cache("1.2.99", "1.3.99")));
    }
}
