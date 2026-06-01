use std::{
    cmp::Ordering,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use directories::BaseDirs;

const ENV_VAR: &str = "SPUR_OPEN_DESIGN_LIBRARY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    DesignSystems,
    DeckThemes,
}

impl Kind {
    pub fn lib_dir(self) -> &'static str {
        match self {
            Self::DesignSystems => "open-design-library",
            Self::DeckThemes => "open-design-deck-library",
        }
    }

    pub fn sub_dir(self) -> &'static str {
        match self {
            Self::DesignSystems => "design-systems",
            Self::DeckThemes => "deck-themes",
        }
    }

    pub fn as_str(self) -> &'static str {
        self.sub_dir()
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "design-systems" | "design_systems" => Some(Self::DesignSystems),
            "deck-themes" | "deck_themes" => Some(Self::DeckThemes),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct IndexItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub scenario: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub swatches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ranked {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub category: Option<String>,
    pub summary: Option<String>,
    pub swatches: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeckTheme {
    pub id: String,
    pub skill_md: String,
    pub example_html: Option<String>,
    pub deck_skeleton_html: Option<String>,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum LibraryError {
    RootNotFound(Kind),
    NotFound { kind: String, id: String },
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootNotFound(kind) => write!(f, "open design library root not found for {kind}"),
            Self::NotFound { kind, id } => write!(f, "{kind} item not found: {id}"),
            Self::Io(error) => write!(f, "open design library I/O error: {error}"),
            Self::Json(error) => write!(f, "open design library JSON error: {error}"),
        }
    }
}

impl Error for LibraryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::RootNotFound(_) | Self::NotFound { .. } => None,
        }
    }
}

impl From<io::Error> for LibraryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LibraryError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(serde::Deserialize)]
struct IndexFile {
    #[serde(default)]
    items: Vec<IndexItem>,
}

pub fn resolve_root(kind: Kind, resource_dir: Option<&Path>) -> Option<PathBuf> {
    let home_dir = user_home_dir();
    let repo_assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    resolve_root_with(
        kind,
        resource_dir,
        home_dir.as_deref(),
        Some(repo_assets.as_path()),
    )
}

