use std::collections::BTreeSet;

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/cpp/spur-edges.scm");

fn capture_texts(source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, SPUR_EDGES_QUERY).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();

    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        if capture_names[capture.index as usize] == capture_name {
            names.push(
                capture
                    .node
                    .utf8_text(source.as_bytes())
                    .expect("capture text")
                    .to_owned(),
            );
        }
    }

    names
}

fn capture_set(source: &str, capture_name: &str) -> BTreeSet<String> {
    capture_texts(source, capture_name).into_iter().collect()
}

#[test]
fn cpp_spur_edges_query_captures_imports_calls_and_only_std_hof_references() {
    let source = r#"
#include "catalog.hpp"
#include <vector>
using namespace demo::util;
using std::vector;

namespace demo {
int helper();

template <typename T>
T make();

struct Runner {
  void step();
};
}

namespace custom {
template <typename It, typename Fn>
void for_each(It first, It last, Fn fn) {}
}

bool keep(int value) {
  return value > 0;
}

bool outside_std(int value) {
  return value > 1;
}

void run(demo::Runner* runner, std::vector<int>& values) {
  helper();
  demo::helper();
  runner->step();
  make<int>();
  std::for_each(values.begin(), values.end(), keep);
  custom::for_each(values.begin(), values.end(), outside_std);
}
"#;

    let import_names = capture_set(source, "import.name");
    for target in [
        "\"catalog.hpp\"",
        "<vector>",
        "demo::util",
        "std::vector",
        "vector",
    ] {
        assert!(
            import_names.contains(target),
            "missing C++ import capture {target}; imports: {import_names:?}"
        );
    }

    let call_names = capture_set(source, "call.name");
    for target in ["helper", "step", "make"] {
        assert!(
            call_names.contains(target),
            "missing C++ call capture {target}; calls: {call_names:?}"
        );
    }

    let reference_names = capture_set(source, "reference.name");
    assert!(
        reference_names.contains("keep"),
        "std::for_each callback should be ReferencesHof; references: {reference_names:?}"
    );
    assert!(
        !reference_names.contains("outside_std"),
        "custom::for_each must not be treated as std HOF; references: {reference_names:?}"
    );
}
