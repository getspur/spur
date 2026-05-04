use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::components::execute_modal::{ExecuteModal, ExecuteModalVariant};

fn render_modal(modal: ExecuteModal) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();

    terminal
        .draw(|frame| {
            modal.render(frame, Rect::new(10, 5, 80, 14));
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let mut rows = Vec::with_capacity(buffer.area.height as usize);
    for y in 0..buffer.area.height {
        let mut row = String::new();
        for x in 0..buffer.area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        rows.push(row.trim_end().to_string());
    }
    rows.join("\n")
}

#[test]
fn confirm_variant_renders_execute_epic_prompt() {
    let rendered = render_modal(ExecuteModal {
        epic_id: "bd-plan".to_string(),
        epic_title: "Auth refactor — migrate to OIDC".to_string(),
        variant: ExecuteModalVariant::Confirm,
    });

    assert!(rendered.contains("Execute Item"), "{rendered}");
    assert!(rendered.contains("bd-plan"), "{rendered}");
    assert!(rendered.contains("Auth refactor"), "{rendered}");
    assert!(
        rendered.contains("This sends a prompt asking the brain to analyze"),
        "{rendered}"
    );
    assert!(rendered.contains("[Enter] confirm"), "{rendered}");
    assert!(rendered.contains("[Esc] cancel"), "{rendered}");
}

#[test]
fn already_executing_variant_renders_session_prompt() {
    let rendered = render_modal(ExecuteModal {
        epic_id: "bd-plan".to_string(),
        epic_title: "Auth refactor — migrate to OIDC".to_string(),
        variant: ExecuteModalVariant::AlreadyExecuting {
            plan_id: "PLN-abc".to_string(),
        },
    });

    assert!(rendered.contains("Already Executing"), "{rendered}");
    assert!(
        rendered.contains("Work item bd-plan is already executing."),
        "{rendered}"
    );
    assert!(rendered.contains("Plan-id: PLN-abc"), "{rendered}");
    assert!(rendered.contains("[s] View session"), "{rendered}");
    assert!(rendered.contains("[Esc] cancel"), "{rendered}");
}
