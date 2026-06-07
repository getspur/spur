use std::{
    cmp::Ordering,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};

const ENV_VAR: &str = "SPUR_HTML_VIDEO_LIBRARY";
const LIB_DIR: &str = "html-video-library";
const INDEX_FILE: &str = "index.json";
const DEFAULT_HTML_FILE_NAMES: [&str; 4] =
    ["template.html", "frame.html", "index.html", "index.htm"];
const DEFAULT_SKILL_FILE_NAMES: [&str; 2] = ["SKILL.md", "skill.md"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSearchResult {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMetadata {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub metadata: TemplateMetadata,
    pub html: String,
    pub skill_md: String,
}

#[derive(Debug)]
pub enum LibraryError {
    RootNotFound,
    NotFound { id: String },
    InvalidIndex { path: PathBuf, reason: String },
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootNotFound => write!(f, "html video library root not found"),
            Self::NotFound { id } => write!(f, "html video template not found: {id}"),
            Self::InvalidIndex { path, reason } => {
                write!(f, "invalid index in {}: {reason}", path.display())
            }
            Self::Io(error) => write!(f, "html video library I/O error: {error}"),
            Self::Json(error) => write!(f, "html video library JSON error: {error}"),
        }
    }
}

impl Error for LibraryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidIndex { .. } | Self::RootNotFound | Self::NotFound { .. } => None,
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

#[derive(Debug, Deserialize)]
struct LibraryIndex {
    #[serde(default)]
    templates: Vec<IndexTemplate>,
    #[serde(default)]
    items: Vec<IndexTemplate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexTemplate {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub fn resolve_root(resource_dir: Option<&Path>) -> Option<PathBuf> {
    let home_dir = user_home_dir();
    let repo_assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    resolve_root_with(
        resource_dir,
        home_dir.as_deref(),
        Some(repo_assets.as_path()),
    )
}

fn resolve_root_with(
    resource_dir: Option<&Path>,
    home_dir: Option<&Path>,
    repo_assets_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(base) = std::env::var_os(ENV_VAR) {
        let candidate = PathBuf::from(&base);
        let legacy_candidate = candidate.join(LIB_DIR);
        if is_library_root(&candidate) {
            return Some(candidate);
        }
        if is_library_root(&legacy_candidate) {
            return Some(legacy_candidate);
        }
    }

    if let Some(home_dir) = home_dir {
        let candidate = home_dir.join(".spur").join(LIB_DIR);
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(resource_dir) = resource_dir {
        let candidate = resource_dir.join(LIB_DIR);
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(repo_assets_dir) = repo_assets_dir {
        let candidate = repo_assets_dir.join(LIB_DIR);
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn is_library_root(path: &Path) -> bool {
    path.join(INDEX_FILE).is_file()
}

fn user_home_dir() -> Option<PathBuf> {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

pub fn search(
    intent: &str,
    top: usize,
    resource_dir: Option<&Path>,
) -> Result<Vec<TemplateSearchResult>, LibraryError> {
    let root = resolve_root(resource_dir).ok_or(LibraryError::RootNotFound)?;
    let items = load_index(&root)?;
    let tokens = tokenize(intent);
    let query_is_empty = tokens.is_empty();
    let mut results: Vec<_> = items
        .into_iter()
        .filter_map(|item| {
            let score = score(item.intent.as_str(), &tokens, &item);
            if !query_is_empty && score == 0.0 {
                None
            } else {
                Some(TemplateSearchResult {
                    id: item.id,
                    title: item.title,
                    intent: item.intent,
                    summary: item.summary,
                    tags: item.tags,
                    score,
                })
            }
        })
        .collect();

    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.title.cmp(&right.title))
    });
    if top > 0 {
        results.truncate(top);
    }
    Ok(results)
}

pub fn get_template(id: &str, resource_dir: Option<&Path>) -> Result<Template, LibraryError> {
    let root = resolve_root(resource_dir).ok_or(LibraryError::RootNotFound)?;
    let items = load_index(&root)?;
    let item = items
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| LibraryError::NotFound { id: id.to_string() })?;

    let template_dir = resolve_template_dir(&root, &item.id)?;
    let html = read_template_html(&template_dir)?;
    let skill_md = read_template_skill_md(&template_dir)?;

    Ok(Template {
        metadata: TemplateMetadata {
            id: item.id,
            title: item.title,
            intent: item.intent,
            summary: item.summary,
            tags: item.tags,
        },
        html,
        skill_md,
    })
}

fn resolve_template_dir(root: &Path, id: &str) -> Result<PathBuf, LibraryError> {
    let package_dir = root.join("templates").join(id);
    if package_dir.is_dir() {
        return Ok(package_dir);
    }

    let direct_dir = root.join(id);
    if direct_dir.is_dir() {
        Ok(direct_dir)
    } else {
        Err(LibraryError::NotFound { id: id.to_string() })
    }
}

fn load_index(root: &Path) -> Result<Vec<IndexTemplate>, LibraryError> {
    let index_path = root.join(INDEX_FILE);
    let raw = fs::read_to_string(&index_path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            LibraryError::InvalidIndex {
                path: index_path.clone(),
                reason: "missing index".to_string(),
            }
        } else {
            LibraryError::Io(error)
        }
    })?;

    let array_result: Result<Vec<IndexTemplate>, serde_json::Error> = serde_json::from_str(&raw);
    if let Ok(items) = array_result {
        return Ok(items);
    }

    let file: LibraryIndex = serde_json::from_str(&raw)?;
    let items = if !file.templates.is_empty() {
        file.templates
    } else if !file.items.is_empty() {
        file.items
    } else {
        Vec::new()
    };
    Ok(items)
}

fn read_template_html(template_dir: &Path) -> Result<String, LibraryError> {
    for name in DEFAULT_HTML_FILE_NAMES {
        let candidate = template_dir.join(name);
        if candidate.is_file() {
            return fs::read_to_string(candidate).map_err(Into::into);
        }
    }
    Err(LibraryError::NotFound {
        id: template_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
    })
}

fn read_template_skill_md(template_dir: &Path) -> Result<String, LibraryError> {
    for name in DEFAULT_SKILL_FILE_NAMES {
        let candidate = template_dir.join(name);
        if candidate.is_file() {
            return fs::read_to_string(candidate).map_err(Into::into);
        }
    }
    Err(LibraryError::NotFound {
        id: template_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
    })
}

pub fn default_render_duration(frame_count: usize, fps: u32, duration: Option<f64>) -> f64 {
    let fps = u32::max(fps, 1);
    duration
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or_else(|| {
            if frame_count <= 1 {
                1.0
            } else {
                frame_count as f64 / fps as f64
            }
        })
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn score(intent: &str, tokens: &[String], item: &IndexTemplate) -> f64 {
    if tokens.is_empty() {
        return 1.0;
    }

    let mut score = 0.0;
    let title = item.title.to_lowercase();
    let intent_index = intent.to_lowercase();
    let summary = item.summary.as_deref().unwrap_or_default().to_lowercase();
    let tags = item.tags.join(" ").to_lowercase();

    for token in tokens {
        if item.id.to_lowercase().contains(token) {
            score += 10.0;
        }
        if title.contains(token) {
            score += 6.0;
        }
        if intent_index.contains(token) {
            score += 4.0;
        }
        if !summary.is_empty() && summary.contains(token) {
            score += 1.5;
        }
        if tags.contains(token) {
            score += 0.5;
        }
    }

    score
}
