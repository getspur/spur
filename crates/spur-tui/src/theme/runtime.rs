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
    let project_path = std::env::current_dir().ok().map(|cwd| {
        cwd.join(".spur")
            .join("themes")
            .join(format!("{name}.yaml"))
    });
    let user_path = home_dir().map(|home| {
        home.join(".spur")
            .join("themes")
            .join(format!("{name}.yaml"))
    });

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
