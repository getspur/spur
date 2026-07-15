use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::{
    validate_project_name, LocalProjectAddResult, LocalProjectCatalogSnapshot, LocalProjectEntry,
    LocalProjectError, LocalProjectRemoveResult, LOCAL_PROJECT_CATALOG_VERSION,
};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
// TOML integers are signed 64-bit values, so this is the largest generation
// that the catalog can represent durably.
const MAX_PERSISTED_GENERATION: u64 = u64::MAX / 2;

#[derive(Clone, Debug)]
enum CatalogPath {
    Explicit(PathBuf),
    Environment,
}

/// Persistent versioned local-project catalog.
#[derive(Clone, Debug)]
pub struct LocalProjectCatalogStore {
    path: CatalogPath,
}

#[derive(Debug, Serialize, Deserialize)]
struct CatalogDocument {
    version: u32,
    generation: u64,
    #[serde(default)]
    projects: Vec<LocalProjectEntry>,
}

impl Default for CatalogDocument {
    fn default() -> Self {
        Self {
            version: LOCAL_PROJECT_CATALOG_VERSION,
            generation: 0,
            projects: Vec::new(),
        }
    }
}

impl LocalProjectCatalogStore {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: CatalogPath::Explicit(path),
        }
    }

    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            path: CatalogPath::Environment,
        }
    }

    pub fn catalog_path(&self) -> Result<PathBuf, LocalProjectError> {
        match &self.path {
            CatalogPath::Explicit(path) => Ok(path.clone()),
            CatalogPath::Environment => environment_catalog_path(),
        }
    }

    pub fn snapshot(&self) -> Result<LocalProjectCatalogSnapshot, LocalProjectError> {
        let path = self.catalog_path()?;
        let lock = open_lock_file(&path)?;
        FileExt::lock_shared(&lock).map_err(|error| catalog_read_error(&path, error))?;
        let document = read_document(&path, CatalogAccess::Read)?.unwrap_or_default();
        let result = Ok(snapshot_from_document(document));
        let _ = FileExt::unlock(&lock);
        result
    }

    pub fn add(
        &self,
        name: &str,
        root: &Path,
        replace: bool,
    ) -> Result<LocalProjectAddResult, LocalProjectError> {
        validate_project_name(name)?;
        let root = canonical_root(root)?;
        self.mutate(|document| {
            match document
                .projects
                .iter_mut()
                .find(|entry| entry.name == name)
            {
                Some(entry) if entry.root == root => Ok(LocalProjectAddResult {
                    changed: false,
                    project: entry.clone(),
                    catalog_generation: document.generation,
                }),
                Some(entry) if !replace => Err(LocalProjectError::Conflict {
                    name: name.to_owned(),
                    registered_root: entry.root.clone(),
                    requested_root: root.clone(),
                }),
                Some(entry) => {
                    let generation = next_generation(document.generation)?;
                    entry.root.clone_from(&root);
                    document.generation = generation;
                    Ok(LocalProjectAddResult {
                        changed: true,
                        project: entry.clone(),
                        catalog_generation: document.generation,
                    })
                }
                None => {
                    let generation = next_generation(document.generation)?;
                    let project = LocalProjectEntry {
                        name: name.to_owned(),
                        root,
                    };
                    document.projects.push(project.clone());
                    document.generation = generation;
                    Ok(LocalProjectAddResult {
                        changed: true,
                        project,
                        catalog_generation: document.generation,
                    })
                }
            }
        })
    }

    pub fn remove(&self, name: &str) -> Result<LocalProjectRemoveResult, LocalProjectError> {
        validate_project_name(name)?;
        self.mutate(|document| {
            let previous_len = document.projects.len();
            document.projects.retain(|entry| entry.name != name);
            let removed = document.projects.len() != previous_len;
            if removed {
                document.generation = next_generation(document.generation)?;
            }
            Ok(LocalProjectRemoveResult {
                removed,
                catalog_generation: document.generation,
            })
        })
    }

    fn mutate<T>(
        &self,
        mutation: impl FnOnce(&mut CatalogDocument) -> Result<T, LocalProjectError>,
    ) -> Result<T, LocalProjectError> {
        self.mutate_with_lock_hook(|| {}, mutation)
    }

    fn mutate_with_lock_hook<T>(
        &self,
        after_lock: impl FnOnce(),
        mutation: impl FnOnce(&mut CatalogDocument) -> Result<T, LocalProjectError>,
    ) -> Result<T, LocalProjectError> {
        let path = self.catalog_path()?;
        ensure_private_directory(&path)?;
        let lock = open_lock_file(&path)?;
        FileExt::lock_exclusive(&lock).map_err(|error| catalog_write_error(&path, error))?;
        after_lock();
        let mut document = read_document(&path, CatalogAccess::Write)?.unwrap_or_default();
        let previous_generation = document.generation;
        let result = mutation(&mut document)?;
        if document.generation != previous_generation {
            document
                .projects
                .sort_by(|left, right| left.name.cmp(&right.name));
            write_document_atomically(&path, &document)?;
        }
        let _ = FileExt::unlock(&lock);
        Ok(result)
    }
}

