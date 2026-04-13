#![cfg(feature = "markdown")]

use spur_tui::components::mermaid::{render_mermaid, RenderError};

#[test]
fn renders_valid_flowchart_to_nonzero_image() {
    let code = "flowchart LR\n    A[Start] --> B[End]\n";
    let img = render_mermaid(code).expect("valid flowchart should render");
    assert!(img.width() > 0, "rendered image has zero width");
    assert!(img.height() > 0, "rendered image has zero height");
}

// NOTE: mermaid-rs-renderer 0.2.x does NOT error on arbitrary text — it
// renders a fallback diagram for any input, including nonsense strings.  The
// original plan assumed the renderer would return Err/panic for malformed
// input, but the actual library is permissive.  This test therefore verifies
// that `render_mermaid` handles such input *without panicking* (returning
// either Ok with a fallback image or a RenderError — both are acceptable).
#[test]
fn malformed_source_does_not_panic() {
    let code = "completely not mermaid";
    let result = render_mermaid(code);
    // The renderer currently returns Ok for any input, but if a future
    // version errors we also accept RenderError variants.
    match result {
        Ok(img) => {
            // Got a fallback image — dimensions may be small but non-zero.
            assert!(img.width() > 0, "fallback image width should be > 0");
            assert!(img.height() > 0, "fallback image height should be > 0");
        }
        Err(RenderError::Render(_)) | Err(RenderError::Panic(_)) => {
            // Acceptable: renderer signalled an error.
        }
        Err(e) => panic!("unexpected error variant: {e:?}"),
    }
}

// NOTE: mermaid-rs-renderer 0.2.x renders empty input as an 8×8 blank SVG
// rather than panicking or returning an error.  This test verifies that
// `render_mermaid` is panic-safe: calling it with an empty string must
// not unwind the thread regardless of what the underlying renderer does.
#[test]
fn empty_input_does_not_panic() {
    // Must not panic — outcome (Ok or Err) is both acceptable.
    let result = render_mermaid("");
    match result {
        Ok(img) => {
            // Got a tiny blank canvas — just ensure dimensions are non-zero.
            assert!(img.width() > 0, "blank image width should be > 0");
            assert!(img.height() > 0, "blank image height should be > 0");
        }
        Err(_) => {
            // Any error variant is also fine — the point is no panic.
        }
    }
}
