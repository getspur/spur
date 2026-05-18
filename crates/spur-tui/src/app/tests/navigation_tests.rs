#[cfg(test)]
mod issue_browser_navigation_tests {
    use super::super::super::*;

    fn issue_summary(id: &str, title: &str) -> spur_acp::IssueSummaryEvent {
        spur_acp::IssueSummaryEvent {
            id: id.into(),
            source: "beads".into(),
            title: title.into(),
            status: "open".into(),
            labels: Vec::new(),
            priority: Some(1),
            issue_type: Some("bug".into()),
            assignee: None,
            description: None,
        }
    }

    #[test]
    fn navigate_to_issue_browser_seeds_from_dashboard_cache_and_refreshes_once() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.handle_spur_event(SpurEvent::now(SpurEventBody::IssuesLoaded {
            issues: vec![issue_summary("bd-1809", "IssueBrowser starts populated")],
        }));

        app.process_action(Action::NavigateTo(ViewId::IssueBrowser));

        let tracked = app
            .issue_browser
            .as_ref()
            .expect("navigation should lazily create IssueBrowser")
            .tracked_issues();
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].id, "bd-1809");
        assert_eq!(tracked[0].title, "IssueBrowser starts populated");

        match rx.try_recv() {
            Ok(UserInput::RefreshIssues) => {}
            Ok(_) => panic!("expected first IssueBrowser navigation to request RefreshIssues"),
            Err(err) => panic!("expected RefreshIssues after first navigation, got {err}"),
        }

        app.process_action(Action::NavigateTo(ViewId::Dashboard));
        app.process_action(Action::NavigateTo(ViewId::IssueBrowser));

        assert!(
            rx.try_recv().is_err(),
            "existing IssueBrowser should not request another refresh on navigation"
        );
    }
}

#[cfg(test)]
mod plan_browser_navigation_tests {
    use super::super::super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{PlanLifecycleEvent, PlanOwnerStateEvent, PlanSummaryEvent};

