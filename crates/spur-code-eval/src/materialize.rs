//! Deterministic, case-isolated repository materialization.

use std::{
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{CodeEvalCase, Suite};

const CHECKOUT_DIRECTORY: &str = "repository";
const METADATA_FILE: &str = ".spur-code-eval-materialization.json";
const METADATA_VERSION: u32 = 1;
static NEXT_TEMPORARY_ROOT: AtomicU64 = AtomicU64::new(0);

/// Auditable failure while creating or validating an isolated repository root.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MaterializeError {
    /// A filesystem operation failed at an explicit path.
    #[error("cannot {operation} at {path}: {message}")]
    Filesystem {
        /// Stable operation name.
        operation: &'static str,
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Operating-system error text.
        message: String,
    },
    /// Git rejected a non-interactive command.
    #[error("git operation {operation} failed with status {status:?}: {stderr}")]
    GitCommand {
        /// Stable operation name.
        operation: &'static str,
        /// Process exit code, when the platform supplied one.
        status: Option<i32>,
        /// Lossy stderr retained for audit diagnostics.
        stderr: String,
    },
    /// A repository subdirectory was not a safe relative path.
    #[error(
        "repository subdirectory must contain only normal relative components: {subdirectory:?}"
    )]
    InvalidSubdirectory {
        /// Rejected subdirectory.
        subdirectory: String,
    },
    /// The pinned repository subtree is absent from the detached checkout.
    #[error("repository subdirectory {subdirectory:?} is missing below {root}")]
    MissingSubdirectory {
        /// Repository clone being rejected.
        root: PathBuf,
        /// Missing relative subtree.
        subdirectory: String,
    },
    /// A pinned subtree resolves outside its repository clone.
    #[error(
        "repository subdirectory {subdirectory:?} below {root} resolves outside the clone to {resolved}"
    )]
    SubdirectoryEscapesRoot {
        /// Repository clone being rejected.
        root: PathBuf,
        /// Pinned relative subtree.
        subdirectory: String,
        /// Canonical path outside the clone.
        resolved: PathBuf,
    },
    /// A materialization root did not contain its audit metadata.
    #[error("materialization metadata is missing from {root}")]
    MissingMetadata {
        /// Root being verified.
        root: PathBuf,
    },
    /// Audit metadata could not be decoded.
    #[error("materialization metadata at {path} is invalid: {message}")]
    InvalidMetadata {
        /// Metadata path.
        path: PathBuf,
        /// Decode or version error.
        message: String,
    },
    /// A root belongs to a different case, dataset revision, commit, or subtree.
    #[error("materialization root {root} has identity {actual:?}, expected {expected:?}")]
    MixedRepositoryRoot {
        /// Root being rejected.
        root: PathBuf,
        /// Identity required by the requested case.
        expected: String,
        /// Identity recorded by the existing root.
        actual: String,
    },
    /// The clone's `origin` URL differs from the repository pin.
    #[error("repository origin at {root} is {actual:?}, expected {expected:?}")]
    OriginMismatch {
        /// Repository clone being rejected.
        root: PathBuf,
        /// Pinned origin URL.
        expected: String,
        /// Configured origin URL.
        actual: String,
    },
    /// The checked-out commit differs from the exact repository revision.
    #[error("repository HEAD at {root} is {actual:?}, expected {expected:?}")]
    HeadMismatch {
        /// Repository clone being rejected.
        root: PathBuf,
        /// Pinned commit object identifier.
        expected: String,
        /// Checked-out commit object identifier.
        actual: String,
    },
    /// The pinned commit is checked out through a movable branch reference.
    #[error("repository HEAD at {root} is attached to {reference:?}")]
    AttachedHead {
        /// Repository clone being rejected.
        root: PathBuf,
        /// Movable branch reference reported by Git.
        reference: String,
    },
    /// Tracked or untracked working-tree state differs from the pinned commit.
    #[error("repository checkout at {root} is dirty: {status}")]
    DirtyCheckout {
        /// Repository clone being rejected.
        root: PathBuf,
        /// Escaped porcelain-v1 status bytes.
        status: String,
    },
    /// The tracked Git tree does not match the declared materialization hash.
    #[error("repository content hash at {root} is {actual:?}, expected {expected:?}")]
    ContentHashMismatch {
        /// Repository clone being rejected.
        root: PathBuf,
        /// Hash declared by the repository pin.
        expected: String,
        /// Hash computed from tracked Git tree entries.
        actual: String,
    },
    /// An existing root records a different pinned dataset-content hash.
    #[error("dataset content hash at {root} is {actual:?}, expected {expected:?}")]
    DatasetHashMismatch {
        /// Materialization root being rejected.
        root: PathBuf,
        /// Dataset hash declared by the requested case.
        expected: String,
        /// Dataset hash recorded when the root was promoted.
        actual: String,
    },
}

