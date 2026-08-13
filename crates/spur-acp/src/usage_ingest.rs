//! Observed-usage ingest for ACP prompt turns.
//!
//! Authority: sol_22359951b3b04179 (feasibility), sol_ea8353ac1fef4f40 (H1),
//! sol_2c4533c83496473b (H2), sol_4a1409298e394938 (H3), sol_d0f08367052542d5 (H6),
//! sol_10e37ea151524119 (R1 Grok witness: turn_completed, not chunk `_meta.totalTokens`).
//!
//! Tokens come only from observed usage fields. Duration, `$cost`, context
//! used/size, first_token, `events.jsonl`, and chunk `_meta.totalTokens` are
//! never token sources.

use agent_client_protocol::schema::v1::Usage;
use serde_json::{Map, Value};

/// Runtime / coding-agent entry names that must never become `model_name`.
const FORBIDDEN_MODEL_NAMES: [&str; 4] = ["grok", "opencode", "codex", "spur"];

/// Parsed Grok `_x.ai/session/update` `turn_completed.usage`.
#[derive(Debug, Clone)]
pub struct TurnCompletedUsage {
    pub usage: Usage,
    /// LLM id from `modelUsage` key or `current_model_id`. Never a runtime id.
    pub model_name: Option<String>,
    pub num_turns: Option<u64>,
}

/// Extract usage from a Grok extension notification `params` object.
///
/// Expected shape (updates.jsonl / live ACP):
/// `{ sessionId, update: { sessionUpdate: "turn_completed", usage: {…} } }`
pub fn turn_completed_from_ext_params(params: &Value) -> Option<TurnCompletedUsage> {
    let update = params.get("update")?;
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("turn_completed") {
        return None;
    }
    let usage_obj = update.get("usage")?.as_object()?;
    usage_from_turn_completed_object(usage_obj, update)
}

fn usage_from_turn_completed_object(
    usage_obj: &Map<String, Value>,
    update: &Value,
) -> Option<TurnCompletedUsage> {
    let input = json_u64(usage_obj, &["inputTokens", "input_tokens"]);
    let output = json_u64(usage_obj, &["outputTokens", "output_tokens"]);
    let cache_create = json_u64(
        usage_obj,
        &[
            "cacheCreationTokens",
            "cache_creation_tokens",
            "cacheCreationInputTokens",
        ],
    );
    let cache_read = json_u64(
        usage_obj,
        &[
            "cachedReadTokens",
            "cache_read_tokens",
            "cached_read_tokens",
            "cacheReadInputTokens",
        ],
    );

    // H1: at least one observed token key. costUsdTicks / apiDurationMs /
    // _meta.totalTokens / context used-size are not token keys.
    if input.is_none() && output.is_none() && cache_create.is_none() && cache_read.is_none() {
        return None;
    }

    let total = json_u64(usage_obj, &["totalTokens", "total_tokens"])
        .unwrap_or_else(|| input.unwrap_or(0).saturating_add(output.unwrap_or(0)));
    let mut usage = Usage::new(total, input.unwrap_or(0), output.unwrap_or(0));
    if let Some(c) = cache_create {
        usage = usage.cached_write_tokens(c);
    }
    if let Some(r) = cache_read {
        usage = usage.cached_read_tokens(r);
    }

    Some(TurnCompletedUsage {
        usage,
        model_name: model_name_from_turn_completed(usage_obj, update),
        num_turns: json_u64(usage_obj, &["numTurns", "num_turns"]),
    })
}