fn environment_catalog_path() -> Result<PathBuf, LocalProjectError> {
    if let Some(path) = std::env::var_os("SPUR_PROJECT_CATALOG") {
        if path.is_empty() {
            return Err(LocalProjectError::ConfigUnavailable {
                reason: "SPUR_PROJECT_CATALOG is empty".to_owned(),
            });
        }
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        if !path.is_empty() {
            let path = absolute_config_root("XDG_CONFIG_HOME", PathBuf::from(path))?;
            return Ok(path.join("spur/projects.toml"));
        }
    }
    std::env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|home| {
            absolute_config_root("HOME", home).map(|home| home.join(".config/spur/projects.toml"))
        })
        .transpose()?
        .ok_or_else(|| LocalProjectError::ConfigUnavailable {
            reason: "set SPUR_PROJECT_CATALOG, XDG_CONFIG_HOME, or HOME".to_owned(),
        })
}

fn absolute_config_root(name: &str, path: PathBuf) -> Result<PathBuf, LocalProjectError> {
    if path.is_absolute() {
        return Ok(path);
    }
    Err(LocalProjectError::ConfigUnavailable {
        reason: format!("{name} must be an absolute path, got `{}`", path.display()),
    })
}

fn canonical_root(root: &Path) -> Result<PathBuf, LocalProjectError> {
    if !root.is_absolute() || root.to_str().is_none() {
        return Err(LocalProjectError::InvalidPath {
            path: root.to_path_buf(),
            reason: "path must be absolute UTF-8".to_owned(),
        });
    }
    let canonical = root
        .canonicalize()
        .map_err(|error| LocalProjectError::InvalidPath {
            path: root.to_path_buf(),
            reason: error.to_string(),
        })?;
    if canonical.to_str().is_none() {
        return Err(LocalProjectError::InvalidPath {
            path: root.to_path_buf(),
            reason: "path resolves to a non-UTF-8 root".to_owned(),
        });
    }
    Ok(canonical)
}

fn next_generation(generation: u64) -> Result<u64, LocalProjectError> {
    if generation >= MAX_PERSISTED_GENERATION {
        return Err(LocalProjectError::GenerationOverflow);
    }
    Ok(generation + 1)
}

fn snapshot_from_document(document: CatalogDocument) -> LocalProjectCatalogSnapshot {
    LocalProjectCatalogSnapshot {
        version: document.version,
        generation: document.generation,
        projects: document.projects,
    }
}

fn read_document(
    path: &Path,
    access: CatalogAccess,
) -> Result<Option<CatalogDocument>, LocalProjectError> {
    read_document_with_hook(path, access, || Ok(()))
}

