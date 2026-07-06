//! Send-time helper: if the outgoing user message contains any
//! `worker://<name>` atoms whose names are known to the registry,
//! prepend a one-line preference hint as the first
//! `ContentBlock::Text` of the outgoing blocks.
//!
//! See design spec §4.6.

use std::collections::HashSet;

use spur_acp::{ContentBlock, TextContent};

use crate::components::input_bar::ProtectedRange;

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct WorkerHint {
    name: String,
    agent: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

impl WorkerHint {
    fn from_uri(uri: &str, known_workers: &HashSet<String>) -> Option<Self> {
        let remainder = uri.strip_prefix("worker://")?;
        let (name, query) = match remainder.split_once('?') {
            Some((name, query)) => (name, Some(query)),
            None => (remainder, None),
        };
        if !known_workers.contains(name) {
            return None;
        }

        let mut hint = Self {
            name: name.to_string(),
            agent: None,
            model: None,
            effort: None,
        };
        if let Some(query) = query {
            for param in query.split('&') {
                let Some((key, value)) = param.split_once('=') else {
                    continue;
                };
                if value.is_empty() {
                    continue;
                }
                match key {
                    "agent" => hint.agent = Some(value.to_string()),
                    "model" => hint.model = Some(value.to_string()),
                    "effort" => hint.effort = Some(value.to_string()),
                    _ => {}
                }
            }
        }
        Some(hint)
    }

    fn is_enriched(&self) -> bool {
        self.agent.is_some() || self.model.is_some() || self.effort.is_some()
    }

    fn render(&self) -> String {
        let mut rendered = self.name.clone();
        let mut params = Vec::new();
        if let Some(agent) = &self.agent {
            params.push(format!("agent={agent}"));
        }
        if let Some(model) = &self.model {
            params.push(format!("model={model}"));
        }
        if let Some(effort) = &self.effort {
            params.push(format!("effort={effort}"));
        }
        if !params.is_empty() {
            rendered.push_str(" (");
            rendered.push_str(&params.join(", "));
            rendered.push(')');
        }
        rendered
    }
}

/// Builds the hint by collecting `worker://<name>` URIs from `ranges`,
/// keeping only names present in `known_workers`, then sorting and
/// deduplicating exact worker/agent/model/effort tuples (sort-then-dedup
/// is required because `Vec::dedup` only removes *consecutive*
/// duplicates).
///
/// Returns `true` if a hint was prepended; otherwise leaves
/// `blocks` unchanged and returns `false`.
pub fn prepend_worker_hint(
    blocks: &mut Vec<ContentBlock>,
    ranges: &[ProtectedRange],
    known_workers: &HashSet<String>,
) -> bool {
    let mut hints: Vec<WorkerHint> = ranges
        .iter()
        .filter_map(|r| WorkerHint::from_uri(&r.uri, known_workers))
        .collect();
    hints.sort();
    hints.dedup();
    if hints.is_empty() {
        return false;
    }
    let mentions = hints
        .iter()
        .map(WorkerHint::render)
        .collect::<Vec<_>>()
        .join(", ");
    let hint = if hints.iter().any(WorkerHint::is_enriched) {
        format!(
            "[UI hint] User-suggested workers for delegation this turn: {mentions} \
             (preference, not override; honor unless delegation.avoid_for clearly matches, \
             or the task needs a different combination)."
        )
    } else {
        format!(
            "[UI hint] User-suggested workers for delegation this turn: {mentions} \
             (preference, not override; honor unless `delegation.avoid_for` clearly matches)."
        )
    };
    blocks.insert(0, ContentBlock::Text(TextContent::new(hint)));
    true
}

pub fn prepend_datasource_hint(
    blocks: &mut Vec<ContentBlock>,
    ranges: &[ProtectedRange],
    mut lookup_hint: impl FnMut(&str) -> Option<String>,
) -> bool {
    let mut hints: Vec<(String, String)> = ranges
        .iter()
        .filter(|range| range.uri.starts_with("datasource://"))
        .filter_map(|range| lookup_hint(&range.uri).map(|hint| (range.uri.clone(), hint)))
        .collect();
    hints.sort_by(|a, b| a.0.cmp(&b.0));
    hints.dedup_by(|a, b| a.0 == b.0);
    if hints.is_empty() {
        return false;
    }

    let body = hints
        .into_iter()
        .map(|(_, hint)| hint)
        .collect::<Vec<_>>()
        .join("\n---\n");
    let hint = format!("[UI hint] Datasource schemas mentioned this turn:\n{body}");
    blocks.insert(0, ContentBlock::Text(TextContent::new(hint)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::components::input_bar::RangeKind;

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn range(uri: &str) -> ProtectedRange {
        ProtectedRange {
            start: 0,
            end: 0,
            kind: RangeKind::Atom,
            uri: uri.into(),
            name: String::new(),
        }
    }

    fn hint_text(blocks: &[ContentBlock]) -> Option<&str> {
        match blocks.first()? {
            ContentBlock::Text(t) => Some(&t.text),
            _ => None,
        }
    }

    #[test]
    fn dedupes_and_sorts_known_workers() {
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![
            range("worker://a"),
            range("worker://a"),
            range("worker://missing"),
            range("worker://b"),
        ];
        let known = known(&["a", "b", "c"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(prepended);
        assert_eq!(blocks.len(), 2);
        let h = hint_text(&blocks).expect("first block is Text");
        assert!(h.starts_with("[UI hint]"));
        assert!(h.contains("a, b"), "expected 'a, b' in hint, got: {}", h);
        assert!(!h.contains("missing"));
    }

    #[test]
    fn noop_when_no_worker_ranges() {
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![range("file:///abs/foo.rs")];
        let known = known(&["a"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(!prepended);
        assert_eq!(blocks.len(), 1);
        assert_eq!(hint_text(&blocks), Some("user text"));
    }

    #[test]
    fn noop_when_all_worker_names_unknown() {
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![range("worker://ghost"), range("worker://phantom")];
        let known = known(&["a", "b"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(!prepended);
        assert_eq!(blocks.len(), 1);
        assert_eq!(hint_text(&blocks), Some("user text"));
    }

    #[test]
    fn enriched_worker_tuple_includes_selected_agent_model_and_effort() {
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![range(
            "worker://codex?agent=spur-narrow-implementer&model=gpt-5.5&effort=low",
        )];
        let known = known(&["codex"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(prepended);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            hint_text(&blocks),
            Some(
                "[UI hint] User-suggested workers for delegation this turn: codex \
                 (agent=spur-narrow-implementer, model=gpt-5.5, effort=low) \
                 (preference, not override; honor unless delegation.avoid_for clearly matches, \
                 or the task needs a different combination)."
            )
        );
    }

    #[test]
    fn multiple_enriched_worker_tuples_are_preserved() {
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![
            range("worker://opencode?model=claude-sonnet-4&effort=high"),
            range("worker://codex?agent=spur-narrow-implementer"),
        ];
        let known = known(&["codex", "opencode"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(prepended);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            hint_text(&blocks),
            Some(
                "[UI hint] User-suggested workers for delegation this turn: codex \
                 (agent=spur-narrow-implementer), opencode \
                 (model=claude-sonnet-4, effort=high) \
                 (preference, not override; honor unless delegation.avoid_for clearly matches, \
                 or the task needs a different combination)."
            )
        );
    }

    #[test]
    fn same_worker_with_different_enriched_tuples_both_survive() {
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![
            range("worker://codex?model=gpt-5.5&effort=low"),
            range("worker://codex?model=gpt-5.5&effort=high"),
            range("worker://codex?model=gpt-5.5&effort=low"),
        ];
        let known = known(&["codex"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(prepended);
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            hint_text(&blocks),
            Some(
                "[UI hint] User-suggested workers for delegation this turn: codex \
                 (model=gpt-5.5, effort=high), codex (model=gpt-5.5, effort=low) \
                 (preference, not override; honor unless delegation.avoid_for clearly matches, \
                 or the task needs a different combination)."
            )
        );
    }

    #[test]
    fn datasource_hint_injects_known_schema() {
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![
            range("datasource://sales"),
            range("datasource://sales"),
            range("datasource://missing"),
        ];
        let prepended = prepend_datasource_hint(&mut blocks, &ranges, |uri| match uri {
            "datasource://sales" => {
                Some("DATASOURCE sales\ncolumns:\n- revenue DOUBLE".to_string())
            }
            _ => None,
        });
        assert!(prepended);
        assert_eq!(blocks.len(), 2);
        let h = hint_text(&blocks).expect("first block is Text");
        assert!(h.starts_with("[UI hint] Datasource schemas"));
        assert!(h.contains("revenue DOUBLE"));
        assert!(!h.contains("missing"));
    }
}
