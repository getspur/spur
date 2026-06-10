//! Guards against drift between the canonical TypeScript SDK and copies
//! vendored into app_gallery apps. If this fails, re-copy the listed files
//! from sdk/typescript/src/ into the app's sdk/ directory.

use std::path::{Path, PathBuf};

const VENDORED_MODULES: &[&str] = &["call_tool.ts", "wire.ts"];

fn repo_root() -> PathBuf {
    // crates/spur-notebook → repo root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn html_video_vendored_sdk_matches_canonical_sdk() {
    let root = repo_root();
    let canonical = root.join("sdk/typescript/src");
    let vendored = root.join("app_gallery/html_video/sdk");

    for module in VENDORED_MODULES {
        let canonical_bytes = std::fs::read(canonical.join(module))
            .unwrap_or_else(|e| panic!("read canonical {module}: {e}"));
        let vendored_bytes = std::fs::read(vendored.join(module))
            .unwrap_or_else(|e| panic!("read vendored {module}: {e}"));
        assert_eq!(
            canonical_bytes, vendored_bytes,
            "app_gallery/html_video/sdk/{module} has drifted from sdk/typescript/src/{module}; \
             re-copy the canonical file"
        );
    }
}
