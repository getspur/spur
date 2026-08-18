//! Build guard and embedded skill manifest generator for spur-cli.
//!
//! Ensures the distributable runtime binary is never built with the
//! policy signing key present in the environment. The signing key
//! must only exist on the admin machine, never in CI.

use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    assert!(
        std::env::var_os("SPUR_POLICY_SIGNING_KEY").is_none(),
        "SPUR_POLICY_SIGNING_KEY must not be present in the build environment. \
         The distributable runtime binary must not transit signing credentials. \
         If you are building spur-license-admin, use `cargo build -p spur-license-admin` instead."
    );

    generate_embedded_skills();
}

fn generate_embedded_skills() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"),
    );
    let assets_root = manifest_dir.join("assets/skills");
    let mut files = Vec::new();
    collect_files(&assets_root, &assets_root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    let mut generated = String::from("static EMBEDDED_SKILL_FILES: &[EmbeddedSkillFile] = &[\n");
    for (relative, absolute, mode) in &files {
        let bytes = std::fs::read(absolute)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", absolute.display()));
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(mode.to_le_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
        writeln!(
            generated,
            "    EmbeddedSkillFile {{ path: {relative:?}, bytes: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/assets/skills/\", {relative:?})), mode: 0o{mode:o} }},"
        )
        .expect("write generated embedded skill entry");
        println!("cargo:rerun-if-changed={}", absolute.display());
    }
    generated.push_str("];\n");
    writeln!(
        generated,
        "const EMBEDDED_SKILL_DIGEST: &str = \"{:x}\";",
        hasher.finalize()
    )
    .expect("write generated embedded skill digest");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR must be set"));
    std::fs::write(out_dir.join("embedded_skills.rs"), generated)
        .expect("write generated embedded skill manifest");
    println!("cargo:rerun-if-changed={}", assets_root.display());
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf, u32)>) {
    let mut entries = std::fs::read_dir(directory)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|err| panic!("failed to enumerate {}: {err}", directory.display()));
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let file_type = entry
            .file_type()
            .unwrap_or_else(|err| panic!("failed to inspect {}: {err}", entry.path().display()));
        let path = entry.path();
        assert!(
            !file_type.is_symlink(),
            "bundled skill assets must not contain symlinks: {}",
            path.display()
        );
        if file_type.is_dir() {
            collect_files(root, &path, files);
            continue;
        }
        assert!(
            file_type.is_file(),
            "bundled skill assets must contain only files and directories: {}",
            path.display()
        );

        let relative = path
            .strip_prefix(root)
            .expect("embedded skill path must be below its root")
            .components()
            .map(|component| {
                component.as_os_str().to_str().unwrap_or_else(|| {
                    panic!("bundled skill asset path must be UTF-8: {}", path.display())
                })
            })
            .collect::<Vec<_>>()
            .join("/");
        files.push((relative, path.clone(), source_mode(&path)));
    }
}

#[cfg(unix)]
fn source_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    let checkout_mode = std::fs::metadata(path)
        .unwrap_or_else(|err| {
            panic!(
                "failed to inspect permissions for {}: {err}",
                path.display()
            )
        })
        .permissions()
        .mode();
    canonical_source_mode(checkout_mode)
}

#[cfg(not(unix))]
fn source_mode(path: &Path) -> u32 {
    let executable = matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("sh")
    ) || path.file_name().and_then(|name| name.to_str())
        == Some("render-graphs.js");
    if executable {
        0o755
    } else {
        0o644
    }
}

#[cfg(unix)]
fn canonical_source_mode(checkout_mode: u32) -> u32 {
    if checkout_mode & 0o111 == 0 {
        0o644
    } else {
        0o755
    }
}
