use rmcp::model::Tool;
use serde_json::{json, Value};
use std::time::Duration;

pub mod delete_cell;
pub mod insert_cell;
pub mod interrupt;
pub mod kernel_info;
pub mod read_cell;
pub mod run_cell;
pub mod snapshot;
pub mod write_cell;

pub(crate) const BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn tools() -> Vec<Tool> {
    vec![
        snapshot::tool(),
        read_cell::tool(),
        kernel_info::tool(),
        insert_cell::tool(),
        write_cell::tool(),
        delete_cell::tool(),
        interrupt::tool(),
        run_cell::tool(),
    ]
}

fn empty_params() -> Value {
    json!({})
}
