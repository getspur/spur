#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::catalog::{CatalogEntry, ItemKind};

    fn sample_entry() -> CatalogEntry {
        CatalogEntry {
            kind: ItemKind::Skill,
            name: "api-design".to_string(),
            source: "acme/repo".to_string(),
            rel_path: "skills/api-design".to_string(),
            pinned_commit: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            description: "REST heuristics".to_string(),
            license: Some("MIT".to_string()),
            content_sha256: "0".repeat(64),
        }
    }

    fn sample_item(name: &str, verdict: &str) -> ManifestItem {
        let entry = CatalogEntry {
            name: name.to_string(),
            ..sample_entry()
        };
        item_from_entry(
            &entry,
            GateRecord {
                verdict: verdict.to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        )
    }

    #[test]
    fn manifest_roundtrips_toml() {
        let manifest = Manifest {
            sources: vec![SourceSpec {
                repo: "acme/repo".to_string(),
                url: None,
                pin: "main".to_string(),
            }],
            items: vec![sample_item("api-design", "clean")],
        };
        let td = tempfile::tempdir().unwrap();

        manifest.save(td.path()).unwrap();

        let loaded = Manifest::load(td.path()).unwrap();
        assert_eq!(loaded, manifest);

        let empty_dir = tempfile::tempdir().unwrap();
        let empty = Manifest::load(empty_dir.path()).unwrap();
        assert!(empty.items.is_empty());
        assert!(empty.sources.is_empty());
    }

    #[test]
    fn vendor_copies_item_and_status_detects_tamper() {
        let td = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let sk = src.path().join("skills/api-design");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: api-design\ndescription: d\n---\nbody",
        )
        .unwrap();
        let entry = CatalogEntry {
            content_sha256: crate::explore::content_hash(&sk).unwrap(),
            ..sample_entry()
        };

        vendor(td.path(), src.path(), &entry).unwrap();

        let pdir = pool_dir(td.path(), "acme/repo", "api-design", &entry.pinned_commit);
        assert!(pdir.join("SKILL.md").exists());

        let mut manifest = Manifest::default();
        manifest.items.push(item_from_entry(
            &entry,
            GateRecord {
                verdict: "clean".to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        ));
        let st = status(td.path(), &manifest);
        assert!(st.sha_mismatch.is_empty());
        assert!(st.missing.is_empty());
        assert_eq!(st.ok, vec!["api-design".to_string()]);

        std::fs::write(pdir.join("SKILL.md"), "tampered").unwrap();
        let st = status(td.path(), &manifest);
        assert_eq!(st.sha_mismatch, vec!["api-design".to_string()]);
    }
}
