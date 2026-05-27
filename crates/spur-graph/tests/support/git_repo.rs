use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

pub struct GitRepo {
    #[allow(dead_code)]
    pub temp: TempDir,
    root: PathBuf,
}

impl GitRepo {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("create repo dir");
        run_git(&root, &["init", "-q", "-b", "main"]);
        run_git(
            &root,
            &["config", "user.email", "spur-graph@example.invalid"],
        );
        run_git(&root, &["config", "user.name", "Spur Graph Test"]);
        Self { temp, root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir parent");
        }
        fs::write(path, content).expect("write file");
    }

    #[allow(dead_code)]
    pub fn remove(&self, relative: &str) {
        fs::remove_file(self.root.join(relative)).expect("remove file");
    }

    pub fn git(&self, args: &[&str]) {
        run_git(&self.root, args);
    }

    #[allow(dead_code)]
    pub fn git_with_env(&self, args: &[&str], envs: &[(&str, String)]) {
        run_git_with_env(&self.root, args, envs);
    }

    pub fn head(&self) -> String {
        git_stdout(&self.root, &["rev-parse", "HEAD"])
            .trim_end()
            .to_owned()
    }

    #[allow(dead_code)]
    pub fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.head()
    }

    #[allow(dead_code)]
    pub fn commit_all_at(&self, message: &str, unix_time: i64) -> String {
        self.git(&["add", "-A"]);
        let timestamp = format!("@{unix_time} +0000");
        self.git_with_env(
            &["commit", "-q", "--allow-empty", "-m", message],
            &[
                ("GIT_AUTHOR_DATE", timestamp.clone()),
                ("GIT_COMMITTER_DATE", timestamp),
            ],
        );
        self.head()
    }
}

#[allow(dead_code)]
pub fn path_str(path: &Path) -> &str {
    path.to_str().expect("path UTF-8")
}

fn run_git(root: &Path, args: &[&str]) {
    run_git_with_env(root, args, &[]);
}

fn run_git_with_env(root: &Path, args: &[&str], envs: &[(&str, String)]) {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    let output = command
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout UTF-8")
}
