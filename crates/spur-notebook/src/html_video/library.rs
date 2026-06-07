use std::{
    cmp::Ordering,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

const ENV_VAR: &str = "HTML_VIDEO_TEMPLATES_DIR";
const LIBRARY_DIR: &str = "html-video-library";
const TEMPLATE_METADATA_FILENAME: &str = "template.html-video.yaml";
const DEFAULT_SOURCE_ENTRY: &str = "template.html";
const DEFAULT_ENGINE: &str = "html";

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct TemplateMetadata {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub best_for: Option<String>,
    #[serde(default = "default_engine")]
    pub engine: String,
    #[serde(default = "default_source_entry")]
    pub source_entry: String,
    #[serde(default)]
    pub inputs_schema: Option<String>,
    #[serde(default)]
    pub min_duration: Option<u32>,
    #[serde(default)]
    pub max_duration: Option<u32>,
    #[serde(default)]
    pub resolutions: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct Template {
    pub metadata: TemplateMetadata,
    pub source_html: String,
    pub skill_md: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct Ranked {
    pub metadata: TemplateMetadata,
    pub score: f64,
}

#[derive(Debug)]
pub enum LibraryError {
    RootNotFound,
    NotFound { id: String },
    Io(io::Error),
    Yaml(serde_yaml::Error),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootNotFound => write!(f, "html-video template library root not found"),
            Self::NotFound { id } => write!(f, "template not found: {id}"),
            Self::Io(error) => write!(f, "html-video template library I/O error: {error}"),
            Self::Yaml(error) => write!(f, "html-video template yaml parse error: {error}"),
        }
    }
}

impl Error for LibraryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Yaml(error) => Some(error),
            Self::RootNotFound | Self::NotFound { .. } => None,
        }
    }
}

impl From<io::Error> for LibraryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_yaml::Error> for LibraryError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::Yaml(error)
    }
}

pub fn resolve_root() -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bundled_dir = manifest_dir.join("assets");
    resolve_root_with(Some(&bundled_dir), Some(manifest_dir))
}

fn resolve_root_with(resource_dir: Option<&Path>, repo_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(base) = std::env::var_os(ENV_VAR) {
        let candidate = PathBuf::from(base);
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(resource_dir) = resource_dir {
        let candidate = resource_dir.join(LIBRARY_DIR);
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(repo_path) = repo_path {
        let candidate = repo_path.join(LIBRARY_DIR);
        if is_library_root(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn is_library_root(path: &Path) -> bool {
    path.join("templates").is_dir()
}

pub fn scan_templates(root: &Path) -> Result<Vec<TemplateMetadata>, LibraryError> {
    let templates_root = root.join("templates");
    if !templates_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut templates = Vec::new();

    for entry in fs::read_dir(templates_root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let metadata_path = path.join(TEMPLATE_METADATA_FILENAME);
        if !metadata_path.is_file() {
            continue;
        }

        let template_id = entry.file_name().to_string_lossy().to_string();
        let mut metadata = load_template_metadata(&metadata_path, &template_id)?;
        if metadata.id.is_empty() {
            metadata.id = template_id;
        }
        templates.push(metadata);
    }

    templates.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.category.cmp(&b.category)));
    Ok(templates)
}

pub fn get_template(id: &str) -> Result<Template, LibraryError> {
    let root = resolve_root().ok_or(LibraryError::RootNotFound)?;
    get_template_with_root(&root, id)
}

pub fn search(intent: &str, top: usize) -> Result<Vec<Ranked>, LibraryError> {
    let root = resolve_root().ok_or(LibraryError::RootNotFound)?;
    search_with_root(&root, intent, top)
}

fn get_template_with_root(root: &Path, id: &str) -> Result<Template, LibraryError> {
    let template_dir = root.join("templates").join(id);
    let metadata_path = template_dir.join(TEMPLATE_METADATA_FILENAME);
    let mut metadata = load_template_metadata(&metadata_path, id)?;
    let source_path = template_dir.join(&metadata.source_entry);
    let source_html = read_required_string(&source_path, id)?;
    let skill_md = read_required_string(&template_dir.join("SKILL.md"), id)?;

    if metadata.id.is_empty() {
        metadata.id = id.to_string();
    }

    Ok(Template {
        metadata,
        source_html,
        skill_md,
    })
}

fn search_with_root(root: &Path, intent: &str, top: usize) -> Result<Vec<Ranked>, LibraryError> {
    let templates = scan_templates(root)?;
    let tokens = tokenize(intent);
    let query_is_empty = tokens.is_empty();

    let mut results = Vec::new();
    for metadata in templates {
        let score = score_template(intent, &metadata);
        if !query_is_empty && score == 0.0 {
            continue;
        }
        results.push(Ranked { metadata, score });
    }

    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.metadata.id.cmp(&right.metadata.id))
    });
    results.truncate(top);
    Ok(results)
}

