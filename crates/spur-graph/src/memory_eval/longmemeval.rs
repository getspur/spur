use serde::Deserialize;

use super::{graphify_slice, EvalSplit, MemoryTask, LME_GRAPHIFY_N};

#[derive(Debug, Deserialize)]
struct LongMemEvalItem {
    question_id: String,
    question: String,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    answer_session_ids: Vec<String>,
}

pub fn parse_longmemeval(json: &str, split: EvalSplit) -> anyhow::Result<Vec<MemoryTask>> {
    let items: Vec<LongMemEvalItem> = serde_json::from_str(json)?;
    let mut tasks = Vec::new();
    for item in items {
        if item.question_id.ends_with("_abs") {
            continue;
        }
        if item.answer_session_ids.is_empty() {
            continue;
        }
        tasks.push(MemoryTask {
            id: item.question_id,
            question: item.question,
            gold_ids: item.answer_session_ids,
            gold_answer: item.answer,
        });
    }
    Ok(match split {
        EvalSplit::Official => tasks,
        EvalSplit::Graphify => graphify_slice(&tasks, LME_GRAPHIFY_N).to_vec(),
    })
}