fn resolve_root_with(
    kind: Kind,
    resource_dir: Option<&Path>,
    home_dir: Option<&Path>,
    repo_assets_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(base) = std::env::var_os(ENV_VAR) {
        let candidate = PathBuf::from(base).join(kind.lib_dir());
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(home_dir) = home_dir {
        let candidate = home_dir
            .join(".spur")
            .join("open-design")
            .join(kind.lib_dir());
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(resource_dir) = resource_dir {
        let candidate = resource_dir.join(kind.lib_dir());
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(repo_assets_dir) = repo_assets_dir {
        let candidate = repo_assets_dir.join(kind.lib_dir());
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn user_home_dir() -> Option<PathBuf> {
    BaseDirs::new()
        .map(|base_dirs| base_dirs.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

fn is_library_root(path: &Path) -> bool {
    path.join("index.json").is_file()
}

pub fn load_index(_kind: Kind, root: &Path) -> Result<Vec<IndexItem>, LibraryError> {
    let index = fs::read_to_string(root.join("index.json"))?;
    let index: IndexFile = serde_json::from_str(&index)?;
    Ok(index.items)
}

pub fn get_design_system(root: &Path, id: &str) -> Result<String, LibraryError> {
    let path = root
        .join(Kind::DesignSystems.sub_dir())
        .join(id)
        .join("DESIGN.md");
    read_required_string(&path, Kind::DesignSystems, id)
}

pub fn get_deck_theme(
    root: &Path,
    id: &str,
    include_skeleton: bool,
) -> Result<DeckTheme, LibraryError> {
    let theme_dir = root.join(Kind::DeckThemes.sub_dir()).join(id);
    let skill_md = read_required_string(&theme_dir.join("SKILL.md"), Kind::DeckThemes, id)?;
    let example_html = read_optional_string(&theme_dir.join("example.html"))?;
    let deck_skeleton_html = if include_skeleton {
        read_optional_string(&root.join("deck-skeleton.html"))?
    } else {
        None
    };

    Ok(DeckTheme {
        id: id.to_string(),
        skill_md,
        example_html,
        deck_skeleton_html,
        files: collect_side_files(&theme_dir)?,
    })
}

pub fn search(
    query: &str,
    kind: Option<Kind>,
    limit: usize,
    resource_dir: Option<&Path>,
) -> Result<Vec<Ranked>, LibraryError> {
    let tokens = tokenize(query);
    let query_is_empty = tokens.is_empty();
    let kinds = match kind {
        Some(kind) => vec![kind],
        None => vec![Kind::DesignSystems, Kind::DeckThemes],
    };
    let mut results = Vec::new();

    for kind in kinds {
        let root = resolve_root(kind, resource_dir).ok_or(LibraryError::RootNotFound(kind))?;
        for item in load_index(kind, &root)? {
            let score = score_item(&tokens, &item);
            if !query_is_empty && score == 0.0 {
                continue;
            }
            results.push(Ranked {
                id: item.id,
                kind: kind.as_str().to_string(),
                title: item.title,
                category: item.category.or(item.scenario),
                summary: item.summary,
                swatches: item.swatches,
                score,
            });
        }
    }

    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    results.truncate(limit);
    Ok(results)
}

fn read_required_string(path: &Path, kind: Kind, id: &str) -> Result<String, LibraryError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(LibraryError::NotFound {
            kind: kind.as_str().to_string(),
            id: id.to_string(),
        }),
        Err(error) => Err(LibraryError::Io(error)),
    }
}

fn read_optional_string(path: &Path) -> Result<Option<String>, LibraryError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LibraryError::Io(error)),
    }
}

fn collect_side_files(theme_dir: &Path) -> Result<Vec<FileEntry>, LibraryError> {
    let mut files = Vec::new();
    collect_side_files_from(theme_dir, theme_dir, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn collect_side_files_from(
    base: &Path,
    dir: &Path,
    files: &mut Vec<FileEntry>,
) -> Result<(), LibraryError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_side_files_from(base, &path, files)?;
        } else if file_type.is_file() {
            let relative = relative_path_string(base, &path);
            if relative == "SKILL.md" || relative == "example.html" {
                continue;
            }
            files.push(FileEntry {
                path: relative,
                bytes: entry.metadata()?.len(),
            });
        }
    }
    Ok(())
}

fn relative_path_string(base: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(base).unwrap_or(path);
    let mut normalized = String::new();
    for component in relative.components() {
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(&component.as_os_str().to_string_lossy());
    }
    normalized
}

fn score_item(query_tokens: &[String], item: &IndexItem) -> f64 {
    let id_title_tokens = tokenize(&format!("{} {}", item.id, item.title));
    let category_tokens = item
        .category
        .as_deref()
        .or(item.scenario.as_deref())
        .map(tokenize)
        .unwrap_or_default();
    let title = item.title.to_lowercase();
    let summary = item.summary.as_deref().unwrap_or_default().to_lowercase();
    let swatches = item
        .swatches
        .iter()
        .map(|swatch| swatch.to_lowercase())
        .collect::<Vec<_>>();
    let mut score = 0.0;

    for token in query_tokens {
        if id_title_tokens
            .iter()
            .any(|field_token| field_token == token)
        {
            score += 3.0;
        }
        if category_tokens
            .iter()
            .any(|field_token| field_token == token)
        {
            score += 2.0;
        }
        if title.contains(token) || summary.contains(token) {
            score += 1.0;
        }
        score += swatches
            .iter()
            .filter(|swatch| swatch.contains(token.as_str()))
            .count() as f64
            * 2.0;
    }

    score
}

fn tokenize(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        ffi::OsString,
        fs, io,
        path::{Path, PathBuf},
        sync::{Mutex, MutexGuard},
    };

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());
    const ENV_VAR: &str = "SPUR_OPEN_DESIGN_LIBRARY";

    struct EnvGuard {
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn unset() -> Self {
            let lock = ENV_LOCK.lock().expect("env lock poisoned");
            let previous = env::var_os(ENV_VAR);
            env::remove_var(ENV_VAR);
            Self {
                previous,
                _lock: lock,
            }
        }

        fn set(&mut self, value: &Path) {
            env::set_var(ENV_VAR, value);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                env::set_var(ENV_VAR, previous);
            } else {
                env::remove_var(ENV_VAR);
            }
        }
    }

    fn write_index(root: &Path, kind: Kind, items: &str) -> io::Result<PathBuf> {
        let lib_root = root.join(kind.lib_dir());
        fs::create_dir_all(&lib_root)?;
        fs::write(
            lib_root.join("index.json"),
            format!(
                r#"{{"version":1,"kind":"{}","count":1,"items":[{}]}}"#,
                kind.as_str(),
                items
            ),
        )?;
        Ok(lib_root)
    }

    #[test]
    fn resolve_root_prefers_env_override_then_resource_then_repo() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        let resource = temp.path().join("resource");
        let repo = temp.path().join("repo-assets");
        let env_root = temp.path().join("env");

        let repo_lib = write_index(
            &repo,
            Kind::DesignSystems,
            r#"{"id":"repo","title":"Repo"}"#,
        )?;
        let resource_lib = write_index(
            &resource,
            Kind::DesignSystems,
            r#"{"id":"resource","title":"Resource"}"#,
        )?;
        let env_lib = write_index(
            &env_root,
            Kind::DesignSystems,
            r#"{"id":"env","title":"Env"}"#,
        )?;

        assert_eq!(
            resolve_root_with(Kind::DesignSystems, None, Some(&home), Some(repo.as_path())),
            Some(repo_lib)
        );
        assert_eq!(
            resolve_root_with(
                Kind::DesignSystems,
                Some(&resource),
                Some(&home),
                Some(repo.as_path())
            ),
            Some(resource_lib)
        );

        env_guard.set(&env_root);
        assert_eq!(
            resolve_root_with(
                Kind::DesignSystems,
                Some(&resource),
                Some(&home),
                Some(repo.as_path())
            ),
            Some(env_lib)
        );
        Ok(())
    }

    #[test]
    fn resolve_root_returns_none_when_nothing_exists() -> io::Result<()> {
        let _env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;

        assert_eq!(
            resolve_root_with(
                Kind::DesignSystems,
                Some(&temp.path().join("resource")),
                Some(&temp.path().join("home")),
                Some(&temp.path().join("repo-assets"))
            ),
            None
        );
        Ok(())
    }

    #[test]
    fn load_index_parses_both_schemas() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let design_root = write_index(
            temp.path(),
            Kind::DesignSystems,
            r##"{"id":"agentic","title":"Agentic","category":"Themed","summary":"Delegated flows","swatches":["#ff5701"]}"##,
        )?;
        let deck_root = write_index(
            temp.path(),
            Kind::DeckThemes,
            r##"{"id":"launch","title":"Launch","scenario":"marketing","mode":"deck","featured":1,"summary":"Launch deck","source":"https://example.com","swatches":["#0a0a0b"]}"##,
        )?;

        let designs = load_index(Kind::DesignSystems, &design_root).expect("design index parses");
        let decks = load_index(Kind::DeckThemes, &deck_root).expect("deck index parses");

        assert_eq!(designs[0].category.as_deref(), Some("Themed"));
        assert_eq!(designs[0].swatches, vec!["#ff5701"]);
        assert_eq!(decks[0].scenario.as_deref(), Some("marketing"));
        assert_eq!(decks[0].swatches, vec!["#0a0a0b"]);
        Ok(())
    }

    #[test]
    fn get_deck_theme_tolerates_missing_example_html() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let theme_dir = temp.path().join(Kind::DeckThemes.sub_dir()).join("minimal");
        fs::create_dir_all(&theme_dir)?;
        fs::write(theme_dir.join("SKILL.md"), "deck skill")?;

        let theme = get_deck_theme(temp.path(), "minimal", true).expect("deck theme loads");

        assert_eq!(theme.skill_md, "deck skill");
        assert_eq!(theme.example_html, None);
        assert_eq!(theme.deck_skeleton_html, None);
        Ok(())
    }

    #[test]
    fn get_design_system_missing_id_errors() -> io::Result<()> {
        let temp = tempfile::tempdir()?;

        let error = get_design_system(temp.path(), "missing").expect_err("missing id errors");

        assert!(matches!(
            error,
            LibraryError::NotFound { kind, id }
                if kind == Kind::DesignSystems.as_str() && id == "missing"
        ));
        Ok(())
    }

    #[test]
    fn search_ranks_title_match_above_summary_only() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        write_index(
            temp.path(),
            Kind::DesignSystems,
            r#"{"id":"summary-only","title":"Plain","summary":"Aurora appears here","swatches":[]},{"id":"title-match","title":"Aurora","summary":"Plain","swatches":[]}"#,
        )?;
        env_guard.set(temp.path());

        let results =
            search("aurora", Some(Kind::DesignSystems), 8, None).expect("search succeeds");

        assert_eq!(
            results
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["title-match", "summary-only"]
        );
        assert!(results[0].score > results[1].score);
        Ok(())
    }

    #[test]
    fn search_empty_query_returns_all() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        write_index(
            temp.path(),
            Kind::DesignSystems,
            r#"{"id":"zeta","title":"Zeta","swatches":[]},{"id":"alpha","title":"Alpha","swatches":[]}"#,
        )?;
        env_guard.set(temp.path());

        let results = search("", Some(Kind::DesignSystems), 8, None).expect("search succeeds");

        assert_eq!(
            results
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert!(results.iter().all(|item| item.score == 0.0));
        Ok(())
    }
}
