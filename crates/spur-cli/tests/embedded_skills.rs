fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn spur_binary_embeds_skill_files_and_nested_resources() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_spur")).expect("read compiled spur binary");

    let skill = include_bytes!("../assets/skills/code-explore/SKILL.md");
    let nested_reference =
        include_bytes!("../assets/skills/notebook-mcp/references/tool-surface.md");

    assert!(
        contains_bytes(&binary, skill),
        "compiled spur binary does not contain the bundled SKILL.md payload"
    );
    assert!(
        contains_bytes(&binary, nested_reference),
        "compiled spur binary does not contain nested bundled skill resources"
    );
}