/// A fully promoted materialization and its checked-out repository paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedRoot {
    root: PathBuf,
    repository_root: PathBuf,
    source_root: PathBuf,
}

impl MaterializedRoot {
    /// Returns the atomic case root containing metadata and the clone.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the root of the detached repository clone.
    #[must_use]
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    /// Returns the selected repository subtree, or the repository root.
    #[must_use]
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }
}

/// Creates deterministic repository roots below one validated base directory.
#[derive(Debug, Clone)]
pub struct Materializer {
    base: PathBuf,
}

impl Materializer {
    /// Creates the base directory and resolves it to an absolute path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use spur_code_eval::Materializer;
    ///
    /// let materializer = Materializer::new("/var/tmp/spur-code-eval")?;
    /// # let _ = materializer;
    /// # Ok::<(), spur_code_eval::MaterializeError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`MaterializeError::Filesystem`] when the base cannot be created
    /// or canonicalized.
    pub fn new(base: impl AsRef<Path>) -> Result<Self, MaterializeError> {
        let base = base.as_ref();
        fs::create_dir_all(base).map_err(|error| filesystem("create directory", base, &error))?;
        let base = fs::canonicalize(base)
            .map_err(|error| filesystem("canonicalize directory", base, &error))?;
        Ok(Self { base })
    }

    /// Returns the deterministic final root for `case` without creating it.
    #[must_use]
    pub fn root_for(&self, case: &CodeEvalCase) -> PathBuf {
        self.base.join(root_identity_hash(case))
    }

    /// Creates a detached local clone and atomically promotes its case root.
    ///
    /// Existing final roots are never overwritten; they are validated against
    /// the requested case instead.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a path, Git command, metadata record, or root
    /// identity is invalid.
    pub fn materialize(&self, case: &CodeEvalCase) -> Result<MaterializedRoot, MaterializeError> {
        validate_subdirectory(case.repository_pin().subdirectory())?;
        let final_root = self.root_for(case);
        if final_root.exists() {
            return self.verify_existing(case, &final_root);
        }

        let identity_hash = root_identity_hash(case);
        let temporary_root = create_temporary_root(&self.base, &identity_hash)?;
        let mut temporary_root = TemporaryRoot::new(temporary_root);
        let checkout = temporary_root.path().join(CHECKOUT_DIRECTORY);

        clone_repository(case, &self.base, &checkout)?;
        checkout_detached(case, &checkout)?;
        verify_repository_state(case, &checkout)?;
        write_metadata(temporary_root.path(), &RootMetadata::from_case(case))?;

        fs::rename(temporary_root.path(), &final_root)
            .map_err(|error| filesystem("promote temporary root", &final_root, &error))?;
        temporary_root.disarm();
        self.verify_existing(case, &final_root)
    }

    /// Validates that `root` belongs to the requested case identity.
    ///
    /// # Errors
    ///
    /// Returns [`MaterializeError::MixedRepositoryRoot`] when another case,
    /// dataset revision, commit, or subtree produced the supplied root.
    pub fn verify_existing(
        &self,
        case: &CodeEvalCase,
        root: impl AsRef<Path>,
    ) -> Result<MaterializedRoot, MaterializeError> {
        validate_subdirectory(case.repository_pin().subdirectory())?;
        let root = root.as_ref();
        let metadata = read_metadata(root)?;
        let expected = RootMetadata::from_case(case);
        if !metadata.same_root_identity(&expected) {
            return Err(MaterializeError::MixedRepositoryRoot {
                root: root.to_path_buf(),
                expected: expected.audit_identity(),
                actual: metadata.audit_identity(),
            });
        }
        if metadata.repository_uri != expected.repository_uri {
            return Err(MaterializeError::OriginMismatch {
                root: root.to_path_buf(),
                expected: expected.repository_uri,
                actual: metadata.repository_uri,
            });
        }
        if metadata.dataset_content_hash != expected.dataset_content_hash {
            return Err(MaterializeError::DatasetHashMismatch {
                root: root.to_path_buf(),
                expected: expected.dataset_content_hash,
                actual: metadata.dataset_content_hash,
            });
        }
        if metadata.materialization_hash != expected.materialization_hash {
            return Err(MaterializeError::ContentHashMismatch {
                root: root.to_path_buf(),
                expected: expected.materialization_hash,
                actual: metadata.materialization_hash,
            });
        }
        let materialized = materialized_root(root, case.repository_pin().subdirectory())?;
        verify_repository_state(case, materialized.repository_root())?;
        Ok(materialized)
    }
}

