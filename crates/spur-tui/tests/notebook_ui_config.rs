use std::fs;

#[test]
fn notebook_ui_config_defaults_chat_response_cap_to_240() {
    let dir = tempfile::tempdir().unwrap();

    let config = spur_tui::notebook_config::NotebookUiConfig::load_from_repo_root(dir.path());

    assert_eq!(config.chat_response_char_cap, 240);
}

#[test]
fn notebook_ui_config_reads_chat_response_cap_from_spur_notebook_toml() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".spur")).unwrap();
    fs::write(
        dir.path().join(".spur/notebook.toml"),
        "[ui]\nchat_response_char_cap = 96\n",
    )
    .unwrap();

    let config = spur_tui::notebook_config::NotebookUiConfig::load_from_repo_root(dir.path());

    assert_eq!(config.chat_response_char_cap, 96);
}
