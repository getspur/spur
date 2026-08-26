use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context as _};
use async_trait::async_trait;
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher as _};
use serde::Deserialize;
use tokio::sync::mpsc;
use watchman_client::prelude::*;
use watchman_client::{Subscription, SubscriptionData};

watchman_client::query_result_type! {
    struct WatchmanFile {
        name: NameField,
        exists: ExistsField,
    }
}

/// The complete physical source set that can affect one worktree overlay.
///
/// The three semantic roles remain independently addressable even when two
/// roles resolve to the same physical directory. `roots()` is the canonical,
/// sorted, de-duplicated set that providers arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSourceSet {
    worktree: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    roots: Vec<PathBuf>,
}

impl ChangeSourceSet {
    /// Resolves the canonical worktree, per-worktree gitdir, and shared
    /// commondir without changing repository configuration.
    pub fn resolve(root: &Path) -> anyhow::Result<Self> {
        let worktree = canonical_git_path(root, &["rev-parse", "--show-toplevel"], "worktree")?;
        let git_dir = canonical_git_path(
            root,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
            "gitdir",
        )?;
        let common_dir = canonical_git_path(
            root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            "commondir",
        )?;
        let roots = BTreeSet::from([worktree.clone(), git_dir.clone(), common_dir.clone()])
            .into_iter()
            .collect();
        Ok(Self {
            worktree,
            git_dir,
            common_dir,
            roots,
        })
    }

    pub fn worktree(&self) -> &Path {
        &self.worktree
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    fn is_git_metadata(&self, path: &Path) -> bool {
        path.starts_with(&self.git_dir) || path.starts_with(&self.common_dir)
    }

    fn contains_root(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| root == path)
    }

    fn owning_roots<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = &'a PathBuf> + 'a {
        self.roots.iter().filter(move |root| path.starts_with(root))
    }
}

fn canonical_git_path(root: &Path, args: &[&str], role: &str) -> anyhow::Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to resolve {role} for `{}`", root.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {args:?} failed while resolving {role} for `{}`: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    let value = std::str::from_utf8(&output.stdout)
        .with_context(|| format!("git emitted a non-UTF-8 {role} path"))?
        .trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        return Err(anyhow!("git emitted an empty {role} path"));
    }
    PathBuf::from(value)
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {role} `{value}`"))
}

/// One provider clock for every armed physical source root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompositeCursor {
    entries: BTreeMap<PathBuf, String>,
}

impl CompositeCursor {
    pub fn from_entries(entries: impl IntoIterator<Item = (PathBuf, String)>) -> CompositeCursor {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    pub fn entries(&self) -> &BTreeMap<PathBuf, String> {
        &self.entries
    }

    fn set(&mut self, root: PathBuf, cursor: String) {
        self.entries.insert(root, cursor);
    }
}

/// Provider order is fixed: Watchman, then notify, then exact-only recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChangeProviderKind {
    Watchman,
    Notify,
    ExactOnly,
}

/// Every condition that invalidates change-provider continuity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustLoss {
    ProviderError {
        provider: ChangeProviderKind,
        message: String,
    },
    Overflow {
        provider: ChangeProviderKind,
    },
    Recrawl,
    FreshInstance,
    Disconnected {
        provider: ChangeProviderKind,
    },
    ChannelDisconnected {
        provider: ChangeProviderKind,
    },
    RootReplaced {
        root: PathBuf,
    },
    AmbiguousEvent {
        provider: ChangeProviderKind,
        detail: String,
    },
    ProvidersUnavailable {
        watchman: String,
        notify: String,
    },
}

/// A provider-neutral, sorted, de-duplicated change observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeBatch {
    Changes {
        cursor: CompositeCursor,
        added: BTreeSet<PathBuf>,
        modified: BTreeSet<PathBuf>,
        deleted: BTreeSet<PathBuf>,
        renamed: BTreeSet<(PathBuf, PathBuf)>,
        git_metadata: BTreeSet<PathBuf>,
    },
    TrustLost {
        cursor: CompositeCursor,
        reason: TrustLoss,
    },
}

impl ChangeBatch {
    pub fn cursor(&self) -> &CompositeCursor {
        match self {
            Self::Changes { cursor, .. } | Self::TrustLost { cursor, .. } => cursor,
        }
    }
}