fn model_name_from_turn_completed(
    usage_obj: &Map<String, Value>,
    update: &Value,
) -> Option<String> {
    if let Some(model_usage) = usage_obj.get("modelUsage").and_then(Value::as_object) {
        let mut best: Option<(u64, String)> = None;
        for (key, value) in model_usage {
            if !is_llm_model_id(key) {
                continue;
            }
            let score = value
                .get("totalTokens")
                .and_then(Value::as_u64)
                .or_else(|| value.get("inputTokens").and_then(Value::as_u64))
                .unwrap_or(0);
            match &best {
                None => best = Some((score, key.clone())),
                Some((prev, _)) if score >= *prev => best = Some((score, key.clone())),
                _ => {}
            }
        }
        if let Some((_, name)) = best {
            return Some(name);
        }
    }

    for source in [update, &Value::Object(usage_obj.clone())] {
        if let Some(id) = source
            .get("current_model_id")
            .or_else(|| source.get("currentModelId"))
            .and_then(Value::as_str)
        {
            if is_llm_model_id(id) {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// True when `name` is an LLM id (not a coding-agent / runtime entry).
pub fn is_llm_model_id(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && !FORBIDDEN_MODEL_NAMES
            .iter()
            .any(|forbidden| trimmed.eq_ignore_ascii_case(forbidden))
}

/// Apply `turn_completed.usage` when `last_prompt_usage` is still empty.
///
/// H6: PromptResponse.usage already in the slot wins. Never invent tokens
/// from duration, `$cost`, or chunk `_meta.totalTokens`.
pub fn maybe_apply_turn_completed(
    last_prompt_usage: &std::sync::Mutex<Option<Usage>>,
    last_num_turns: &std::sync::Mutex<Option<u64>>,
    session_models: &std::sync::Mutex<std::collections::HashMap<String, String>>,
    params: &Value,
) -> bool {
    let Some(parsed) = turn_completed_from_ext_params(params) else {
        return false;
    };
    if let Ok(mut slot) = last_prompt_usage.lock() {
        if slot.is_none() {
            *slot = Some(parsed.usage);
        }
    }
    if let Some(n) = parsed.num_turns {
        if let Ok(mut slot) = last_num_turns.lock() {
            if slot.is_none() {
                *slot = Some(n);
            }
        }
    }
    if let Some(model) = parsed.model_name {
        if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
            if let Ok(mut cache) = session_models.lock() {
                cache.insert(session_id.to_owned(), model);
            }
        }
    }
    true
}

fn json_u64(obj: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Some(n) = v.as_i64() {
                return Some(n.max(0) as u64);
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn grok_turn_completed_params() -> Value {
        json!({
            "sessionId": "019ff892-5356-7de3-a63f-3137f2bc894a",
            "update": {
                "sessionUpdate": "turn_completed",
                "prompt_id": "68689533-da69-4c3b-a82c-bfabc71d922c",
                "stop_reason": "end_turn",
                "usage": {
                    "inputTokens": 19334,
                    "outputTokens": 170,
                    "totalTokens": 19504,
                    "cachedReadTokens": 11648,
                    "cacheCreationTokens": 0,
                    "reasoningTokens": 126,
                    "modelCalls": 1,
                    "apiDurationMs": 4469,
                    "costUsdTicks": 222160000,
                    "modelUsage": {
                        "grok-4.6-build": {
                            "inputTokens": 19334,
                            "outputTokens": 170,
                            "totalTokens": 19504,
                            "cachedReadTokens": 11648,
                            "cacheCreationTokens": 0
                        }
                    },
                    "numTurns": 1
                }
            },
            "_meta": {
                "eventId": "evt-1",
                "agentTimestampMs": 1_786_581_772_492_i64
            }
        })
    }

    #[test]
    fn r1_maps_turn_completed_usage_fields() {
        let parsed = turn_completed_from_ext_params(&grok_turn_completed_params())
            .expect("observed turn_completed.usage");
        assert_eq!(parsed.usage.input_tokens, 19334);
        assert_eq!(parsed.usage.output_tokens, 170);
        assert_eq!(parsed.usage.cached_write_tokens, Some(0));
        assert_eq!(parsed.usage.cached_read_tokens, Some(11648));
        assert_eq!(parsed.num_turns, Some(1));
        assert_eq!(parsed.model_name.as_deref(), Some("grok-4.6-build"));
    }

    #[test]
    fn r1_does_not_use_chunk_meta_total_tokens() {
        // Witness: ping session chunk `_meta.totalTokens` 11670 ≠ turn_completed 22371.
        let params = json!({
            "sessionId": "ping",
            "update": {
                "sessionUpdate": "turn_completed",
                "usage": {
                    "inputTokens": 20000,
                    "outputTokens": 2371,
                    "cacheCreationTokens": 0,
                    "cachedReadTokens": 50,
                    "numTurns": 1,
                    "modelUsage": { "grok-4.6": { "inputTokens": 20000, "outputTokens": 2371 } }
                }
            },
            "_meta": { "totalTokens": 11670 }
        });
        let parsed = turn_completed_from_ext_params(&params).expect("usage");
        assert_eq!(parsed.usage.input_tokens, 20000);
        assert_eq!(parsed.usage.output_tokens, 2371);
        assert_ne!(
            parsed
                .usage
                .input_tokens
                .saturating_add(parsed.usage.output_tokens),
            11670
        );
        assert_eq!(
            parsed
                .usage
                .input_tokens
                .saturating_add(parsed.usage.output_tokens),
            22371
        );
        assert_eq!(parsed.model_name.as_deref(), Some("grok-4.6"));
    }

    #[test]
    fn h1_ignores_duration_cost_and_meta_only_payloads() {
        let duration_cost_only = json!({
            "sessionId": "s",
            "update": {
                "sessionUpdate": "turn_completed",
                "usage": {
                    "apiDurationMs": 4469,
                    "costUsdTicks": 222160000,
                    "reasoningTokens": 126
                }
            },
            "_meta": { "totalTokens": 11670 }
        });
        assert!(turn_completed_from_ext_params(&duration_cost_only).is_none());
    }

    #[test]
    fn h1_ignores_non_turn_completed_session_updates() {
        let params = json!({
            "sessionId": "s",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "usage": { "inputTokens": 9, "outputTokens": 1 }
            }
        });
        assert!(turn_completed_from_ext_params(&params).is_none());
    }

    #[test]
    fn h3_rejects_runtime_ids_as_model_name() {
        let params = json!({
            "sessionId": "s",
            "update": {
                "sessionUpdate": "turn_completed",
                "current_model_id": "grok",
                "usage": {
                    "inputTokens": 10,
                    "outputTokens": 2,
                    "modelUsage": { "grok": { "inputTokens": 10 } }
                }
            }
        });
        let parsed = turn_completed_from_ext_params(&params).expect("tokens still observed");
        assert_eq!(parsed.model_name, None);
        assert!(!is_llm_model_id("grok"));
        assert!(!is_llm_model_id("opencode"));
        assert!(!is_llm_model_id("codex"));
        assert!(!is_llm_model_id("spur"));
        assert!(is_llm_model_id("grok-4.6"));
    }

    #[test]
    fn h3_current_model_id_fallback_when_model_usage_absent() {
        let params = json!({
            "sessionId": "s",
            "update": {
                "sessionUpdate": "turn_completed",
                "current_model_id": "grok-4.6",
                "usage": { "inputTokens": 3, "outputTokens": 1 }
            }
        });
        let parsed = turn_completed_from_ext_params(&params).expect("usage");
        assert_eq!(parsed.model_name.as_deref(), Some("grok-4.6"));
    }

    #[test]
    fn h7_observed_zero_cache_is_some_zero_not_missing() {
        let parsed = turn_completed_from_ext_params(&grok_turn_completed_params()).expect("usage");
        assert_eq!(parsed.usage.cached_write_tokens, Some(0));
    }

    #[test]
    fn h7_missing_cache_keys_stay_none() {
        let params = json!({
            "sessionId": "s",
            "update": {
                "sessionUpdate": "turn_completed",
                "usage": { "inputTokens": 4, "outputTokens": 1, "numTurns": 2 }
            }
        });
        let parsed = turn_completed_from_ext_params(&params).expect("usage");
        assert_eq!(parsed.usage.cached_write_tokens, None);
        assert_eq!(parsed.usage.cached_read_tokens, None);
        assert_eq!(parsed.num_turns, Some(2));
    }

    #[test]
    fn h6_turn_completed_fills_empty_slot() {
        let usage_slot = std::sync::Mutex::new(None);
        let turns_slot = std::sync::Mutex::new(None);
        let models = std::sync::Mutex::new(std::collections::HashMap::new());
        assert!(maybe_apply_turn_completed(
            &usage_slot,
            &turns_slot,
            &models,
            &grok_turn_completed_params()
        ));
        let usage = usage_slot.lock().unwrap().clone().expect("filled");
        assert_eq!(usage.input_tokens, 19334);
        assert_eq!(*turns_slot.lock().unwrap(), Some(1));
        assert_eq!(
            models
                .lock()
                .unwrap()
                .get("019ff892-5356-7de3-a63f-3137f2bc894a"),
            Some(&"grok-4.6-build".to_string())
        );
    }

    #[test]
    fn h6_does_not_clobber_prompt_response_usage() {
        let usage_slot = std::sync::Mutex::new(Some(Usage::new(100, 80, 20)));
        let turns_slot = std::sync::Mutex::new(None);
        let models = std::sync::Mutex::new(std::collections::HashMap::new());
        assert!(maybe_apply_turn_completed(
            &usage_slot,
            &turns_slot,
            &models,
            &grok_turn_completed_params()
        ));
        let usage = usage_slot.lock().unwrap().clone().expect("kept");
        assert_eq!(usage.input_tokens, 80);
        assert_eq!(usage.output_tokens, 20);
    }
}