/// Computes the canonical SHA-256 identity of tracked Git tree entries.
///
/// The hash covers the exact NUL-delimited `git ls-tree` bytes, including mode,
/// object kind, blob/tree object identifier, and repository-relative path. The
/// returned form is `sha256:<lowercase hex>`.
///
/// # Errors
///
/// Returns a typed error when `repository_root` is not readable as a Git
/// repository, or when `subdirectory` is unsafe.
pub fn compute_materialization_hash(
    repository_root: impl AsRef<Path>,
    subdirectory: Option<&str>,
) -> Result<String, MaterializeError> {
    validate_subdirectory(subdirectory)?;
    let repository_root = repository_root.as_ref();
    let mut arguments = vec![
        OsStr::new("ls-tree"),
        OsStr::new("-r"),
        OsStr::new("-z"),
        OsStr::new("--full-tree"),
        OsStr::new("HEAD"),
        OsStr::new("--"),
    ];
    if let Some(subdirectory) = subdirectory {
        arguments.push(OsStr::new(subdirectory));
    }
    let output = run_git("list tracked tree", repository_root, &arguments)?;
    let digest = Sha256::digest(output.stdout);
    Ok(format!("sha256:{digest:x}"))
}

#[derive(Debug, Serialize, Deserialize)]
struct RootMetadata {
    version: u32,
    suite: Suite,
    case_id: String,
    dataset_revision: String,
    dataset_content_hash: String,
    repository_uri: String,
    repository_commit: String,
    subdirectory: Option<String>,
    materialization_hash: String,
}

impl RootMetadata {
    fn from_case(case: &CodeEvalCase) -> Self {
        Self {
            version: METADATA_VERSION,
            suite: case.suite(),
            case_id: case.case_id().to_owned(),
            dataset_revision: case.dataset_pin().revision().to_owned(),
            dataset_content_hash: case.dataset_pin().content_hash().to_owned(),
            repository_uri: case.repository_pin().uri().to_owned(),
            repository_commit: case.repository_pin().commit_sha().to_owned(),
            subdirectory: case.repository_pin().subdirectory().map(str::to_owned),
            materialization_hash: case.repository_pin().materialization_hash().to_owned(),
        }
    }

    fn same_root_identity(&self, other: &Self) -> bool {
        self.version == other.version
            && self.suite == other.suite
            && self.case_id == other.case_id
            && self.dataset_revision == other.dataset_revision
            && self.repository_commit == other.repository_commit
            && self.subdirectory == other.subdirectory
    }

    fn audit_identity(&self) -> String {
        format!(
            "v{}:{:?}:{}:{}:{}:{}",
            self.version,
            self.suite,
            self.case_id,
            self.dataset_revision,
            self.repository_commit,
            self.subdirectory.as_deref().unwrap_or(".")
        )
    }
}

struct TemporaryRoot {
    path: PathBuf,
    armed: bool,
}