#[derive(Debug, Clone)]
enum RawChange {
    Added(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    TrustLost(TrustLoss),
}

fn normalize_raw_batch(
    sources: &ChangeSourceSet,
    provider: ChangeProviderKind,
    cursor: CompositeCursor,
    changes: impl IntoIterator<Item = RawChange>,
) -> ChangeBatch {
    let mut added = BTreeSet::new();
    let mut modified = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    let mut renamed = BTreeSet::new();
    let mut git_metadata = BTreeSet::new();
    let mut observed = false;

    for change in changes {
        observed = true;
        let trust_loss = match change {
            RawChange::TrustLost(reason) => Some(reason),
            RawChange::Added(path) => {
                let path = normalize_path(sources.worktree(), path);
                if sources.is_git_metadata(&path) {
                    git_metadata.insert(path);
                } else {
                    deleted.remove(&path);
                    modified.remove(&path);
                    added.insert(path);
                }
                None
            }
            RawChange::Modified(path) => {
                let path = normalize_path(sources.worktree(), path);
                if sources.is_git_metadata(&path) {
                    git_metadata.insert(path);
                } else if !added.contains(&path) && !deleted.contains(&path) {
                    modified.insert(path);
                }
                None
            }
            RawChange::Deleted(path) => {
                let path = normalize_path(sources.worktree(), path);
                if sources.is_git_metadata(&path) {
                    git_metadata.insert(path);
                } else {
                    added.remove(&path);
                    modified.remove(&path);
                    deleted.insert(path);
                }
                None
            }
            RawChange::Renamed { from, to } => {
                let from = normalize_path(sources.worktree(), from);
                let to = normalize_path(sources.worktree(), to);
                if sources.is_git_metadata(&from) || sources.is_git_metadata(&to) {
                    git_metadata.extend([from, to]);
                } else {
                    added.remove(&from);
                    added.remove(&to);
                    modified.remove(&from);
                    modified.remove(&to);
                    deleted.remove(&from);
                    deleted.remove(&to);
                    renamed.insert((from, to));
                }
                None
            }
        };
        if let Some(reason) = trust_loss {
            return ChangeBatch::TrustLost { cursor, reason };
        }
    }

    if !observed {
        return ChangeBatch::TrustLost {
            cursor,
            reason: TrustLoss::AmbiguousEvent {
                provider,
                detail: "provider emitted an empty batch".to_owned(),
            },
        };
    }

    ChangeBatch::Changes {
        cursor,
        added,
        modified,
        deleted,
        renamed,
        git_metadata,
    }
}

fn normalize_path(worktree: &Path, path: PathBuf) -> PathBuf {
    let path = if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// An armed provider plus its capture cursor and normalized event stream.
pub struct ChangeSubscription {
    provider: ChangeProviderKind,
    initial_cursor: CompositeCursor,
    cursor: CompositeCursor,
    receiver: mpsc::UnboundedReceiver<ChangeBatch>,
    _guard: Option<Box<dyn Send>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl ChangeSubscription {
    pub fn provider(&self) -> ChangeProviderKind {
        self.provider
    }

    pub fn initial_cursor(&self) -> &CompositeCursor {
        &self.initial_cursor
    }

    pub fn cursor(&self) -> &CompositeCursor {
        &self.cursor
    }

    /// Returns a trust-loss batch when the provider channel disconnects.
    pub async fn next_batch(&mut self) -> ChangeBatch {
        match self.receiver.recv().await {
            Some(batch) => {
                self.cursor = batch.cursor().clone();
                batch
            }
            None => ChangeBatch::TrustLost {
                cursor: self.cursor.clone(),
                reason: TrustLoss::ChannelDisconnected {
                    provider: self.provider,
                },
            },
        }
    }

    fn active(
        provider: ChangeProviderKind,
        initial_cursor: CompositeCursor,
        receiver: mpsc::UnboundedReceiver<ChangeBatch>,
        guard: Option<Box<dyn Send>>,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    ) -> Self {
        Self {
            provider,
            cursor: initial_cursor.clone(),
            initial_cursor,
            receiver,
            _guard: guard,
            tasks,
        }
    }

    fn exact_only(watchman: anyhow::Error, notify: anyhow::Error) -> Self {
        let cursor = CompositeCursor::default();
        let (sender, receiver) = mpsc::unbounded_channel();
        let _ = sender.send(ChangeBatch::TrustLost {
            cursor: cursor.clone(),
            reason: TrustLoss::ProvidersUnavailable {
                watchman: format!("{watchman:#}"),
                notify: format!("{notify:#}"),
            },
        });
        drop(sender);
        Self::active(
            ChangeProviderKind::ExactOnly,
            cursor,
            receiver,
            None,
            Vec::new(),
        )
    }

    #[cfg(test)]
    fn for_test(
        provider: ChangeProviderKind,
        initial_cursor: CompositeCursor,
        batches: Vec<ChangeBatch>,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        for batch in batches {
            sender.send(batch).expect("test subscription receiver");
        }
        drop(sender);
        Self::active(provider, initial_cursor, receiver, None, Vec::new())
    }
}

impl Drop for ChangeSubscription {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[async_trait]
trait ProviderConnector: Send + Sync {
    async fn subscribe(&self, sources: &ChangeSourceSet) -> anyhow::Result<ChangeSubscription>;
}

/// Creates a total provider selection: Watchman first, notify second, and a
/// terminal exact-only subscription if neither provider can be armed.
#[derive(Clone)]
pub struct SubscriptionFactory {
    watchman: Arc<dyn ProviderConnector>,
    notify: Arc<dyn ProviderConnector>,
}

impl Default for SubscriptionFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionFactory {
    pub fn new() -> Self {
        Self {
            watchman: Arc::new(WatchmanConnector),
            notify: Arc::new(NotifyConnector),
        }
    }

    pub async fn subscribe(&self, sources: &ChangeSourceSet) -> ChangeSubscription {
        match self.watchman.subscribe(sources).await {
            Ok(subscription) => subscription,
            Err(watchman_error) => match self.notify.subscribe(sources).await {
                Ok(subscription) => subscription,
                Err(notify_error) => ChangeSubscription::exact_only(watchman_error, notify_error),
            },
        }
    }

    #[cfg(test)]
    fn with_connectors(
        watchman: Arc<dyn ProviderConnector>,
        notify: Arc<dyn ProviderConnector>,
    ) -> Self {
        Self { watchman, notify }
    }
}

struct WatchmanConnector;

#[async_trait]
impl ProviderConnector for WatchmanConnector {
    async fn subscribe(&self, sources: &ChangeSourceSet) -> anyhow::Result<ChangeSubscription> {
        let client = Connector::new()
            .connect()
            .await
            .context("failed to connect to Watchman")?;
        let mut initial_cursor = CompositeCursor::default();
        let mut subscriptions = Vec::with_capacity(sources.roots().len());

        for root in sources.roots() {
            let canonical = CanonicalPath::with_canonicalized_path(root.clone());
            let resolved = client
                .resolve_root(canonical)
                .await
                .with_context(|| format!("Watchman failed to resolve `{}`", root.display()))?;
            let clock = client
                .clock(&resolved, SyncTimeout::Default)
                .await
                .with_context(|| format!("Watchman failed to clock `{}`", root.display()))?;
            let since = Clock::Spec(clock);
            initial_cursor.set(root.clone(), encode_watchman_clock(&since)?);
            let (subscription, _response) = client
                .subscribe::<WatchmanFile>(
                    &resolved,
                    SubscribeRequest {
                        since: Some(since),
                        empty_on_fresh_instance: false,
                        ..SubscribeRequest::default()
                    },
                )
                .await
                .with_context(|| format!("Watchman failed to subscribe `{}`", root.display()))?;
            subscriptions.push((root.clone(), subscription));
        }

        let (sender, receiver) = mpsc::unbounded_channel();
        let shared_cursor = Arc::new(Mutex::new(initial_cursor.clone()));
        let mut tasks = Vec::with_capacity(subscriptions.len());
        for (root, subscription) in subscriptions {
            tasks.push(spawn_watchman_subscription(
                sources.clone(),
                root,
                subscription,
                Arc::clone(&shared_cursor),
                sender.clone(),
            ));
        }
        drop(sender);

        Ok(ChangeSubscription::active(
            ChangeProviderKind::Watchman,
            initial_cursor,
            receiver,
            None,
            tasks,
        ))
    }
}

fn encode_watchman_clock(clock: &Clock) -> anyhow::Result<String> {
    serde_json::to_string(clock).context("failed to encode Watchman clock")
}

fn spawn_watchman_subscription(
    sources: ChangeSourceSet,
    root: PathBuf,
    mut subscription: Subscription<WatchmanFile>,
    cursor: Arc<Mutex<CompositeCursor>>,
    sender: mpsc::UnboundedSender<ChangeBatch>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match subscription.next().await {
                Ok(SubscriptionData::FilesChanged(result)) => {
                    let encoded_clock = match encode_watchman_clock(&result.clock) {
                        Ok(clock) => clock,
                        Err(error) => {
                            send_watchman_batch(
                                &sources,
                                &cursor,
                                &sender,
                                None,
                                vec![RawChange::TrustLost(TrustLoss::ProviderError {
                                    provider: ChangeProviderKind::Watchman,
                                    message: format!("{error:#}"),
                                })],
                            );
                            break;
                        }
                    };
                    if result.is_fresh_instance {
                        send_watchman_batch(
                            &sources,
                            &cursor,
                            &sender,
                            Some((root.clone(), encoded_clock)),
                            vec![RawChange::TrustLost(TrustLoss::FreshInstance)],
                        );
                        break;
                    }
                    let changes: Vec<_> = result
                        .files
                        .unwrap_or_default()
                        .into_iter()
                        .map(|file| {
                            let path = root.join(file.name.into_inner());
                            if file.exists.into_inner() {
                                RawChange::Modified(path)
                            } else {
                                RawChange::Deleted(path)
                            }
                        })
                        .collect();
                    if changes.is_empty() {
                        lock_cursor(&cursor).set(root.clone(), encoded_clock);
                        continue;
                    }
                    if !send_watchman_batch(
                        &sources,
                        &cursor,
                        &sender,
                        Some((root.clone(), encoded_clock)),
                        changes,
                    ) {
                        break;
                    }
                }
                Ok(SubscriptionData::Canceled) => {
                    send_watchman_batch(
                        &sources,
                        &cursor,
                        &sender,
                        None,
                        vec![RawChange::TrustLost(TrustLoss::Disconnected {
                            provider: ChangeProviderKind::Watchman,
                        })],
                    );
                    break;
                }
                Ok(SubscriptionData::StateEnter { state_name, .. }) => {
                    send_watchman_batch(
                        &sources,
                        &cursor,
                        &sender,
                        None,
                        vec![RawChange::TrustLost(TrustLoss::AmbiguousEvent {
                            provider: ChangeProviderKind::Watchman,
                            detail: format!("Watchman entered state `{state_name}`"),
                        })],
                    );
                    break;
                }
                Ok(SubscriptionData::StateLeave { state_name, .. }) => {
                    send_watchman_batch(
                        &sources,
                        &cursor,
                        &sender,
                        None,
                        vec![RawChange::TrustLost(TrustLoss::AmbiguousEvent {
                            provider: ChangeProviderKind::Watchman,
                            detail: format!("Watchman left state `{state_name}`"),
                        })],
                    );
                    break;
                }
                Err(error) => {
                    send_watchman_batch(
                        &sources,
                        &cursor,
                        &sender,
                        None,
                        vec![RawChange::TrustLost(TrustLoss::ProviderError {
                            provider: ChangeProviderKind::Watchman,
                            message: format!("{error:#}"),
                        })],
                    );
                    break;
                }
            }
        }
    })
}

fn send_watchman_batch(
    sources: &ChangeSourceSet,
    cursor: &Mutex<CompositeCursor>,
    sender: &mpsc::UnboundedSender<ChangeBatch>,
    cursor_update: Option<(PathBuf, String)>,
    changes: Vec<RawChange>,
) -> bool {
    let mut cursor = lock_cursor(cursor);
    if let Some((root, value)) = cursor_update {
        cursor.set(root, value);
    }
    let batch = normalize_raw_batch(
        sources,
        ChangeProviderKind::Watchman,
        cursor.clone(),
        changes,
    );
    sender.send(batch).is_ok()
}

fn lock_cursor(cursor: &Mutex<CompositeCursor>) -> std::sync::MutexGuard<'_, CompositeCursor> {
    cursor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct NotifyConnector;

#[async_trait]
impl ProviderConnector for NotifyConnector {
    async fn subscribe(&self, sources: &ChangeSourceSet) -> anyhow::Result<ChangeSubscription> {
        let initial_cursor = CompositeCursor::from_entries(
            sources
                .roots()
                .iter()
                .cloned()
                .map(|root| (root, "0".to_owned())),
        );
        let shared_cursor = Arc::new(Mutex::new(initial_cursor.clone()));
        let sequence = Arc::new(AtomicU64::new(0));
        let callback_sources = sources.clone();
        let callback_cursor = Arc::clone(&shared_cursor);
        let callback_sequence = Arc::clone(&sequence);
        let (sender, receiver) = mpsc::unbounded_channel();
        let callback_sender = sender.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let sequence = callback_sequence.fetch_add(1, Ordering::Relaxed) + 1;
            let raw_changes = match event {
                Ok(event) => {
                    let raw_changes = raw_changes_from_notify(&callback_sources, event);
                    if raw_changes.is_empty() {
                        return;
                    }
                    raw_changes
                }
                Err(error) => {
                    vec![RawChange::TrustLost(TrustLoss::ProviderError {
                        provider: ChangeProviderKind::Notify,
                        message: error.to_string(),
                    })]
                }
            };
            send_notify_batch(
                &callback_sources,
                &callback_cursor,
                &callback_sender,
                sequence,
                raw_changes,
            );
        })
        .context("failed to create notify watcher")?;

