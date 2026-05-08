//! Startup theme resolution: cascades `.spur/themes/<name>.yaml` (project)
//! → `~/.spur/themes/<name>.yaml` (user) → built-in. All failures degrade
//! to the `dark` built-in with a stderr log so the TUI never panics on
//! a malformed user theme.

use std::path::{Path, PathBuf};

use super::loader::{load_built_in, load_theme_from_str, RawTheme, Theme, ThemeError};

/// Outcome of a theme resolution. Carried back to the caller so the App
/// can render diagnostics or re-resolve when `/theme reload` lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeLoadOutcome {
    /// Theme loaded from a project-local `.spur/themes/<name>.yaml`.
    Project(PathBuf),
    /// Theme loaded from a user `~/.spur/themes/<name>.yaml`.
    User(PathBuf),
    /// Theme came from the embedded built-in set (`dark`/`light`/`high-contrast`).
    BuiltIn,
    /// All resolution paths failed; the `dark` built-in is the safety net.
    FellBackToDark { reason: String },
}

/// Resolve `name` against the cascade and return both the materialized
/// `Theme` and the `ThemeLoadOutcome` that produced it.
///
/// On any failure the `dark` built-in is returned and the failure is
/// emitted to stderr. The returned `Theme` is never an error variant —
/// the TUI's startup must always have a usable theme.
pub fn load_runtime_theme(name: &str) -> (Theme, ThemeLoadOutcome) {
    // Reject path-traversal payloads before they hit the filesystem. A
    // name like `../../../../etc/passwd` would otherwise escape the
    // themes directory; we fall through to built-in lookup which only
    // matches the embedded `dark` / `light` / `high-contrast` set.
    let safe_name = is_safe_theme_name(name);
    let project_path = if safe_name {
        std::env::current_dir().ok().map(|cwd| {
            cwd.join(".spur")
                .join("themes")
                .join(format!("{name}.yaml"))
        })
    } else {
        None
    };
    let user_path = if safe_name {
        home_dir().map(|home| {
            home.join(".spur")
                .join("themes")
                .join(format!("{name}.yaml"))
        })
    } else {
        None
    };

    let project_exists = project_path
        .as_deref()
        .map(|p| p.is_file())
        .unwrap_or(false);
    let user_exists = user_path.as_deref().map(|p| p.is_file()).unwrap_or(false);

    if project_exists && user_exists {
        eprintln!(
            "spur-tui theme: project theme `{}` shadows user theme `{}`",
            project_path.as_deref().unwrap().display(),
            user_path.as_deref().unwrap().display(),
        );
    }

    if let Some(path) = project_path.as_deref() {
        if path.is_file() {
            match load_from_file(path) {
                Ok(theme) => return (theme, ThemeLoadOutcome::Project(path.to_path_buf())),
                Err(err) => eprintln!(
                    "spur-tui theme: failed to load project theme `{}`: {err}; falling through",
                    path.display()
                ),
            }
        }
    }

    if let Some(path) = user_path.as_deref() {
        if path.is_file() {
            match load_from_file(path) {
                Ok(theme) => return (theme, ThemeLoadOutcome::User(path.to_path_buf())),
                Err(err) => eprintln!(
                    "spur-tui theme: failed to load user theme `{}`: {err}; falling through",
                    path.display()
                ),
            }
        }
    }

    match load_built_in(name) {
        Ok(theme) => (theme, ThemeLoadOutcome::BuiltIn),
        Err(err) => {
            let reason = format!("built-in `{name}` failed: {err}");
            eprintln!("spur-tui theme: {reason}; falling back to `dark`");
            let dark = load_built_in("dark").expect("dark built-in must load");
            (dark, ThemeLoadOutcome::FellBackToDark { reason })
        }
    }
}

/// Built-in theme names embedded in the binary, in the canonical
/// listing order used by `/theme` (no args).
pub const BUILT_IN_THEME_NAMES: &[&str] = &["dark", "light", "high-contrast"];

/// Discovered set of theme names available to `/theme <name>`.
/// `built_in` is fixed; `project` and `user` are scanned from
/// `.spur/themes/*.yaml` and `~/.spur/themes/*.yaml` respectively.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AvailableThemes {
    pub built_in: Vec<String>,
    pub project: Vec<String>,
    pub user: Vec<String>,
}

