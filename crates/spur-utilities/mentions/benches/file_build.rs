//! Full snapshot rebuilds over a generated tree (no engine snapshot cache).
//! OS filesystem caches are warm; fixture creation is outside every timer.
//! `scripts/spur-cargo bench -p spur-mentions --bench file_build`
use std::hint::black_box;
use std::time::Instant;

use ignore::WalkBuilder;
use spur_mentions::{FileMentionSource, FilesystemProfile, MentionSource, SourceContext};

fn median_ns(mut body: impl FnMut()) -> u128 {
    for _ in 0..3 {
        body();
    }
    let mut samples: Vec<_> = (0..25)
        .map(|_| {
            let start = Instant::now();
            body();
            start.elapsed().as_nanos()
        })
        .collect();
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn main() {
    for count in [1_000, 10_000] {
        let tree = tempfile::tempdir().unwrap();
        for dir in 0..100 {
            std::fs::create_dir(tree.path().join(format!("dir{dir}"))).unwrap();
        }
        for file in 0..count {
            std::fs::write(
                tree.path().join(format!("dir{}/file{file}.rs", file % 100)),
                "",
            )
            .unwrap();
        }
        let root = tree.path();
        for (name, profile) in [
            ("tui", FilesystemProfile::tui()),
            ("notebook", FilesystemProfile::notebook_compat()),
        ] {
            for stat in [false, true] {
                let ns = median_ns(|| {
                    let walker = WalkBuilder::new(root)
                        .follow_links(profile.follow_symlinks)
                        .hidden(!profile.include_hidden)
                        .git_ignore(profile.apply_ignore_rules)
                        .git_exclude(profile.apply_ignore_rules)
                        .ignore(profile.apply_ignore_rules)
                        .build();
                    for entry in walker {
                        let entry = entry.unwrap();
                        if stat {
                            black_box(entry.path().is_dir());
                        }
                        black_box(entry);
                    }
                });
                println!("walk files={count} profile={name} extra_stat={stat} median_ns={ns}");
            }
            let context = SourceContext {
                filesystem_profile: profile,
            };
            let ns = median_ns(|| {
                let snapshot = FileMentionSource::new().build(root, &context).unwrap();
                assert_eq!(
                    snapshot.entries.len(),
                    count + if profile.include_directories { 100 } else { 0 }
                );
                black_box(snapshot);
            });
            println!("build files={count} profile={name} median_ns={ns}");
        }
    }
}
