//! sud-m-hint acceptance: the `InputBar` mention-hint policy lives at
//! `spur_tui::components::input_bar_mention_hint` (spec §4 item 3) — next to
//! `input_bar_wrap`, reachable through the public `components` module. The
//! behavior is unchanged from the old `mentions::hint` module; its unit tests
//! moved with the file. This test pins the new public path end to end.

use std::collections::HashSet;

use spur_acp::{ContentBlock, TextContent};
use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::components::input_bar_mention_hint::prepend_worker_hint;

fn range(uri: &str) -> ProtectedRange {
    use spur_tui::components::input_bar::RangeKind;
    ProtectedRange {
        start: 0,
        end: 0,
        kind: RangeKind::Atom,
        uri: uri.into(),
        name: String::new(),
    }
}

#[test]
fn prepend_worker_hint_is_reachable_via_components_module() {
    let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
    let ranges = vec![range("worker://codex"), range("worker://kimi")];
    let known: HashSet<String> = ["codex", "kimi"].into_iter().map(String::from).collect();

    let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);

    assert!(prepended);
    assert_eq!(blocks.len(), 2);
    match &blocks[0] {
        ContentBlock::Text(t) => {
            assert!(t.text.starts_with("[UI hint]"), "got: {}", t.text);
            assert!(t.text.contains("codex, kimi"), "got: {}", t.text);
        }
        _ => panic!("first block should be the hint text"),
    }
    match &blocks[1] {
        ContentBlock::Text(t) => assert_eq!(t.text, "user text"),
        _ => panic!("second block should be the user text"),
    }
}
