use crate::graph::{ExecutionPlan, ExecutionTrack, PlanBody, PlanSummary, TrackItem};
use crate::graph_engine::score::{is_actionable, transitive_unblocks, ScoreConfig};
use crate::graph_engine::snapshot::GraphSnapshot;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};

pub fn compute_plan(snap: &GraphSnapshot, _cfg: &ScoreConfig) -> ExecutionPlan {
    let open: HashSet<NodeIndex> = snap
        .graph
        .node_indices()
        .filter(|&ix| matches!(snap.graph[ix].status.as_str(), "open" | "in_progress"))
        .collect();

    let mut memo = HashMap::new();
    let mut depth = HashMap::new();
    let mut open_ordered: Vec<NodeIndex> = open.iter().copied().collect();
    open_ordered.sort_by(|a, b| snap.graph[*a].id.cmp(&snap.graph[*b].id));
    for ix in open_ordered {
        let mut visiting = HashSet::new();
        depth.insert(ix, depth_for(snap, ix, &open, &mut memo, &mut visiting));
    }

    let mut by_depth: HashMap<u32, Vec<NodeIndex>> = HashMap::new();
    for (ix, depth) in depth {
        by_depth.entry(depth).or_default().push(ix);
    }

    let mut ordered_depths: Vec<u32> = by_depth.keys().copied().collect();
    ordered_depths.sort_unstable();

    let mut tracks = Vec::new();
    for (i, depth) in ordered_depths.into_iter().enumerate() {
        let mut items = by_depth
            .remove(&depth)
            .expect("depth came from by_depth keys");
        items.sort_by(|a, b| {
            snap.graph[*a]
                .priority
                .cmp(&snap.graph[*b].priority)
                .then_with(|| snap.graph[*a].id.cmp(&snap.graph[*b].id))
        });

        let track_items: Vec<TrackItem> = items
            .into_iter()
            .map(|ix| {
                let node = &snap.graph[ix];
                let mut unblocks: Vec<String> = snap
                    .graph
                    .edges(ix)
                    .filter(|edge| edge.weight().kind.is_blocking())
                    .map(|edge| snap.graph[edge.target()].id.clone())
                    .collect();
                unblocks.sort_unstable();

                TrackItem {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    priority: Some(node.priority),
                    status: Some(node.status.clone()),
                    unblocks: if unblocks.is_empty() {
                        None
                    } else {
                        Some(unblocks)
                    },
                }
            })
            .collect();

        let reason = if track_items.len() == 1 && depth == 0 {
            "Single actionable item".into()
        } else {
            format!("Generation {depth} - depends on {depth} completed track(s) above")
        };

        tracks.push(ExecutionTrack {
            track_id: track_letter(i),
            items: track_items,
            reason: Some(reason),
        });
    }

    let total_actionable = open.iter().filter(|&&ix| is_actionable(snap, ix)).count();
    let total_blocked = open.len() - total_actionable;

    let summary = open
        .iter()
        .copied()
        .max_by(|&a, &b| {
            transitive_unblocks(snap, a)
                .cmp(&transitive_unblocks(snap, b))
                .then_with(|| snap.graph[b].id.cmp(&snap.graph[a].id))
        })
        .map(|ix| {
            let unblocks_count = transitive_unblocks(snap, ix);
            PlanSummary {
                highest_impact: Some(snap.graph[ix].id.clone()),
                impact_reason: Some(format!("Unblocks {unblocks_count} downstream issue(s)")),
                unblocks_count,
            }
        });

    ExecutionPlan {
        generated_at: Some(snap.generated_at.to_rfc3339()),
        data_hash: Some(snap.data_hash.clone()),
        plan: PlanBody {
            tracks,
            total_actionable,
            total_blocked,
            summary,
        },
        usage_hints: vec!["jq '.plan.tracks[] | {id: .track_id, items: [.items[].id]}'".into()],
        raw: serde_json::Value::Null,
    }
}

fn depth_for(
    snap: &GraphSnapshot,
    ix: NodeIndex,
    open: &HashSet<NodeIndex>,
    memo: &mut HashMap<NodeIndex, u32>,
    visiting: &mut HashSet<NodeIndex>,
) -> u32 {
    if let Some(&depth) = memo.get(&ix) {
        return depth;
    }
    if !visiting.insert(ix) {
        return 0;
    }

    let mut max_pred = 0;
    for edge in snap.graph.edges_directed(ix, petgraph::Direction::Incoming) {
        if !edge.weight().kind.is_blocking() {
            continue;
        }

        let pred = edge.source();
        if !open.contains(&pred) {
            continue;
        }
        if visiting.contains(&pred) {
            continue;
        }

        let depth = 1 + depth_for(snap, pred, open, memo, visiting);
        if depth > max_pred {
            max_pred = depth;
        }
    }

    visiting.remove(&ix);
    memo.insert(ix, max_pred);
    max_pred
}

fn track_letter(i: usize) -> String {
    let mut s = String::new();
    let mut n = i + 1;
    while n > 0 {
        let r = (n - 1) % 26;
        s.insert(0, (b'A' + r as u8) as char);
        n = (n - 1) / 26;
    }
    format!("track-{s}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::Utc;

    fn node(id: &str, status: &str, priority: i32) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: status.into(),
            priority,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap_of(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let mut snap = GraphSnapshot::new(None);
        for node in nodes {
            snap.add_node(node);
        }
        for (from, to, kind) in edges {
            assert!(snap.add_edge(from, to, kind));
        }
        snap.data_hash = snap.compute_data_hash();
        snap
    }

    #[test]
    fn linear_chain_yields_three_tracks() {
        let s = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
            ],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        assert_eq!(p.plan.tracks.len(), 3);
        assert_eq!(p.plan.tracks[0].items[0].id, "a");
        assert_eq!(p.plan.tracks[1].items[0].id, "b");
        assert_eq!(p.plan.tracks[2].items[0].id, "c");
    }

    #[test]
    fn parallel_independent_items_share_track_zero() {
        let s = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
            ],
            vec![],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        assert_eq!(p.plan.tracks.len(), 1);
        assert_eq!(p.plan.tracks[0].items.len(), 3);
    }

    #[test]
    fn highest_impact_picks_max_unblocks() {
        let s = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
                node("d", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
                ("a", "d", DependencyKind::Blocks),
            ],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        assert_eq!(p.plan.summary.unwrap().highest_impact.as_deref(), Some("a"));
    }

    #[test]
    fn closed_parent_does_not_increase_depth() {
        let s = snap_of(
            vec![node("a", "closed", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        assert_eq!(p.plan.tracks.len(), 1);
        assert_eq!(p.plan.tracks[0].items[0].id, "b");
    }

    #[test]
    fn compute_plan_does_not_panic_on_open_cycle() {
        let s = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2)],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "a", DependencyKind::Blocks),
            ],
        );
        let cfg = ScoreConfig::default();
        let result = std::panic::catch_unwind(|| compute_plan(&s, &cfg));

        assert!(result.is_ok());
    }

    #[test]
    fn highest_impact_tie_breaks_to_smallest_id() {
        let s = snap_of(vec![node("b", "open", 2), node("a", "open", 2)], vec![]);
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);

        assert_eq!(p.plan.summary.unwrap().highest_impact.as_deref(), Some("a"));
    }
}
