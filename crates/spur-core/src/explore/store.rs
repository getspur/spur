use directories::BaseDirs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};

#[cfg(test)]
static GLOBAL_ROOT_OVERRIDE: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);
#[cfg(test)]
static GLOBAL_ROOT_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

pub fn global_root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = GLOBAL_ROOT_OVERRIDE.lock().unwrap().clone() {
        return root;
    }

    BaseDirs::new().map(|dirs| dirs.home_dir().join(".spur/explore"))
}

#[cfg(test)]
pub(crate) struct GlobalRootOverrideGuard {
    previous: Option<Option<PathBuf>>,
    _lock: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for GlobalRootOverrideGuard {
    fn drop(&mut self) {
        *GLOBAL_ROOT_OVERRIDE.lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
pub(crate) fn force_global_root_for_tests(root: Option<PathBuf>) -> GlobalRootOverrideGuard {
    let lock = GLOBAL_ROOT_OVERRIDE_LOCK.lock().unwrap();
    let mut override_slot = GLOBAL_ROOT_OVERRIDE.lock().unwrap();
    let previous = override_slot.replace(root);
    GlobalRootOverrideGuard {
        previous,
        _lock: lock,
    }
}

pub fn local_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".spur/explore")
}

pub fn catalog_path_in_store(store_root: &Path) -> PathBuf {
    store_root.join("index/catalog.json")
}

pub fn local_catalog_path(repo_root: &Path) -> PathBuf {
    catalog_path_in_store(&local_root(repo_root))
}

pub fn cache_dir_in_store(store_root: &Path, repo: &str) -> PathBuf {
    store_root
        .join("cache")
        .join(repo.replace(['/', '\\'], "-"))
}

pub fn local_cache_dir(repo_root: &Path, repo: &str) -> PathBuf {
    cache_dir_in_store(&local_root(repo_root), repo)
}

pub fn layered_cache_dir(repo_root: &Path, repo: &str) -> PathBuf {
    if let Some(global) = global_root().filter(|root| root.exists()) {
        let global_cache = cache_dir_in_store(&global, repo);
        if global_cache.exists() {
            return global_cache;
        }
    }
    local_cache_dir(repo_root, repo)
}

pub fn pool_dir_in_store(
    store_root: &Path,
    source: &str,
    name: &str,
    pinned_commit: &str,
) -> PathBuf {
    let owner = source.split('/').next().unwrap_or(source);
    let sha7 = pinned_commit.get(..7).unwrap_or(pinned_commit);
    store_root
        .join("pool")
        .join(owner)
        .join(format!("{name}@{sha7}"))
}

pub fn local_pool_dir(repo_root: &Path, source: &str, name: &str, pinned_commit: &str) -> PathBuf {
    pool_dir_in_store(&local_root(repo_root), source, name, pinned_commit)
}

pub fn layered_pool_dir(
    repo_root: &Path,
    source: &str,
    name: &str,
    pinned_commit: &str,
) -> PathBuf {
    if let Some(global) = global_root().filter(|root| root.exists()) {
        let global_pool = pool_dir_in_store(&global, source, name, pinned_commit);
        if global_pool.exists() {
            return global_pool;
        }
    }
    local_pool_dir(repo_root, source, name, pinned_commit)
}

pub fn manifest_path_in_store(store_root: &Path) -> PathBuf {
    store_root.join("manifest.toml")
}

pub fn local_manifest_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".spur/explore.toml")
}