/// Scan the cascade roots for available custom theme files. Built-ins
/// are always returned. Filesystem errors are silently ignored — this
/// is a discovery helper, not a load path.
pub fn list_available_themes() -> AvailableThemes {
    let built_in = BUILT_IN_THEME_NAMES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let project = std::env::current_dir()
        .ok()
        .map(|cwd| scan_theme_dir(&cwd.join(".spur").join("themes")))
        .unwrap_or_default();
    let user = home_dir()
        .map(|home| scan_theme_dir(&home.join(".spur").join("themes")))
        .unwrap_or_default();
    AvailableThemes {
        built_in,
        project,
        user,
    }
}

fn scan_theme_dir(dir: &Path) -> Vec<String> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read_dir
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                return None;
            }
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

fn load_from_file(path: &Path) -> Result<Theme, ThemeError> {
    let yaml = std::fs::read_to_string(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    load_theme_from_str(&yaml, parent_resolver)
}

/// Resolves `extends:` for user/project themes: built-in names are
/// embedded YAML, custom parents are not chained (single-level rule).
fn parent_resolver(name: &str) -> Option<RawTheme> {
    let yaml = match name {
        "dark" => include_str!("../../themes/dark.yaml"),
        "light" => include_str!("../../themes/light.yaml"),
        "high-contrast" => include_str!("../../themes/high-contrast.yaml"),
        _ => return None,
    };
    serde_yml::from_str(yaml).ok()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Reject theme names that could escape the themes directory or smuggle
/// shell metacharacters into a filesystem lookup. Built-ins
/// (`dark`/`light`/`high-contrast`) and unadorned identifiers pass.
fn is_safe_theme_name(name: &str) -> bool {
    // `~` is rejected defensively even though `Path::join` does not
    // shell-expand it — keeps the contract explicit so any future
    // consumer that does expand (e.g. shellexpand crate) stays safe.
    !name.is_empty()
        && !name.starts_with('~')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains('\0')
}

/// Test-only helpers shared with `app::theme_threading_tests` so cwd/HOME
/// mutation stays serialized and panic-safe across both modules.
#[cfg(test)]
pub(crate) mod test_support {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tempfile::TempDir;

    pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Restores cwd + HOME on drop. Runs even if the test closure panics,
    /// so a single failing test cannot corrupt env for subsequent tests.
    struct EnvGuard {
        prev_cwd: Option<PathBuf>,
        prev_home: Option<OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(p) = self.prev_cwd.take() {
                let _ = std::env::set_current_dir(p);
            }
            match self.prev_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// Run `f` with cwd + HOME each pointing at a fresh tempdir. Both are
    /// restored on return AND on panic. Serialized via `TEST_LOCK` because
    /// `set_current_dir` and `HOME` are process-global.
    pub(crate) fn with_isolated_dirs<F: FnOnce(&Path, &Path)>(f: F) {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cwd = TempDir::new().expect("project tempdir");
        let home = TempDir::new().expect("home tempdir");
        let _guard = EnvGuard {
            prev_cwd: std::env::current_dir().ok(),
            prev_home: std::env::var_os("HOME"),
        };
        std::env::set_current_dir(cwd.path()).expect("set cwd");
        std::env::set_var("HOME", home.path());
        f(cwd.path(), home.path());
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::with_isolated_dirs;
    use super::*;
    use std::fs;

    #[test]
    fn falls_back_to_dark_when_built_in_unknown() {
        with_isolated_dirs(|_, _| {
            let (theme, outcome) = load_runtime_theme("definitely-not-a-theme");
            assert_eq!(theme.name, "dark");
            assert!(matches!(outcome, ThemeLoadOutcome::FellBackToDark { .. }));
        });
    }

    #[test]
    fn loads_built_in_light() {
        with_isolated_dirs(|_, _| {
            let (theme, outcome) = load_runtime_theme("light");
            assert_eq!(theme.name, "light");
            assert!(matches!(outcome, ThemeLoadOutcome::BuiltIn));
        });
    }

    #[test]
    fn project_file_shadows_user_file_with_warning() {
        with_isolated_dirs(|cwd, home| {
            let project_dir = cwd.join(".spur").join("themes");
            fs::create_dir_all(&project_dir).unwrap();
            fs::write(
                project_dir.join("dark.yaml"),
                "version: 1\nname: project-dark\n",
            )
            .unwrap();
            let user_dir = home.join(".spur").join("themes");
            fs::create_dir_all(&user_dir).unwrap();
            fs::write(user_dir.join("dark.yaml"), "version: 1\nname: user-dark\n").unwrap();

            let (theme, outcome) = load_runtime_theme("dark");
            assert_eq!(theme.name, "project-dark");
            assert!(matches!(outcome, ThemeLoadOutcome::Project(_)));
        });
    }

    #[test]
    fn list_available_themes_picks_up_project_yamls() {
        with_isolated_dirs(|cwd, _| {
            let project_dir = cwd.join(".spur").join("themes");
            fs::create_dir_all(&project_dir).unwrap();
            fs::write(project_dir.join("solarized.yaml"), "version: 1\n").unwrap();
            fs::write(project_dir.join("nord.yaml"), "version: 1\n").unwrap();

            let available = list_available_themes();
            assert_eq!(available.project, vec!["nord", "solarized"]);
            assert!(available.user.is_empty());
            assert_eq!(
                available.built_in,
                BUILT_IN_THEME_NAMES
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn list_available_themes_picks_up_user_yamls() {
        with_isolated_dirs(|_, home| {
            let user_dir = home.join(".spur").join("themes");
            fs::create_dir_all(&user_dir).unwrap();
            fs::write(user_dir.join("dracula.yaml"), "version: 1\n").unwrap();

            let available = list_available_themes();
            assert!(available.project.is_empty());
            assert_eq!(available.user, vec!["dracula"]);
        });
    }

    #[test]
    fn list_available_themes_returns_empty_lists_when_dirs_missing() {
        with_isolated_dirs(|_, _| {
            // Neither `.spur/themes/` nor `~/.spur/themes/` exists.
            let available = list_available_themes();
            assert!(available.project.is_empty());
            assert!(available.user.is_empty());
            // Built-ins are always present regardless of filesystem state.
            assert_eq!(available.built_in.len(), BUILT_IN_THEME_NAMES.len());
        });
    }

    #[test]
    fn list_available_themes_filters_out_non_yaml_files() {
        with_isolated_dirs(|cwd, _| {
            let project_dir = cwd.join(".spur").join("themes");
            fs::create_dir_all(&project_dir).unwrap();
            fs::write(project_dir.join("real.yaml"), "version: 1\n").unwrap();
            fs::write(project_dir.join("readme.md"), "ignored\n").unwrap();
            fs::write(project_dir.join("backup.yml"), "version: 1\n").unwrap();
            fs::write(project_dir.join("noext"), "version: 1\n").unwrap();
            fs::create_dir_all(project_dir.join("subdir")).unwrap();

            let available = list_available_themes();
            assert_eq!(available.project, vec!["real"]);
        });
    }

    #[test]
    fn rejects_path_traversal_theme_name() {
        with_isolated_dirs(|_, _| {
            let (theme, outcome) = load_runtime_theme("../../../../etc/passwd");
            assert_eq!(theme.name, "dark");
            assert!(matches!(outcome, ThemeLoadOutcome::FellBackToDark { .. }));
        });
    }

    /// Unit-test the predicate directly so each forbidden pattern is
    /// exercised in isolation. The integration-style `rejects_path_
    /// traversal_theme_name` test only covers a combined `/` + `..`
    /// payload; this guards each rejection branch separately.
    #[test]
    fn is_safe_theme_name_rejects_each_dangerous_pattern() {
        // Allowed names.
        assert!(is_safe_theme_name("dark"));
        assert!(is_safe_theme_name("my-custom"));
        assert!(is_safe_theme_name("my.theme"));
        // Rejected: empty.
        assert!(!is_safe_theme_name(""));
        // Rejected: tilde prefix (defensive).
        assert!(!is_safe_theme_name("~user"));
        assert!(!is_safe_theme_name("~"));
        // Rejected: forward slash.
        assert!(!is_safe_theme_name("foo/bar"));
        assert!(!is_safe_theme_name("/etc/passwd"));
        // Rejected: backslash.
        assert!(!is_safe_theme_name("foo\\bar"));
        assert!(!is_safe_theme_name("C:\\Windows"));
        // Rejected: parent traversal.
        assert!(!is_safe_theme_name(".."));
        assert!(!is_safe_theme_name("foo..bar"));
        // Rejected: null byte.
        assert!(!is_safe_theme_name("foo\0bar"));
    }

    #[test]
    fn invalid_user_theme_falls_through_to_built_in() {
        with_isolated_dirs(|_, home| {
            let user_dir = home.join(".spur").join("themes");
            fs::create_dir_all(&user_dir).unwrap();
            fs::write(user_dir.join("dark.yaml"), "version: 99\n").unwrap();

            let (theme, outcome) = load_runtime_theme("dark");
            assert_eq!(theme.name, "dark");
            assert!(matches!(outcome, ThemeLoadOutcome::BuiltIn));
        });
    }
}