        for root in sources.roots() {
            watcher
                .watch(root, RecursiveMode::Recursive)
                .with_context(|| format!("notify failed to watch `{}`", root.display()))?;
        }
        drop(sender);

        Ok(ChangeSubscription::active(
            ChangeProviderKind::Notify,
            initial_cursor,
            receiver,
            Some(Box::new(watcher)),
            Vec::new(),
        ))
    }
}

fn raw_changes_from_notify(sources: &ChangeSourceSet, event: Event) -> Vec<RawChange> {
    let paths: Vec<_> = event
        .paths
        .into_iter()
        .map(|path| normalize_path(sources.worktree(), path))
        .collect();

    if is_root_replacement(sources, &event.kind, &paths) {
        return paths
            .into_iter()
            .filter(|path| sources.contains_root(path))
            .map(|root| RawChange::TrustLost(TrustLoss::RootReplaced { root }))
            .collect();
    }
    if paths.is_empty() && !matches!(&event.kind, EventKind::Access(_)) {
        return vec![RawChange::TrustLost(TrustLoss::AmbiguousEvent {
            provider: ChangeProviderKind::Notify,
            detail: format!("notify emitted {:?} without a path", event.kind),
        })];
    }

    match event.kind {
        EventKind::Access(_) => Vec::new(),
        EventKind::Create(_) => paths.into_iter().map(RawChange::Added).collect(),
        EventKind::Remove(_) => paths.into_iter().map(RawChange::Deleted).collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() == 2 => {
            vec![RawChange::Renamed {
                from: paths[0].clone(),
                to: paths[1].clone(),
            }]
        }
        EventKind::Modify(ModifyKind::Name(_)) => {
            vec![RawChange::TrustLost(TrustLoss::AmbiguousEvent {
                provider: ChangeProviderKind::Notify,
                detail: format!(
                    "notify emitted an unpaired rename with {} path(s)",
                    paths.len()
                ),
            })]
        }
        EventKind::Modify(_) => paths.into_iter().map(RawChange::Modified).collect(),
        EventKind::Any | EventKind::Other => {
            vec![RawChange::TrustLost(TrustLoss::AmbiguousEvent {
                provider: ChangeProviderKind::Notify,
                detail: format!("notify emitted ambiguous event kind {:?}", event.kind),
            })]
        }
    }
}