fn read_document_with_hook(
    path: &Path,
    access: CatalogAccess,
    after_open: impl FnOnce() -> std::io::Result<()>,
) -> Result<Option<CatalogDocument>, LocalProjectError> {
    let Some(file) = open_catalog_file(path, access)? else {
        return Ok(None);
    };
    after_open().map_err(|error| catalog_access_error(access, path, error))?;
    let text =
        std::io::read_to_string(file).map_err(|error| catalog_access_error(access, path, error))?;
    let document: CatalogDocument =
        toml::from_str(&text).map_err(|error| LocalProjectError::CatalogParse {
            path: path.to_path_buf(),
            reason: format!("{error}; repair or remove the catalog"),
        })?;
    if document.version != LOCAL_PROJECT_CATALOG_VERSION {
        return Err(LocalProjectError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: document.version,
        });
    }
    let mut names = HashSet::with_capacity(document.projects.len());
    for entry in &document.projects {
        validate_project_name(&entry.name).map_err(|error| LocalProjectError::CatalogParse {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        if !names.insert(entry.name.clone()) {
            return Err(LocalProjectError::DuplicateName {
                path: path.to_path_buf(),
                name: entry.name.clone(),
            });
        }
        if !entry.root.is_absolute() || entry.root.to_str().is_none() {
            return Err(LocalProjectError::CatalogParse {
                path: path.to_path_buf(),
                reason: format!(
                    "project `{}` has a non-absolute or non-UTF-8 root",
                    entry.name
                ),
            });
        }
    }
    Ok(Some(document))
}

fn write_document_atomically(
    path: &Path,
    document: &CatalogDocument,
) -> Result<(), LocalProjectError> {
    write_document_atomically_with_hook(path, document, || Ok(()))
}

fn write_document_atomically_with_hook(
    path: &Path,
    document: &CatalogDocument,
    before_rename: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), LocalProjectError> {
    write_document_atomically_with_hooks(path, document, before_rename, sync_parent_directory)
}

fn write_document_atomically_with_hooks(
    path: &Path,
    document: &CatalogDocument,
    before_rename: impl FnOnce() -> std::io::Result<()>,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), LocalProjectError> {
    let parent = path
        .parent()
        .ok_or_else(|| LocalProjectError::CatalogWrite {
            path: path.to_path_buf(),
            reason: "catalog path has no parent directory".to_owned(),
        })?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("projects.toml");
    let temp_path = parent.join(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        sequence
    ));
    let serialized =
        toml::to_string(document).map_err(|error| LocalProjectError::CatalogWrite {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    let write_result = (|| {
        let mut file = private_create_new(&temp_path)?;
        file.write_all(serialized.as_bytes())?;
        file.sync_all()?;
        before_rename()?;
        fs::rename(&temp_path, path)?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(catalog_write_error(path, error));
    }
    if let Err(error) = sync_parent(parent) {
        if !is_unsupported_directory_sync_error(&error) {
            // The rename is already visible, but without a successful parent
            // sync its durability cannot be promised to the caller.
            return Err(catalog_write_error(path, error));
        }
        tracing::warn!(
            catalog_path = %path.display(),
            error = %error,
            "local project catalog was atomically replaced but parent directory sync failed"
        );
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), LocalProjectError> {
    let parent = path
        .parent()
        .ok_or_else(|| LocalProjectError::CatalogWrite {
            path: path.to_path_buf(),
            reason: "catalog path has no parent directory".to_owned(),
        })?;
    ensure_directory_exists(parent, path)
}

fn ensure_directory_exists(directory: &Path, catalog_path: &Path) -> Result<(), LocalProjectError> {
    // A relative catalog in the current directory has an empty parent. This
    // mirrors `create_dir_all("")`, which is a successful no-op.
    if directory.as_os_str().is_empty() {
        return Ok(());
    }
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => {
            return Err(catalog_write_error(
                catalog_path,
                format!(
                    "catalog parent `{}` is not a directory or is a symbolic link",
                    directory.display()
                ),
            ));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(catalog_write_error(catalog_path, error));
        }
        Err(_) => {}
    }

    if let Some(parent) = directory.parent() {
        ensure_directory_exists(parent, catalog_path)?;
    }
    match fs::create_dir(directory) {
        Ok(()) => set_private_directory_permissions(directory)
            .map_err(|error| catalog_write_error(catalog_path, error)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match fs::symlink_metadata(directory) {
                Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
                Ok(_) => Err(catalog_write_error(
                    catalog_path,
                    format!(
                        "catalog parent `{}` is not a directory or is a symbolic link",
                        directory.display()
                    ),
                )),
                Err(error) => Err(catalog_write_error(catalog_path, error)),
            }
        }
        Err(error) => Err(catalog_write_error(catalog_path, error)),
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|name| name.to_str()).map_or_else(
        || "projects.toml.lock".to_owned(),
        |name| format!("{name}.lock"),
    );
    path.with_file_name(name)
}

fn open_lock_file(path: &Path) -> Result<File, LocalProjectError> {
    ensure_private_directory(path)?;
    let lock_path = lock_path(path);
    let file = private_open(&lock_path).map_err(|error| catalog_write_error(path, error))?;
    set_private_file_permissions_on_handle(&file)
        .map_err(|error| catalog_write_error(path, error))?;
    Ok(file)
}

#[cfg(unix)]
fn private_open(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure_regular_file(&file)?;
    Ok(file)
}

#[cfg(not(unix))]
fn private_open(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    ensure_regular_file(&file)?;
    Ok(file)
}

fn ensure_regular_file(file: &File) -> std::io::Result<()> {
    if file.metadata()?.file_type().is_file() {
        Ok(())
    } else {
        Err(std::io::Error::other("path is not a regular file"))
    }
}

#[cfg(unix)]
fn set_private_file_permissions_on_handle(file: &File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions_on_handle(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn private_create_new(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_create_new(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Clone, Copy)]
enum CatalogAccess {
    Read,
    Write,
}

fn open_catalog_file(
    path: &Path,
    access: CatalogAccess,
) -> Result<Option<File>, LocalProjectError> {
    let result = open_catalog_file_nofollow(path);
    let file = match result {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(catalog_access_error(access, path, error)),
    };
    ensure_regular_file(&file).map_err(|error| catalog_access_error(access, path, error))?;
    set_private_file_permissions_on_handle(&file)
        .map_err(|error| catalog_access_error(access, path, error))?;
    Ok(Some(file))
}

#[cfg(unix)]
fn open_catalog_file_nofollow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(windows)]
fn open_catalog_file_nofollow(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_catalog_file_nofollow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

fn catalog_access_error(
    access: CatalogAccess,
    path: &Path,
    error: impl std::fmt::Display,
) -> LocalProjectError {
    match access {
        CatalogAccess::Read => catalog_read_error(path, error),
        CatalogAccess::Write => catalog_write_error(path, error),
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn is_unsupported_directory_sync_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Unsupported
        || error
            .raw_os_error()
            .is_some_and(|code| code == libc::EINVAL || code == libc::ENOTSUP)
}

#[cfg(not(unix))]
fn is_unsupported_directory_sync_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Unsupported
}

fn catalog_read_error(path: &Path, error: impl std::fmt::Display) -> LocalProjectError {
    LocalProjectError::CatalogRead {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

fn catalog_write_error(path: &Path, error: impl std::fmt::Display) -> LocalProjectError {
    LocalProjectError::CatalogWrite {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_read_uses_the_same_nofollow_descriptor_after_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("projects.toml");
        let replacement = temp.path().join("replacement.toml");
        let original = CatalogDocument {
            version: 1,
            generation: 1,
            projects: Vec::new(),
        };
        let swapped = CatalogDocument {
            version: 1,
            generation: 9,
            projects: Vec::new(),
        };
        fs::write(
            &path,
            toml::to_string(&original).expect("serialize original"),
        )
        .expect("write original");
        fs::write(
            &replacement,
            toml::to_string(&swapped).expect("serialize replacement"),
        )
        .expect("write replacement");

        let observed = read_document_with_hook(&path, CatalogAccess::Read, || {
            fs::rename(&replacement, &path)
        })
        .expect("read opened catalog")
        .expect("catalog exists");

        assert_eq!(observed.generation, 1);
        let pathname: CatalogDocument =
            toml::from_str(&fs::read_to_string(&path).expect("read swapped catalog from pathname"))
                .expect("parse swapped catalog");
        assert_eq!(pathname.generation, 9);
    }

    #[test]
    fn parent_directory_sync_io_error_is_reported_after_atomic_replace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("projects.toml");
        let previous = "version = 1\ngeneration = 1\nprojects = []\n";
        fs::write(&path, previous).expect("seed prior catalog");
        let document = CatalogDocument {
            version: 1,
            generation: 2,
            projects: Vec::new(),
        };

        let result = write_document_atomically_with_hooks(
            &path,
            &document,
            || Ok(()),
            |_| Err(std::io::Error::from_raw_os_error(libc::EIO)),
        );

        assert!(matches!(
            result,
            Err(LocalProjectError::CatalogWrite { .. })
        ));
        let visible: CatalogDocument =
            toml::from_str(&fs::read_to_string(&path).expect("read atomically replaced catalog"))
                .expect("parse replaced catalog");
        assert_eq!(visible.generation, 2);
    }

    #[test]
    fn competing_mutation_blocks_inside_the_real_catalog_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp = tempfile::tempdir().expect("tempdir");
        let store_a = LocalProjectCatalogStore::new(temp.path().join("projects.toml"));
        let store_b = store_a.clone();
        let alpha = temp.path().join("alpha");
        let beta = temp.path().join("beta");
        fs::create_dir_all(&alpha).expect("create alpha");
        fs::create_dir_all(&beta).expect("create beta");
        let alpha = alpha.canonicalize().expect("canonical alpha");
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = std::thread::spawn(move || {
            store_a
                .mutate_with_lock_hook(
                    || {
                        locked_tx.send(()).expect("announce held lock");
                        release_rx.recv().expect("wait for release");
                    },
                    |document| {
                        document.projects.push(LocalProjectEntry {
                            name: "alpha".to_owned(),
                            root: alpha,
                        });
                        document.generation = next_generation(document.generation)?;
                        Ok(())
                    },
                )
                .expect("first mutation");
        });
        locked_rx.recv().expect("first mutation acquired lock");

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            started_tx.send(()).expect("announce second mutation");
            let result = store_b.add("beta", &beta, false);
            done_tx.send(result).expect("report second mutation");
        });
        started_rx.recv().expect("second mutation started");
        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(250)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "second mutation completed while the first held the catalog lock"
        );

        release_tx.send(()).expect("release first mutation");
        first.join().expect("first thread");
        let second_result = done_rx.recv().expect("second mutation result");
        second_result.expect("second mutation");
        second.join().expect("second thread");

        let snapshot = LocalProjectCatalogStore::new(temp.path().join("projects.toml"))
            .snapshot()
            .expect("final snapshot");
        assert_eq!(snapshot.generation, 2);
        assert_eq!(snapshot.projects.len(), 2);
    }

    #[test]
    fn pre_rename_atomic_failure_preserves_previous_catalog_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("projects.toml");
        let previous = "version = 1\ngeneration = 1\nprojects = []\n";
        fs::write(&path, previous).expect("seed prior catalog");
        let document = CatalogDocument {
            version: 1,
            generation: 2,
            projects: Vec::new(),
        };

        let result = write_document_atomically_with_hook(&path, &document, || {
            Err(std::io::Error::other("injected failure before rename"))
        });

        assert!(matches!(
            result,
            Err(LocalProjectError::CatalogWrite { .. })
        ));
        assert_eq!(fs::read_to_string(path).expect("prior catalog"), previous);
        let remaining = fs::read_dir(temp.path())
            .expect("catalog directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![std::ffi::OsString::from("projects.toml")]);
    }
}
