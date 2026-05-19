#[cfg(test)]
mod upgrade_banner_tests {
    use super::super::super::*;
    use crate::app::events::next_upgrade_result;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn upgrade_receiver_none_pending_future_is_inert() {
        let mut app = App::new_for_tests();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            next_upgrade_result(&mut app.upgrade_rx),
        )
        .await;

        assert!(
            result.is_err(),
            "missing upgrade receiver should park forever"
        );
        assert!(app.user_warning_for_test().is_none());
        assert!(app.upgrade_rx.is_none());
    }

    #[tokio::test]
    async fn upgrade_receiver_some_banner_warns_once_and_clears_receiver() {
        let (tx, rx) = oneshot::channel();
        let mut app = App::build_with_license_state(
            None,
            None,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            App::default_license_state(PLACEHOLDER_STATUS_TEXT),
            crate::landing::LandingDecision::ShowDashboard,
            None,
            Some(rx),
        );

        tx.send(Some(spur_core::UpgradeBanner {
            current: "1.0.0".into(),
            latest: "1.1.0".into(),
        }))
        .expect("receiver should still be held by app");

        let result = next_upgrade_result(&mut app.upgrade_rx).await;
        app.handle_upgrade_result(result);

        assert_eq!(
            app.user_warning_for_test(),
            Some("SPUR 1.1.0 is available; current 1.0.0. Run: spur upgrade")
        );
        assert!(app.upgrade_rx.is_none());

        app.dismiss_user_warning();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            next_upgrade_result(&mut app.upgrade_rx),
        )
        .await;

        assert!(result.is_err(), "cleared receiver should not refire");
        assert!(app.user_warning_for_test().is_none());
    }

    #[tokio::test]
    async fn upgrade_receiver_none_result_clears_without_warning() {
        let (tx, rx) = oneshot::channel();
        let mut app = App::build_with_license_state(
            None,
            None,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            App::default_license_state(PLACEHOLDER_STATUS_TEXT),
            crate::landing::LandingDecision::ShowDashboard,
            None,
            Some(rx),
        );

        tx.send(None).expect("receiver should still be held by app");

        let result = next_upgrade_result(&mut app.upgrade_rx).await;
        app.handle_upgrade_result(result);

        assert!(app.user_warning_for_test().is_none());
        assert!(app.upgrade_rx.is_none());
    }
}