fn is_root_replacement(sources: &ChangeSourceSet, kind: &EventKind, paths: &[PathBuf]) -> bool {
    let replaces_path = matches!(
        kind,
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    );
    replaces_path && paths.iter().any(|path| sources.contains_root(path))
}

fn send_notify_batch(
    sources: &ChangeSourceSet,
    cursor: &Mutex<CompositeCursor>,
    sender: &mpsc::UnboundedSender<ChangeBatch>,
    sequence: u64,
    changes: Vec<RawChange>,
) {
    let mut affected = BTreeSet::new();
    for change in &changes {
        match change {
            RawChange::Added(path) | RawChange::Modified(path) | RawChange::Deleted(path) => {
                affected.extend(sources.owning_roots(path).cloned());
            }
            RawChange::Renamed { from, to } => {
                affected.extend(sources.owning_roots(from).cloned());
                affected.extend(sources.owning_roots(to).cloned());
            }
            RawChange::TrustLost(_) => {}
        }
    }
    let mut cursor = lock_cursor(cursor);
    for root in affected {
        let current = cursor
            .entries
            .get(&root)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        if sequence > current {
            cursor.set(root, sequence.to_string());
        }
    }
    let batch = normalize_raw_batch(sources, ChangeProviderKind::Notify, cursor.clone(), changes);
    let _ = sender.send(batch);
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use notify::event::{ModifyKind, RenameMode};
    use notify::{Event, EventKind};
    use tokio::sync::mpsc;

    use super::{
        normalize_raw_batch, raw_changes_from_notify, send_notify_batch, ChangeBatch,
        ChangeProviderKind, ChangeSourceSet, ChangeSubscription, CompositeCursor,
        ProviderConnector, RawChange, SubscriptionFactory, TrustLoss,
    };

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository_fixture() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let repository = temp.path().join("repository");
        std::fs::create_dir(&repository).expect("repository directory");
        run_git(&repository, &["init", "--quiet"]);
        run_git(
            &repository,
            &[
                "-c",
                "user.name=Spur Test",
                "-c",
                "user.email=spur@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        (temp, repository)
    }

    #[test]
    fn overlay_watch_resolves_normal_repository_and_collapses_shared_git_roles() {
        let (_temp, repository) = repository_fixture();

        let sources = ChangeSourceSet::resolve(&repository).expect("resolve sources");
        let canonical_repository = repository.canonicalize().expect("canonical repository");

        assert_eq!(sources.worktree(), canonical_repository);
        assert_eq!(sources.git_dir(), sources.common_dir());
        assert_eq!(sources.roots().len(), 2);
        assert!(sources.roots().windows(2).all(|pair| pair[0] < pair[1]));
        assert!(sources.roots().contains(&sources.worktree().to_path_buf()));
        assert!(sources.roots().contains(&sources.git_dir().to_path_buf()));
    }

    #[test]
    fn overlay_watch_resolves_linked_worktree_gitdir_and_shared_commondir() {
        let (temp, repository) = repository_fixture();
        let linked = temp.path().join("linked worktree");
        run_git(
            &repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "overlay-watch-linked",
                linked.to_str().expect("UTF-8 linked path"),
            ],
        );

        let main = ChangeSourceSet::resolve(&repository).expect("main sources");
        let linked = ChangeSourceSet::resolve(&linked).expect("linked sources");

        assert_eq!(main.common_dir(), linked.common_dir());
        assert_ne!(linked.git_dir(), linked.common_dir());
        assert_eq!(linked.roots().len(), 3);
        assert!(linked.roots().contains(&linked.worktree().to_path_buf()));
        assert!(linked.roots().contains(&linked.git_dir().to_path_buf()));
        assert!(linked.roots().contains(&linked.common_dir().to_path_buf()));
    }

    #[derive(Clone)]
    struct FakeConnector {
        kind: ChangeProviderKind,
        failure: Option<&'static str>,
        calls: Arc<Mutex<Vec<ChangeProviderKind>>>,
        initial_cursor: CompositeCursor,
        batches: Vec<ChangeBatch>,
    }

    #[async_trait]
    impl ProviderConnector for FakeConnector {
        async fn subscribe(
            &self,
            _sources: &ChangeSourceSet,
        ) -> anyhow::Result<ChangeSubscription> {
            self.calls.lock().expect("call log").push(self.kind);
            if let Some(message) = self.failure {
                anyhow::bail!(message);
            }
            Ok(ChangeSubscription::for_test(
                self.kind,
                self.initial_cursor.clone(),
                self.batches.clone(),
            ))
        }
    }

    fn fake_factory(
        watchman_failure: Option<&'static str>,
        notify_failure: Option<&'static str>,
    ) -> (SubscriptionFactory, Arc<Mutex<Vec<ChangeProviderKind>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cursor = CompositeCursor::default();
        let watchman = FakeConnector {
            kind: ChangeProviderKind::Watchman,
            failure: watchman_failure,
            calls: Arc::clone(&calls),
            initial_cursor: cursor.clone(),
            batches: Vec::new(),
        };
        let notify = FakeConnector {
            kind: ChangeProviderKind::Notify,
            failure: notify_failure,
            calls: Arc::clone(&calls),
            initial_cursor: cursor,
            batches: Vec::new(),
        };
        (
            SubscriptionFactory::with_connectors(Arc::new(watchman), Arc::new(notify)),
            calls,
        )
    }

    #[tokio::test]
    async fn overlay_watch_selects_watchman_then_notify_then_exact_only() {
        let (_temp, repository) = repository_fixture();
        let sources = ChangeSourceSet::resolve(&repository).expect("sources");

        let (factory, calls) = fake_factory(None, None);
        let subscription = factory.subscribe(&sources).await;
        assert_eq!(subscription.provider(), ChangeProviderKind::Watchman);
        assert_eq!(
            *calls.lock().expect("call log"),
            vec![ChangeProviderKind::Watchman]
        );

        let (factory, calls) = fake_factory(Some("watchman unavailable"), None);
        let subscription = factory.subscribe(&sources).await;
        assert_eq!(subscription.provider(), ChangeProviderKind::Notify);
        assert_eq!(
            *calls.lock().expect("call log"),
            vec![ChangeProviderKind::Watchman, ChangeProviderKind::Notify]
        );

        let (factory, calls) =
            fake_factory(Some("watchman unavailable"), Some("notify unavailable"));
        let mut subscription = factory.subscribe(&sources).await;
        assert_eq!(subscription.provider(), ChangeProviderKind::ExactOnly);
        assert_eq!(
            *calls.lock().expect("call log"),
            vec![ChangeProviderKind::Watchman, ChangeProviderKind::Notify]
        );
        assert!(matches!(
            subscription.next_batch().await,
            ChangeBatch::TrustLost {
                reason: TrustLoss::ProvidersUnavailable { .. },
                ..
            }
        ));
    }

    #[test]
    fn overlay_watch_normalizes_changes_and_git_metadata_deterministically() {
        let (_temp, repository) = repository_fixture();
        let sources = ChangeSourceSet::resolve(&repository).expect("sources");
        let cursor = CompositeCursor::from_entries([(
            sources.worktree().to_path_buf(),
            "clock-1".to_owned(),
        )]);
        let added = sources.worktree().join("src/added.rs");
        let modified = sources.worktree().join("src/modified.rs");
        let deleted = sources.worktree().join("src/deleted.rs");
        let renamed_from = sources.worktree().join("src/old.rs");
        let renamed_to = sources.worktree().join("src/new.rs");
        let git_metadata = sources.common_dir().join("refs/heads/main");

        let batch = normalize_raw_batch(
            &sources,
            ChangeProviderKind::Notify,
            cursor.clone(),
            [
                RawChange::Modified(modified.clone()),
                RawChange::Added(added.clone()),
                RawChange::Modified(git_metadata.clone()),
                RawChange::Deleted(deleted.clone()),
                RawChange::Renamed {
                    from: renamed_from.clone(),
                    to: renamed_to.clone(),
                },
                RawChange::Modified(modified.clone()),
            ],
        );

        assert_eq!(
            batch,
            ChangeBatch::Changes {
                cursor,
                added: BTreeSet::from([added]),
                modified: BTreeSet::from([modified]),
                deleted: BTreeSet::from([deleted]),
                renamed: BTreeSet::from([(renamed_from, renamed_to)]),
                git_metadata: BTreeSet::from([git_metadata]),
            }
        );
    }

    #[test]
    fn overlay_watch_provider_loss_never_becomes_empty_success() {
        let (_temp, repository) = repository_fixture();
        let sources = ChangeSourceSet::resolve(&repository).expect("sources");
        let cursor = CompositeCursor::default();
        let cases = [
            (
                RawChange::TrustLost(TrustLoss::ProviderError {
                    provider: ChangeProviderKind::Watchman,
                    message: "watchman error".to_owned(),
                }),
                TrustLoss::ProviderError {
                    provider: ChangeProviderKind::Watchman,
                    message: "watchman error".to_owned(),
                },
            ),
            (
                RawChange::TrustLost(TrustLoss::Overflow {
                    provider: ChangeProviderKind::Watchman,
                }),
                TrustLoss::Overflow {
                    provider: ChangeProviderKind::Watchman,
                },
            ),
            (RawChange::TrustLost(TrustLoss::Recrawl), TrustLoss::Recrawl),
            (
                RawChange::TrustLost(TrustLoss::FreshInstance),
                TrustLoss::FreshInstance,
            ),
            (
                RawChange::TrustLost(TrustLoss::Disconnected {
                    provider: ChangeProviderKind::Watchman,
                }),
                TrustLoss::Disconnected {
                    provider: ChangeProviderKind::Watchman,
                },
            ),
        ];

        for (raw, expected) in cases {
            assert_eq!(
                normalize_raw_batch(
                    &sources,
                    ChangeProviderKind::Watchman,
                    cursor.clone(),
                    [raw]
                ),
                ChangeBatch::TrustLost {
                    cursor: cursor.clone(),
                    reason: expected,
                }
            );
        }

        assert!(matches!(
            normalize_raw_batch(
                &sources,
                ChangeProviderKind::Notify,
                cursor,
                [RawChange::TrustLost(TrustLoss::RootReplaced {
                    root: sources.worktree().to_path_buf()
                })]
            ),
            ChangeBatch::TrustLost {
                reason: TrustLoss::RootReplaced { .. },
                ..
            }
        ));
    }

    #[test]
    fn overlay_watch_notify_ambiguous_events_revoke_trust() {
        let (_temp, repository) = repository_fixture();
        let sources = ChangeSourceSet::resolve(&repository).expect("sources");
        let one_sided_rename = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(sources.worktree().join("old.rs"));
        let missing_path = Event::new(EventKind::Any);

        for event in [one_sided_rename, missing_path] {
            assert!(matches!(
                normalize_raw_batch(
                    &sources,
                    ChangeProviderKind::Notify,
                    CompositeCursor::default(),
                    raw_changes_from_notify(&sources, event),
                ),
                ChangeBatch::TrustLost {
                    reason: TrustLoss::AmbiguousEvent {
                        provider: ChangeProviderKind::Notify,
                        ..
                    },
                    ..
                }
            ));
        }
    }

    #[tokio::test]
    async fn overlay_watch_notify_cursor_never_regresses_across_callback_order() {
        let (_temp, repository) = repository_fixture();
        let sources = ChangeSourceSet::resolve(&repository).expect("sources");
        let root = sources.worktree().to_path_buf();
        let cursor = Mutex::new(CompositeCursor::from_entries([(
            root.clone(),
            "0".to_owned(),
        )]));
        let (sender, mut receiver) = mpsc::unbounded_channel();

        send_notify_batch(
            &sources,
            &cursor,
            &sender,
            2,
            vec![RawChange::Modified(root.join("second.rs"))],
        );
        send_notify_batch(
            &sources,
            &cursor,
            &sender,
            1,
            vec![RawChange::Modified(root.join("first.rs"))],
        );

        let _first = receiver.recv().await.expect("first batch");
        let second = receiver.recv().await.expect("second batch");
        assert_eq!(second.cursor().entries().get(&root).unwrap(), "2");
    }

    #[tokio::test]
    async fn overlay_watch_fake_subscription_replays_every_batch_after_c0() {
        let (_temp, repository) = repository_fixture();
        let sources = ChangeSourceSet::resolve(&repository).expect("sources");
        let root = sources.worktree().to_path_buf();
        let c0 = CompositeCursor::from_entries([(root.clone(), "c0".to_owned())]);
        let c1 = CompositeCursor::from_entries([(root.clone(), "c1".to_owned())]);
        let c2 = CompositeCursor::from_entries([(root.clone(), "c2".to_owned())]);
        let first = ChangeBatch::Changes {
            cursor: c1.clone(),
            added: BTreeSet::new(),
            modified: BTreeSet::from([root.join("during-scan.rs")]),
            deleted: BTreeSet::new(),
            renamed: BTreeSet::new(),
            git_metadata: BTreeSet::new(),
        };
        let second = ChangeBatch::Changes {
            cursor: c2.clone(),
            added: BTreeSet::new(),
            modified: BTreeSet::new(),
            deleted: BTreeSet::from([root.join("after-scan.rs")]),
            renamed: BTreeSet::new(),
            git_metadata: BTreeSet::new(),
        };
        let calls = Arc::new(Mutex::new(Vec::new()));
        let watchman = FakeConnector {
            kind: ChangeProviderKind::Watchman,
            failure: None,
            calls: Arc::clone(&calls),
            initial_cursor: c0.clone(),
            batches: vec![first.clone(), second.clone()],
        };
        let notify = FakeConnector {
            kind: ChangeProviderKind::Notify,
            failure: Some("must not be selected"),
            calls,
            initial_cursor: CompositeCursor::default(),
            batches: Vec::new(),
        };
        let factory = SubscriptionFactory::with_connectors(Arc::new(watchman), Arc::new(notify));

        let mut subscription = factory.subscribe(&sources).await;
        assert_eq!(subscription.initial_cursor(), &c0);
        assert_eq!(subscription.next_batch().await, first);
        assert_eq!(subscription.next_batch().await, second);
        assert_eq!(subscription.cursor(), &c2);
        assert_eq!(
            subscription.next_batch().await,
            ChangeBatch::TrustLost {
                cursor: c2,
                reason: TrustLoss::ChannelDisconnected {
                    provider: ChangeProviderKind::Watchman,
                },
            }
        );
    }

    #[test]
    fn overlay_watch_composite_cursor_is_root_order_independent() {
        let first = CompositeCursor::from_entries([
            (PathBuf::from("/z"), "2".to_owned()),
            (PathBuf::from("/a"), "1".to_owned()),
        ]);
        let second = CompositeCursor::from_entries(BTreeMap::from([
            (PathBuf::from("/a"), "1".to_owned()),
            (PathBuf::from("/z"), "2".to_owned()),
        ]));
        assert_eq!(first, second);
    }
}
