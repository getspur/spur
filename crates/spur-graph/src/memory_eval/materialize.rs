use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct LocomoSample {
    sample_id: String,
    conversation: Value,
}

pub fn materialize_locomo(json: &str, root: &Path) -> anyhow::Result<()> {
    let samples: Vec<LocomoSample> = serde_json::from_str(json)?;
    for sample in samples {
        let conv_dir = root.join(&sample.sample_id);
        fs::create_dir_all(&conv_dir)?;
        let Some(object) = sample.conversation.as_object() else {
            continue;
        };
        let mut session_keys: Vec<&String> = object
            .keys()
            .filter(|key| key.starts_with("session_") && !key.ends_with("_date_time"))
            .collect();
        session_keys.sort();
        for key in session_keys {
            let Some(turns) = object.get(key).and_then(Value::as_array) else {
                continue;
            };
            let mut markdown = format!("# {key}\n\n");
            for turn in turns {
                let dia_id = turn
                    .get("dia_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let text = turn.get("text").and_then(Value::as_str).unwrap_or("");
                markdown.push_str(&format!("## {dia_id} {text}\n\n{text}\n\n"));
            }
            fs::write(conv_dir.join(format!("{key}.md")), markdown)?;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct LongMemEvalItem {
    question_id: String,
    haystack_session_ids: Vec<String>,
    haystack_sessions: Vec<Vec<Value>>,
}

pub fn materialize_longmemeval(json: &str, root: &Path) -> anyhow::Result<()> {
    let items: Vec<LongMemEvalItem> = serde_json::from_str(json)?;
    for item in items {
        if item.question_id.ends_with("_abs") {
            continue;
        }
        let q_dir = root.join(&item.question_id);
        fs::create_dir_all(&q_dir)?;
        for (index, session_id) in item.haystack_session_ids.iter().enumerate() {
            let turns = item
                .haystack_sessions
                .get(index)
                .cloned()
                .unwrap_or_default();
            let mut markdown = format!("# {session_id}\n\n");
            for turn in turns {
                let content = turn.get("content").and_then(Value::as_str).unwrap_or("");
                markdown.push_str(&format!("## {session_id} {content}\n\n{content}\n\n"));
            }
            fs::write(q_dir.join(format!("{session_id}.md")), markdown)?;
        }
    }
    Ok(())
}