    fn plan_summary(plan_id: &str, owner_state: PlanOwnerStateEvent) -> PlanSummaryEvent {
        PlanSummaryEvent {
            plan_id: plan_id.into(),
            epic_id: format!("bd-{plan_id}"),
            title: format!("Plan {plan_id}"),
            source_body_preview: None,
            owner_state,
            lifecycle: PlanLifecycleEvent::Pending,
            counts: None,
            updated_at: None,
            created_at: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn wrap(body: SpurEventBody) -> SpurEvent {
        SpurEvent::now(body)
    }

    #[test]
    fn navigate_to_plan_browser_lazily_creates_and_refreshes_once() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        // Inc 1 (bd-d587.1): NavigateTo(PlanBrowser) requires an active session.
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("brain-1".into()),
        }));

        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert_eq!(app.current_view(), &ViewId::PlanBrowser);
        assert!(
            app.plan_browser.is_some(),
            "navigation should lazily create PlanBrowser"
        );
        match rx.try_recv() {
            Ok(UserInput::RefreshPlans) => {}
            Ok(_) => panic!("expected RefreshPlans, got different user input"),
            Err(err) => panic!("expected RefreshPlans after first navigation, got {err}"),
        }

        app.process_action(Action::NavigateTo(ViewId::Dashboard));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert!(
            rx.try_recv().is_err(),
            "existing PlanBrowser should not request another refresh on navigation"
        );
    }

    #[test]
    fn navigate_to_plan_browser_without_session_blocks_with_hint() {
        // Inc 1 (bd-d587.1): without an active brain session, opening PlanBrowser
        // would yield a list where no row can ever classify as Mine. We block-with-hint
        // instead of opening an empty browser.
        let mut app = App::new_for_tests();

        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert_eq!(
            app.current_view(),
            &ViewId::Dashboard,
            "navigation must be refused when no session is active"
        );
        assert!(
            app.plan_browser.is_none(),
            "PlanBrowser must not be created when navigation is refused"
        );
    }

    #[test]
    fn resume_plan_action_sends_user_input() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.process_action(Action::ResumePlan {
            plan_id: "plan-42".into(),
        });

        match rx.try_recv() {
            Ok(UserInput::ResumePlan { plan_id }) => assert_eq!(plan_id, "plan-42"),
            Ok(_) => panic!("expected ResumePlan, got different user input"),
            Err(err) => panic!("expected ResumePlan user input, got {err}"),
        }
    }

    #[test]
    fn claim_plan_action_sends_user_input() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.process_action(Action::ClaimPlan {
            plan_id: "plan-42".into(),
        });

        match rx.try_recv() {
            Ok(UserInput::ClaimPlan { plan_id }) => assert_eq!(plan_id, "plan-42"),
            Ok(_) => panic!("expected ClaimPlan, got different user input"),
            Err(err) => panic!("expected ClaimPlan user input, got {err}"),
        }
    }

    #[test]
    fn navigating_existing_plan_browser_updates_current_session() {
        // Inc 1 (bd-d587.1): seed an initial brain so the first navigation succeeds,
        // then assert that re-navigating after a session swap updates current_session
        // on the already-created PlanBrowser.
        let mut app = App::new_for_tests();
        let first = SessionId("brain-1".into());
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: first.clone(),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));
        assert_eq!(
            app.plan_browser
                .as_ref()
                .expect("PlanBrowser should exist")
                .current_session_for_test(),
            &first,
        );

        let second = SessionId("brain-2".into());
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: second.clone(),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        assert_eq!(
            app.plan_browser
                .as_ref()
                .expect("PlanBrowser should still exist")
                .current_session_for_test(),
            &second
        );
    }

    #[test]
    fn open_issue_in_backlog_navigates_and_fetches_detail() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.process_action(Action::OpenIssueInBacklog {
            id: "bd-plan-1".into(),
        });

        assert_eq!(app.current_view(), &ViewId::IssueBrowser);
        assert!(
            app.issue_browser.is_some(),
            "OpenIssueInBacklog should create IssueBrowser"
        );
        match rx.try_recv() {
            Ok(UserInput::GetIssueDetail { id }) => assert_eq!(id, "bd-plan-1"),
            Ok(_) => panic!("expected GetIssueDetail for backlog epic, got different user input"),
            Err(err) => panic!("expected GetIssueDetail for backlog epic, got {err}"),
        }
    }

    #[test]
    fn plan_browser_spur_events_route_to_view() {
        let mut app = App::new_for_tests();
        // Inc 1 (bd-d587.1): NavigateTo(PlanBrowser) requires an active session.
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("brain-1".into()),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));

        app.handle_spur_event(SpurEvent::now(SpurEventBody::PlansLoaded {
            plans: vec![plan_summary("plan-1", PlanOwnerStateEvent::Unowned)],
            warnings: Vec::new(),
        }));

        let plans = app
            .plan_browser
            .as_ref()
            .expect("PlanBrowser should exist")
            .plans();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].plan_id, "plan-1");
    }

    #[test]
    fn plan_browser_keys_bridge_refresh_claim_and_start_to_user_input() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        // Inc 1 (bd-d587.1): NavigateTo(PlanBrowser) requires an active session.
        app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("brain-1".into()),
        }));
        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));
        match rx.try_recv() {
            Ok(UserInput::RefreshPlans) => {}
            Ok(_) => panic!("expected initial RefreshPlans, got different user input"),
            Err(err) => panic!("expected initial RefreshPlans, got {err}"),
        }
        app.handle_spur_event(SpurEvent::now(SpurEventBody::PlansLoaded {
            plans: vec![plan_summary("plan-1", PlanOwnerStateEvent::Unowned)],
            warnings: Vec::new(),
        }));

        app.handle_crossterm_event_for_test(key(KeyCode::Char('r')));
        app.handle_crossterm_event_for_test(key(KeyCode::Char('c')));
        app.handle_crossterm_event_for_test(key(KeyCode::Enter));

        match rx.try_recv() {
            Ok(UserInput::RefreshPlans) => {}
            Ok(_) => panic!("expected RefreshPlans from r key, got different user input"),
            Err(err) => panic!("expected RefreshPlans from r key, got {err}"),
        }
        match rx.try_recv() {
            Ok(UserInput::ClaimPlan { plan_id }) => assert_eq!(plan_id, "plan-1"),
            Ok(_) => panic!("expected ClaimPlan from c confirm, got different user input"),
            Err(err) => panic!("expected ClaimPlan from c confirm, got {err}"),
        }
    }
}

/// Inc 2 (bd-d587.2): unit tests for the view_history stack semantics.
/// Drives `navigate_to` / `navigate_back` directly (not via Action arms)
/// so the invariants are tested in isolation from action-routing logic.
#[cfg(test)]
mod view_history_tests {
    use super::super::super::*;
    use spur_acp::SessionId;

