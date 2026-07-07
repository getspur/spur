#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn content_hash_file_and_dir_are_stable() {
        let td = tempfile::tempdir().unwrap();
        let f = td.path().join("SKILL.md");
        std::fs::write(&f, "hello").unwrap();
        let h1 = crate::explore::content_hash(&f).unwrap();
        assert_eq!(h1, crate::explore::content_hash(&f).unwrap());

        let d = td.path().join("skill");
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(d.join("SKILL.md"), "body").unwrap();
        std::fs::write(d.join("scripts/run.sh"), "#!/bin/sh").unwrap();
        let hd = crate::explore::content_hash(&d).unwrap();
        assert_eq!(hd, crate::explore::content_hash(&d).unwrap());
        assert_ne!(h1, hd);
    }

    #[test]
    fn catalog_saves_and_loads_from_index_path() {
        let td = tempfile::tempdir().unwrap();
        let cat = Catalog {
            synced_at_epoch: Some(1),
            entries: vec![sample_entry()],
        };

        cat.save(td.path()).unwrap();

        assert!(td.path().join(".spur/explore/index/catalog.json").exists());
        assert_eq!(Catalog::load(td.path()).unwrap().entries, cat.entries);
    }

    #[test]
    fn scan_finds_skills_and_agents_in_checkout() {
        let td = tempfile::tempdir().unwrap();
        let sk = td.path().join("skills/api-design");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: api-design\ndescription: \"REST heuristics\"\nlicense: MIT\n---\nbody",
        )
        .unwrap();
        let ag = td.path().join("agents");
        std::fs::create_dir_all(&ag).unwrap();
        std::fs::write(
            ag.join("rust-pro.md"),
            "---\nname: rust-pro\ndescription: Rust specialist\n---\nYou are...",
        )
        .unwrap();
        std::fs::write(td.path().join("README.md"), "# readme").unwrap();

        let entries = scan_source_checkout(td.path(), "acme/repo", &"a".repeat(40)).unwrap();

        let names: Vec<_> = entries.iter().map(|e| (e.kind, e.name.as_str())).collect();
        assert!(names.contains(&(ItemKind::Skill, "api-design")));
        assert!(names.contains(&(ItemKind::Agent, "rust-pro")));
        assert_eq!(entries.len(), 2);
        let skill = entries.iter().find(|e| e.kind == ItemKind::Skill).unwrap();
        assert_eq!(skill.license.as_deref(), Some("MIT"));
        assert_eq!(skill.rel_path, "skills/api-design");
    }
}
