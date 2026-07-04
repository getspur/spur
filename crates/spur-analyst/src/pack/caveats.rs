use serde_json::{json, Value};

use crate::SymbolEvidenceCaveat;

pub(crate) fn symbol_caveat_value(caveat: &SymbolEvidenceCaveat) -> Value {
    caveat_value(
        caveat.code.clone(),
        caveat.message.clone(),
        caveat.stable_symbol_id.clone(),
    )
}

pub(crate) fn caveat_value(
    code: impl Into<String>,
    message: impl Into<String>,
    stable_symbol_id: Option<String>,
) -> Value {
    json!({
        "code": code.into(),
        "message": message.into(),
        "stable_symbol_id": stable_symbol_id,
    })
}

pub(crate) fn push_graph_path_caveat(
    caveats: &mut Vec<Value>,
    message: impl Into<String>,
    source: &str,
) {
    let caveat = caveat_value("graph_path_unavailable", message, Some(source.to_owned()));
    if !caveats.contains(&caveat) {
        caveats.push(caveat);
    }
}
