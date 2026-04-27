//! Smoke-test: render a few mermaid diagrams to PNG so the output can be
//! visually inspected. Run with:
//!
//!     cargo run -p spur-tui --example mermaid_smoke --features markdown
//!
//! Writes PNGs to /tmp/mermaid-smoke/*.png and prints a summary.

#[cfg(feature = "markdown")]
fn main() {
    use image::GenericImageView;
    use spur_tui::components::mermaid::render_mermaid;
    use std::path::PathBuf;

    let out_dir = PathBuf::from("/tmp/mermaid-smoke");
    std::fs::create_dir_all(&out_dir).unwrap();

    let samples = [
        (
            "flowchart",
            "flowchart LR\n    A[Start] --> B{Decision}\n    B -- yes --> C[End]\n    B -- no --> D[Retry]\n    D --> B\n",
        ),
        (
            "sequence",
            "sequenceDiagram\n    participant A as Alice\n    participant B as Bob\n    A->>B: Hello\n    B-->>A: Hi back\n",
        ),
        (
            "classdef",
            "classDiagram\n    class Animal {\n      +name: string\n      +speak() void\n    }\n    class Dog\n    Animal <|-- Dog\n",
        ),
    ];

    for (name, src) in samples {
        print!("rendering {name}… ");
        match render_mermaid(src, 800) {
            Ok(img) => {
                let (w, h) = img.dimensions();
                let path = out_dir.join(format!("{name}.png"));
                img.save(&path).unwrap();

                // Sample some statistics: what fraction of pixels are opaque,
                // how many distinct colors, center pixel.
                let rgba = img.to_rgba8();
                let total = (w * h) as usize;
                let mut opaque = 0usize;
                let mut white_bg = 0usize;
                for p in rgba.pixels() {
                    if p.0[3] == 255 {
                        opaque += 1;
                    }
                    if p.0[0] > 240 && p.0[1] > 240 && p.0[2] > 240 && p.0[3] == 255 {
                        white_bg += 1;
                    }
                }
                let opaque_pct = (opaque as f64 / total as f64) * 100.0;
                let white_pct = (white_bg as f64 / total as f64) * 100.0;

                let center = rgba.get_pixel(w / 2, h / 2).0;
                println!(
                    "ok ({w}×{h}) → {path:?}  opaque={opaque_pct:.1}%  white_bg≈{white_pct:.1}%  center_rgba={center:?}"
                );
            }
            Err(e) => println!("FAIL: {e}"),
        }
    }

    println!("\nOpen /tmp/mermaid-smoke/*.png to inspect.");
}

#[cfg(not(feature = "markdown"))]
fn main() {
    eprintln!("Build with --features markdown");
    std::process::exit(1);
}
