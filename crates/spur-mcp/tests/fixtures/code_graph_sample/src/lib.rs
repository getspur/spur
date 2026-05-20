pub fn launch_order() -> bool {
    orchestrate_order()
}

pub fn orchestrate_order() -> bool {
    let parsed = parse_order();
    let charged = charge_order();
    parsed && charged
}

fn parse_order() -> bool {
    true
}

fn charge_order() -> bool {
    audit_order()
}

fn audit_order() -> bool {
    true
}