impl TemporaryRoot {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn clone_repository(
    case: &CodeEvalCase,
    current_dir: &Path,
    checkout: &Path,
) -> Result<(), MaterializeError> {
    run_git(
        "clone repository",
        current_dir,
        &[
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new("--no-local"),
            OsStr::new("--"),
            OsStr::new(case.repository_pin().uri()),
            checkout.as_os_str(),
        ],
    )?;
    Ok(())
}

fn create_temporary_root(base: &Path, identity_hash: &str) -> Result<PathBuf, MaterializeError> {
    loop {
        let sequence = NEXT_TEMPORARY_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!(
            ".{identity_hash}.tmp-{}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(filesystem("create temporary root", &path, &error)),
        }
    }
}

fn checkout_detached(case: &CodeEvalCase, checkout: &Path) -> Result<(), MaterializeError> {
    run_git(
        "checkout detached revision",
        checkout,
        &[
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new(case.repository_pin().commit_sha()),
        ],
    )?;
    Ok(())
}

fn verify_origin(case: &CodeEvalCase, repository_root: &Path) -> Result<(), MaterializeError> {
    let output = run_git(
        "read repository origin",
        repository_root,
        &[
            OsStr::new("remote"),
            OsStr::new("get-url"),
            OsStr::new("origin"),
        ],
    )?;
    let actual_bytes = trim_line_ending(&output.stdout);
    let expected = case.repository_pin().uri();
    if actual_bytes == expected.as_bytes() {
        Ok(())
    } else {
        Err(MaterializeError::OriginMismatch {
            root: repository_root.to_path_buf(),
            expected: expected.to_owned(),
            actual: String::from_utf8_lossy(actual_bytes).into_owned(),
        })
    }
}

fn verify_head(case: &CodeEvalCase, repository_root: &Path) -> Result<(), MaterializeError> {
    let output = run_git(
        "read repository HEAD",
        repository_root,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("HEAD^{commit}"),
        ],
    )?;
    let actual_bytes = trim_line_ending(&output.stdout);
    let actual = String::from_utf8_lossy(actual_bytes);
    let expected = case.repository_pin().commit_sha();
    if actual.eq_ignore_ascii_case(expected) {
        let reference = run_git(
            "read repository HEAD attachment",
            repository_root,
            &[
                OsStr::new("rev-parse"),
                OsStr::new("--abbrev-ref"),
                OsStr::new("HEAD"),
            ],
        )?;
        let reference = String::from_utf8_lossy(trim_line_ending(&reference.stdout));
        if reference == "HEAD" {
            Ok(())
        } else {
            Err(MaterializeError::AttachedHead {
                root: repository_root.to_path_buf(),
                reference: reference.into_owned(),
            })
        }
    } else {
        Err(MaterializeError::HeadMismatch {
            root: repository_root.to_path_buf(),
            expected: expected.to_owned(),
            actual: actual.into_owned(),
        })
    }
}

fn verify_clean(repository_root: &Path) -> Result<(), MaterializeError> {
    let output = run_git(
        "read repository status",
        repository_root,
        &[
            OsStr::new("status"),
            OsStr::new("--porcelain=v1"),
            OsStr::new("--untracked-files=all"),
            OsStr::new("-z"),
        ],
    )?;
    if output.stdout.is_empty() {
        Ok(())
    } else {
        Err(MaterializeError::DirtyCheckout {
            root: repository_root.to_path_buf(),
            status: format!("{:?}", String::from_utf8_lossy(&output.stdout)),
        })
    }
}

fn verify_content_hash(
    case: &CodeEvalCase,
    repository_root: &Path,
) -> Result<(), MaterializeError> {
    let actual =
        compute_materialization_hash(repository_root, case.repository_pin().subdirectory())?;
    let expected = case.repository_pin().materialization_hash();
    if actual == expected {
        Ok(())
    } else {
        Err(MaterializeError::ContentHashMismatch {
            root: repository_root.to_path_buf(),
            expected: expected.to_owned(),
            actual,
        })
    }
}

fn verify_repository_state(
    case: &CodeEvalCase,
    repository_root: &Path,
) -> Result<(), MaterializeError> {
    verify_subdirectory_exists(case, repository_root)?;
    verify_origin(case, repository_root)?;
    verify_head(case, repository_root)?;
    verify_clean(repository_root)?;
    verify_content_hash(case, repository_root)
}

fn verify_subdirectory_exists(
    case: &CodeEvalCase,
    repository_root: &Path,
) -> Result<(), MaterializeError> {
    resolve_source_root(repository_root, case.repository_pin().subdirectory()).map(drop)
}

