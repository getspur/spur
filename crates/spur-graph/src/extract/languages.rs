use tree_sitter::Language;

pub fn rust_language() -> Language {
    tree_sitter_rust::LANGUAGE.into()
}
