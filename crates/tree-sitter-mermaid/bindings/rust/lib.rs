//! Tree-sitter grammar for Mermaid, compiled against workspace `tree-sitter` 0.26.
//!
//! C parser sources are vendored from [monaqa/tree-sitter-mermaid](https://github.com/monaqa/tree-sitter-mermaid)
//! (MIT). This crate does not use crates.io `tree-sitter-mermaid` 0.1.0 (rust 1.95 / tree-sitter 0.26).

use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_mermaid() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for this grammar.
///
/// # Safety
/// `tree_sitter_mermaid` is generated parser C and returns a process-lifetime `TSLanguage`.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_mermaid) };

#[cfg(test)]
mod tests {
    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("Error loading Mermaid parser");
        let src = "flowchart TD\n    SPEC[spec]\n    CHECK[check]\n    SPEC --> CHECK\n";
        let tree = parser.parse(src, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "flowchart parse error: {}",
            tree.root_node().to_sexp()
        );
    }
}
