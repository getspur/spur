use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::commands::submit_router::{route, SubmitDecision};
use spur_tui::commands::CommandRegistry;

#[test]
fn plain_text_routes_to_send() {
    let reg = CommandRegistry::new();
    let dec = route("hello world", &[], &[], &reg, false);
    match dec {
        SubmitDecision::Send { blocks, interrupt } => {
            assert_eq!(blocks.len(), 1);
            assert!(matches!(&blocks[0], ContentBlock::Text(t) if t.text == "hello world"));
            assert!(!interrupt);
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn spur_local_slash_dispatches_action() {
    let reg = CommandRegistry::new();
    let dec = route("/help", &[], &[], &reg, false);
    match dec {
        SubmitDecision::Local { action } => {
            assert!(matches!(action, Action::ShowHelp));
        }
        other => panic!("expected Local, got {:?}", other),
    }
}

fn prompt_text_entry(handle: &str, name: &str, desc: &str) -> spur_tui::commands::CommandEntry {
    let cfg = spur_acp::CommandsConfig {
        dispatch: spur_acp::DispatchKind::PromptText,
        ..Default::default()
    };
    spur_tui::agents::build_entry(handle, &cfg, &spur_acp::AvailableCommand::new(name, desc))
}

#[test]
fn agent_slash_becomes_text_block_stripped_of_prefix() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![prompt_text_entry("claude", "compact", "compact history")],
    );
    let dec = route("/compact please", &[], &[], &reg, false);
    match dec {
        SubmitDecision::Send { blocks, .. } => {
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                ContentBlock::Text(t) => assert_eq!(t.text, "/compact please"),
                other => panic!("expected text, got {:?}", other),
            }
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn explicit_prefix_claude_help_sends_bare_to_claude() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands("claude", vec![prompt_text_entry("claude", "help", "help")]);
    let dec = route("/claude:help", &[], &[], &reg, false);
    match dec {
        SubmitDecision::Send { blocks, .. } => match &blocks[0] {
            ContentBlock::Text(t) => assert_eq!(t.text, "/help"),
            other => panic!("got {:?}", other),
        },
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn interrupt_prefix_bang_is_preserved() {
    let reg = CommandRegistry::new();
    let dec = route("!stop now", &[], &[], &reg, true);
    match dec {
        SubmitDecision::Send { interrupt, blocks } => {
            assert!(interrupt);
            match &blocks[0] {
                ContentBlock::Text(t) => assert_eq!(t.text, "!stop now"),
                other => panic!("got {:?}", other),
            }
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn blocks_preview_roundtrips_text() {
    use spur_tui::commands::submit_router::blocks_preview;
    let blocks = vec![ContentBlock::Text(spur_acp::TextContent::new("hello"))];
    assert_eq!(blocks_preview(&blocks), "hello");
}