    fn seed_session(app: &mut App, sid: &str) {
        app.handle_spur_event(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "test-brain".into(),
            session: SessionId(sid.into()),
        }));
    }

    #[test]
    fn navigate_to_pushes_leaving_view_then_back_pops_it() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        // BrainSpawned auto-navigated us into SessionDetail. Stack should be [Dashboard].
        assert_eq!(app.view_history, vec![ViewId::Dashboard]);

        app.navigate_to(ViewId::IssueBrowser);
        assert_eq!(app.current_view, ViewId::IssueBrowser);
        assert_eq!(
            app.view_history,
            vec![
                ViewId::Dashboard,
                ViewId::SessionDetail(SessionId("brain-1".into()))
            ],
        );

        app.navigate_back();
        assert_eq!(
            app.current_view,
            ViewId::SessionDetail(SessionId("brain-1".into()))
        );
        assert_eq!(app.view_history, vec![ViewId::Dashboard]);

        app.navigate_back();
        assert_eq!(app.current_view, ViewId::Dashboard);
        assert!(app.view_history.is_empty());
    }

    #[test]
    fn navigate_to_dashboard_clears_history() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        app.navigate_to(ViewId::IssueBrowser);
        app.navigate_to(ViewId::SessionPicker);
        assert!(app.view_history.len() >= 2);

        app.navigate_to(ViewId::Dashboard);

        assert_eq!(app.current_view, ViewId::Dashboard);
        assert!(
            app.view_history.is_empty(),
            "Dashboard is canonical root and must clear history"
        );
    }

    #[test]
    fn navigate_to_same_view_is_no_op() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        let history_before = app.view_history.clone();

        app.navigate_to(ViewId::SessionDetail(SessionId("brain-1".into())));

        assert_eq!(
            app.view_history, history_before,
            "navigate_to(current_view) must not push or mutate history"
        );
    }

    #[test]
    fn push_history_skips_duplicate_top() {
        let mut app = App::new_for_tests();
        app.view_history.push(ViewId::IssueBrowser);
        app.push_history(ViewId::IssueBrowser);
        assert_eq!(app.view_history, vec![ViewId::IssueBrowser]);
    }

    #[test]
    fn push_history_caps_at_max_evicting_oldest() {
        let mut app = App::new_for_tests();
        // Pre-fill exactly to the cap with a non-Dashboard, non-current view.
        for _ in 0..NAV_HISTORY_MAX {
            app.view_history.push(ViewId::IssueBrowser);
            // Defeat the no-dup-top guard by alternating — easier to use raw push for this test.
        }
        // Force overflow via the public API.
        app.push_history(ViewId::SessionPicker);

        assert_eq!(app.view_history.len(), NAV_HISTORY_MAX);
        assert_eq!(
            app.view_history.last(),
            Some(&ViewId::SessionPicker),
            "newest entry must remain at the top"
        );
    }

    #[test]
    fn navigate_back_from_dashboard_with_active_session_falls_back_to_session_detail() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        // Land back on Dashboard with empty history.
        app.navigate_to(ViewId::Dashboard);
        assert!(app.view_history.is_empty());
        assert_eq!(app.current_view, ViewId::Dashboard);

        app.navigate_back();

        assert_eq!(
            app.current_view,
            ViewId::SessionDetail(SessionId("brain-1".into())),
            "Dashboard back-with-empty-history returns to active session detail"
        );
    }

    #[test]
    fn navigate_back_from_dashboard_with_no_session_is_no_op() {
        let mut app = App::new_for_tests();
        assert_eq!(app.current_view, ViewId::Dashboard);
        assert!(app.view_history.is_empty());
        assert!(app.session_detail.is_none());

        app.navigate_back();

        assert_eq!(
            app.current_view,
            ViewId::Dashboard,
            "no session + empty history must not move the user anywhere"
        );
    }

    #[test]
    fn navigate_back_nulls_plan_inspector_overlay_state() {
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");
        app.process_action(Action::NavigateTo(ViewId::PlanInspector(SessionId(
            "brain-1".into(),
        ))));
        assert!(app.plan_inspector.is_some());

        app.process_action(Action::NavigateBack);

        assert!(
            app.plan_inspector.is_none(),
            "leaving PlanInspector via navigate_back must null the overlay state"
        );
    }

    #[test]
    fn end_to_end_dashboard_to_sprints_to_issue_browser_back_chain() {
        // Reproduces the user-reported flow: Dashboard \u2192 SessionDetail \u2192 PlanBrowser
        // \u2192 (e for view-epic) IssueBrowser \u2192 Esc must land back at PlanBrowser
        // (not Dashboard, which was the pre-Inc-2 bug).
        let mut app = App::new_for_tests();
        seed_session(&mut app, "brain-1");

        app.process_action(Action::NavigateTo(ViewId::PlanBrowser));
        assert_eq!(app.current_view, ViewId::PlanBrowser);

        app.process_action(Action::OpenIssueInBacklog {
            id: "bd-epic-1".into(),
        });
        assert_eq!(app.current_view, ViewId::IssueBrowser);

        // Drive a real Esc keystroke through the crossterm path so the
        // IssueBrowser view's own handler is exercised (not just the action
        // it should produce). This is the regression hook: previously the
        // view returned NavigateTo(Dashboard) here, which silently bypassed
        // view_history and skipped past PlanBrowser entirely.
        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.current_view,
            ViewId::PlanBrowser,
            "Esc from IssueBrowser must return to PlanBrowser, not Dashboard"
        );

        app.process_action(Action::NavigateBack);
        assert_eq!(
            app.current_view,
            ViewId::SessionDetail(SessionId("brain-1".into())),
            "Esc from PlanBrowser must return to SessionDetail"
        );
    }
}
