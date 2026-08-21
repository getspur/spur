use spur_acp::config::{ConfigPatch, EditorMode, SkillsProjectionMode, SpurConfig};

#[test]
fn graph_patch_writes_canonical_alias() {
    let mut cfg = SpurConfig::default();
    ConfigPatch::GraphEmbeddingModel {
        alias: "coderank".into(),
    }
    .apply(&mut cfg)
    .unwrap();
    assert_eq!(cfg.graph.embedding_model.as_deref(), Some("coderank"));
}

#[test]
fn graph_patch_rejects_unknown_alias() {
    let mut cfg = SpurConfig::default();
    let err = ConfigPatch::GraphEmbeddingModel {
        alias: "not-a-model".into(),
    }
    .apply(&mut cfg)
    .expect_err("unknown alias");
    assert!(format!("{err:#}").contains("embedding"));
    assert!(cfg.graph.embedding_model.is_none());
}

#[test]
fn skills_and_tui_patches_do_not_clobber_siblings() {
    let mut cfg = SpurConfig::default();
    cfg.tui.theme = "light".into();
    ConfigPatch::TuiEditMode(EditorMode::Vim)
        .apply(&mut cfg)
        .unwrap();
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);
    assert_eq!(cfg.tui.theme, "light");

    ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::AllActive)
        .apply(&mut cfg)
        .unwrap();
    assert_eq!(cfg.skills.projection_mode, SkillsProjectionMode::AllActive);
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);
}

#[test]
fn section_id_matches_configure_tokens() {
    assert_eq!(
        ConfigPatch::GraphEmbeddingModel {
            alias: "nomic".into()
        }
        .section_id(),
        "graph"
    );
    assert_eq!(ConfigPatch::TuiDisablePasteBurst(true).section_id(), "tui");
    assert_eq!(
        ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::CatalogOnly).section_id(),
        "skills"
    );
}