fn run_git(
    operation: &'static str,
    current_dir: &Path,
    arguments: &[&OsStr],
) -> Result<Output, MaterializeError> {
    let output = Command::new("git")
        .args([
            "-c",
            "credential.interactive=never",
            "-c",
            "core.askPass=",
            "-c",
            "advice.detachedHead=false",
        ])
        .args(arguments)
        .current_dir(current_dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .output()
        .map_err(|error| filesystem("execute git", current_dir, &error))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(MaterializeError::GitCommand {
            operation,
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn write_metadata(root: &Path, metadata: &RootMetadata) -> Result<(), MaterializeError> {
    let path = root.join(METADATA_FILE);
    let bytes =
        serde_json::to_vec_pretty(metadata).map_err(|error| MaterializeError::InvalidMetadata {
            path: path.clone(),
            message: error.to_string(),
        })?;
    fs::write(&path, bytes).map_err(|error| filesystem("write metadata", &path, &error))
}

fn read_metadata(root: &Path) -> Result<RootMetadata, MaterializeError> {
    let path = root.join(METADATA_FILE);
    let bytes = fs::read(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            MaterializeError::MissingMetadata {
                root: root.to_path_buf(),
            }
        } else {
            filesystem("read metadata", &path, &error)
        }
    })?;
    let metadata: RootMetadata =
        serde_json::from_slice(&bytes).map_err(|error| MaterializeError::InvalidMetadata {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if metadata.version != METADATA_VERSION {
        return Err(MaterializeError::InvalidMetadata {
            path,
            message: format!(
                "unsupported metadata version {}; expected {METADATA_VERSION}",
                metadata.version
            ),
        });
    }
    Ok(metadata)
}

fn materialized_root(
    root: &Path,
    subdirectory: Option<&str>,
) -> Result<MaterializedRoot, MaterializeError> {
    let root =
        fs::canonicalize(root).map_err(|error| filesystem("canonicalize root", root, &error))?;
    let repository_root = root.join(CHECKOUT_DIRECTORY);
    let source_root = resolve_source_root(&repository_root, subdirectory)?;
    Ok(MaterializedRoot {
        root,
        repository_root,
        source_root,
    })
}

fn resolve_source_root(
    repository_root: &Path,
    subdirectory: Option<&str>,
) -> Result<PathBuf, MaterializeError> {
    let Some(subdirectory) = subdirectory else {
        return Ok(repository_root.to_path_buf());
    };
    let candidate = repository_root.join(subdirectory);
    let resolved = fs::canonicalize(&candidate).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            MaterializeError::MissingSubdirectory {
                root: repository_root.to_path_buf(),
                subdirectory: subdirectory.to_owned(),
            }
        } else {
            filesystem("canonicalize repository subdirectory", &candidate, &error)
        }
    })?;
    let canonical_repository = fs::canonicalize(repository_root)
        .map_err(|error| filesystem("canonicalize repository root", repository_root, &error))?;
    if !resolved.starts_with(&canonical_repository) {
        return Err(MaterializeError::SubdirectoryEscapesRoot {
            root: canonical_repository,
            subdirectory: subdirectory.to_owned(),
            resolved,
        });
    }
    if resolved.is_dir() {
        Ok(resolved)
    } else {
        Err(MaterializeError::MissingSubdirectory {
            root: canonical_repository,
            subdirectory: subdirectory.to_owned(),
        })
    }
}

fn validate_subdirectory(subdirectory: Option<&str>) -> Result<(), MaterializeError> {
    let Some(subdirectory) = subdirectory else {
        return Ok(());
    };
    let path = Path::new(subdirectory);
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(MaterializeError::InvalidSubdirectory {
            subdirectory: subdirectory.to_owned(),
        })
    }
}

fn root_identity_hash(case: &CodeEvalCase) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, suite_name(case.suite()).as_bytes());
    hash_field(&mut hasher, case.case_id().as_bytes());
    hash_field(&mut hasher, case.dataset_pin().revision().as_bytes());
    hash_field(&mut hasher, case.repository_pin().commit_sha().as_bytes());
    hash_field(
        &mut hasher,
        case.repository_pin()
            .subdirectory()
            .unwrap_or(".")
            .as_bytes(),
    );
    format!("{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(value.len().to_le_bytes());
    hasher.update(value);
}

fn trim_line_ending(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

const fn suite_name(suite: Suite) -> &'static str {
    match suite {
        Suite::RepoQa => "repo_qa",
        Suite::CrossCodeEval => "cross_code_eval",
        Suite::Jcg => "jcg",
    }
}

fn filesystem(operation: &'static str, path: &Path, error: &io::Error) -> MaterializeError {
    MaterializeError::Filesystem {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}