fn load_template_metadata(path: &Path, id: &str) -> Result<TemplateMetadata, LibraryError> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(LibraryError::NotFound { id: id.to_string() });
        }
        Err(error) => {
            return Err(LibraryError::Io(error));
        }
    };
    let mut metadata: TemplateMetadata = serde_yaml::from_str(&data)?;
    metadata.id = metadata.id.trim().to_string();
    metadata.source_entry = metadata.source_entry.trim().to_string();
    metadata.engine = metadata.engine.trim().to_string();
    if metadata.source_entry.is_empty() {
        metadata.source_entry = DEFAULT_SOURCE_ENTRY.to_string();
    }
    if metadata.engine.is_empty() {
        metadata.engine = DEFAULT_ENGINE.to_string();
    }
    Ok(metadata)
}

fn read_required_string(path: &Path, id: &str) -> Result<String, LibraryError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(LibraryError::NotFound { id: id.to_string() })
        }
        Err(error) => Err(LibraryError::Io(error)),
    }
}

fn score_template(intent: &str, item: &TemplateMetadata) -> f64 {
    let query_tokens = tokenize(intent);
    let category_tokens = tokenize(item.category.as_deref().unwrap_or_default());
    let best_for_tokens = tokenize(item.best_for.as_deref().unwrap_or_default());
    let tag_tokens = item
        .tags
        .iter()
        .flat_map(|tag| tokenize(tag))
        .collect::<Vec<_>>();
    let mut score = 0.0;

    for token in query_tokens {
        if category_tokens.iter().any(|field| field == &token) {
            score += 3.0;
        }
        if tag_tokens.iter().any(|tag| tag == &token) {
            score += 2.0;
        }
        if best_for_tokens.iter().any(|field| field == &token) {
            score += 1.0;
        }
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

fn default_source_entry() -> String {
    DEFAULT_SOURCE_ENTRY.to_string()
}

fn default_engine() -> String {
    DEFAULT_ENGINE.to_string()
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
    const ENV_VAR: &str = "HTML_VIDEO_TEMPLATES_DIR";

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

    fn write_library_root(root: &Path) -> io::Result<PathBuf> {
        let library_root = root.to_path_buf();
        fs::create_dir_all(library_root.join("templates"))?;
        Ok(library_root)
    }

    fn write_template(
        root: &Path,
        id: &str,
        source_entry: &str,
        metadata: &str,
        skill_md: &str,
        html: &str,
    ) -> io::Result<PathBuf> {
        let template_dir = root.join("templates").join(id);
        fs::create_dir_all(&template_dir)?;
        fs::write(template_dir.join(TEMPLATE_METADATA_FILENAME), metadata)?;
        fs::write(template_dir.join(source_entry), html)?;
        fs::write(template_dir.join("SKILL.md"), skill_md)?;
        Ok(template_dir)
    }

    fn write_library_with_templates(root: &Path) -> io::Result<PathBuf> {
        write_library_root(root)
    }

    #[test]
    fn resolve_root_prefers_env_override_then_resource_then_fallback() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        let resource = temp.path().join("resource");
        let fallback = temp.path().join("fallback");
        let env_root = temp.path().join("env");

        let resource_lib = write_library_root(&resource.join(LIBRARY_DIR))?;
        let fallback_lib = write_library_root(&fallback.join(LIBRARY_DIR))?;
        let env_lib = write_library_root(&env_root)?;

        assert_eq!(
            resolve_root_with(Some(&resource), Some(&fallback)),
            Some(resource_lib.clone())
        );

        env_guard.set(&env_root);
        assert_eq!(
            resolve_root_with(Some(&resource), Some(&fallback)),
            Some(env_lib.clone())
        );
        env::remove_var(ENV_VAR);
        assert_eq!(resolve_root_with(None, Some(&fallback)), Some(fallback_lib));
        Ok(())
    }

    #[test]
    fn scan_templates_reads_template_metadata() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = write_library_with_templates(temp.path())?;
        write_template(
            &root,
            "product-pitch",
            "template.html",
            r#"id: product-pitch
category: marketing
tags:
  - keynote
  - deck
best_for: investor pitch
engine: html
source_entry: template.html
inputs_schema: schema
min_duration: 30
max_duration: 120
resolutions:
  - 1080p
  - 4k
"#,
            "template skill",
            "<html><body>pitch</body></html>",
        )?;

        let mut templates = scan_templates(&root)?;
        templates.sort_by(|left, right| left.id.cmp(&right.id));

        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].id, "product-pitch");
        assert_eq!(templates[0].category.as_deref(), Some("marketing"));
        assert_eq!(templates[0].tags, vec!["keynote", "deck"]);
        assert_eq!(templates[0].best_for.as_deref(), Some("investor pitch"));
        assert_eq!(templates[0].engine, "html");
        assert_eq!(templates[0].source_entry, "template.html");
        assert_eq!(templates[0].inputs_schema.as_deref(), Some("schema"));
        assert_eq!(templates[0].min_duration, Some(30));
        assert_eq!(templates[0].max_duration, Some(120));
        assert_eq!(templates[0].resolutions, vec!["1080p", "4k"]);
        Ok(())
    }

    #[test]
    fn get_template_returns_metadata_and_source_html_and_skill() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        let root = write_library_with_templates(temp.path())?;
        write_template(
            &root,
            "story",
            "story.html",
            r#"id: story
category: social
tags:
  - short
  - promo
best_for: product launch
engine: html
source_entry: story.html
inputs_schema: schema.json
resolutions:
  - 720p
  - 1080p
"#,
            "skill",
            "<html><body>story</body></html>",
        )?;
        env_guard.set(&root);

        let template = get_template("story").expect("template exists");

        assert_eq!(template.metadata.id, "story");
        assert_eq!(template.metadata.category.as_deref(), Some("social"));
        assert_eq!(template.metadata.tags, vec!["short", "promo"]);
        assert_eq!(
            template.metadata.best_for.as_deref(),
            Some("product launch")
        );
        assert_eq!(template.metadata.engine, "html");
        assert_eq!(template.metadata.source_entry, "story.html");
        assert_eq!(
            template.metadata.inputs_schema.as_deref(),
            Some("schema.json")
        );
        assert_eq!(template.source_html, "<html><body>story</body></html>");
        assert_eq!(template.skill_md, "skill");
        Ok(())
    }

    #[test]
    fn get_template_missing_id_errors() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        let root = write_library_with_templates(temp.path())?;
        env_guard.set(&root);

        let err = get_template("missing").expect_err("missing id errors");
        assert!(matches!(err, LibraryError::NotFound { id } if id == "missing"));
        Ok(())
    }

    #[test]
    fn search_ranks_category_match_above_tags() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        let root = write_library_with_templates(temp.path())?;
        write_template(
            &root,
            "alpha",
            "template.html",
            r#"id: alpha
category: short
tags:
  - review
best_for: education
engine: html
source_entry: template.html
"#,
            "alpha skill",
            "<html>alpha</html>",
        )?;
        write_template(
            &root,
            "beta",
            "template.html",
            r#"id: beta
category: long
tags:
  - short
best_for: social video
engine: html
source_entry: template.html
"#,
            "beta skill",
            "<html>beta</html>",
        )?;
        env_guard.set(&root);

        let results = search("short", 2).expect("search succeeds");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].metadata.id, "alpha");
        assert!(results[0].score > results[1].score);
        Ok(())
    }

    #[test]
    fn search_empty_query_returns_all() -> io::Result<()> {
        let mut env_guard = EnvGuard::unset();
        let temp = tempfile::tempdir()?;
        let root = write_library_with_templates(temp.path())?;
        write_template(
            &root,
            "zeta",
            "template.html",
            r#"id: zeta
engine: html
source_entry: template.html
"#,
            "zeta skill",
            "<html>zeta</html>",
        )?;
        write_template(
            &root,
            "alpha",
            "template.html",
            r#"id: alpha
engine: html
source_entry: template.html
"#,
            "alpha skill",
            "<html>alpha</html>",
        )?;
        env_guard.set(&root);

        let results = search("", 8).expect("search succeeds");

        assert_eq!(
            results
                .iter()
                .map(|result| result.metadata.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert!(results.iter().all(|result| result.score == 0.0));
        Ok(())
    }
}
