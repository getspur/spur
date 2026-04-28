// Verifies ViewId can be used as a HashMap key.
use std::collections::HashMap;

use spur_tui::action::ViewId;

#[test]
fn view_id_is_hashable() {
    let mut map: HashMap<ViewId, u32> = HashMap::new();
    map.insert(ViewId::Dashboard, 1);
    map.insert(ViewId::IssueBrowser, 2);
    assert_eq!(map[&ViewId::Dashboard], 1);
    assert_eq!(map[&ViewId::IssueBrowser], 2);
}
