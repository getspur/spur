use serde::Deserialize;
use serde_json::Value;

use super::{
    graphify_slice, EvalSplit, MemoryTask, LOCOMO_ADVERSARIAL_CATEGORY, LOCOMO_GRAPHIFY_N,
};

#[derive(Debug, Deserialize)]
struct LocomoSample {
    sample_id: String,
    qa: Vec<LocomoQa>,
}

#[derive(Debug, Deserialize)]
struct LocomoQa {
    question: String,
    #[serde(default)]
    answer: Value,
    category: u32,
    #[serde(default)]
    evidence: Vec<String>,
}

pub fn parse_locomo(json: &str, split: EvalSplit) -> anyhow::Result<Vec<MemoryTask>> {
    let samples: Vec<LocomoSample> = serde_json::from_str(json)?;
    let mut tasks = Vec::new();
    for sample in samples {
        for (index, qa) in sample.qa.into_iter().enumerate() {
            if qa.category == LOCOMO_ADVERSARIAL_CATEGORY {
                continue;
            }
            if qa.evidence.is_empty() {
                continue;
            }
            tasks.push(MemoryTask {
                id: format!("{}#{index}", sample.sample_id),
                question: qa.question,
                gold_ids: qa.evidence,
                gold_answer: stringify_answer(&qa.answer),
            });
        }
    }
    Ok(match split {
        EvalSplit::Official => tasks,
        EvalSplit::Graphify => graphify_slice(&tasks, LOCOMO_GRAPHIFY_N).to_vec(),
    })
}

fn stringify_answer(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => String::new(),
    }
}
