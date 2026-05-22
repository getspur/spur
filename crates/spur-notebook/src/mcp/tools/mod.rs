use rmcp::model::Tool;
use serde_json::{json, Value};
use std::time::Duration;

pub mod kernel_info;
pub mod read_cell;
pub mod snapshot;

pub(crate) const BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn tools() -> Vec<Tool> {
    vec![snapshot::tool(), read_cell::tool(), kernel_info::tool()]
}

fn empty_params() -> Value {
    json!({})
}
