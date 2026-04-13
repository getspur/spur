use spur_acp::{SessionId, SpurEvent};
use spur_core::{ExecutorId, ExecutorLineage, LifecycleState};

fn spawn(id: &str, parent: Option<&str>) -> SpurEvent {
    SpurEvent::ExecutorSpawned {
        id: id.into(),
        parent_id: parent.map(|s| s.into()),
        session_id: SessionId(format!("sess-{}", id)),
        agent: "kiro".into(),
        role: if parent.is_none() {
            "Brain".into()
        } else {
            "Executor".into()
        },
        task_spec: format!("task for {}", id),
    }
}

#[test]
fn spawn_creates_root_when_no_parent() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));

    assert_eq!(l.root_ids().len(), 1);
    let n = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert!(n.parent_id.is_none());
    assert_eq!(n.phase, LifecycleState::Spawning);
    assert_eq!(n.attempts.len(), 1);
}

#[test]
fn spawn_links_child_under_parent() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));
    l.apply(&spawn("worker-1", Some("brain-1")));

    assert_eq!(l.root_ids().len(), 1);
    let parent = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert_eq!(parent.child_ids.len(), 1);
    assert_eq!(parent.child_ids[0], ExecutorId::new("worker-1"));

    let child = l.node(&ExecutorId::new("worker-1")).unwrap();
    assert_eq!(child.parent_id, Some(ExecutorId::new("brain-1")));
}

#[test]
fn phase_change_updates_node_phase() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "brain-1".into(),
        phase: "Running".into(),
    });

    let n = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Running);
}

#[test]
fn phase_change_terminal_sets_attempt_ended() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Succeeded".into(),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    let a = n.current_attempt().unwrap();
    assert!(a.ended_at.is_some(), "terminal phase must close the attempt");
}

#[test]
fn unknown_phase_string_is_ignored() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Bogus".into(),
    });
    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.phase, LifecycleState::Spawning, "unchanged on unknown phase");
}
