use std::path::PathBuf;

use spur_telemetry::events::IntoProp;

fn require_into_prop<T: IntoProp>(_: T) {}

fn main() {
    require_into_prop(PathBuf::from("nope"));
}
