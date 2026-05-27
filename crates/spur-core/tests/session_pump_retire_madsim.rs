#![cfg(madsim)]

extern crate madsim_tokio as tokio;

use std::time::Duration;

use agent_client_protocol::schema::{
    AvailableCommand, AvailableCommandsUpdate, SessionId, SessionNotification, SessionUpdate,
};
use spur_acp::SpurEventBody;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

mod event_funnel {
    use spur_acp::SpurEventBody;

    #[derive(Clone)]
    pub struct FunnelHandle {
        tx: tokio::sync::mpsc::UnboundedSender<SpurEventBody>,
    }

    impl FunnelHandle {
        pub fn emit(&self, event: SpurEventBody) {
            let _ = self.tx.send(event);
        }
    }

    pub fn test_channel() -> (
        FunnelHandle,
        tokio::sync::mpsc::UnboundedReceiver<SpurEventBody>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (FunnelHandle { tx }, rx)
    }
}

#[path = "../src/notification_pump.rs"]
mod notification_pump;

#[test]
fn retire_grace_delivers_before_deadline_and_aborts_after_deadline() {
    madsim::runtime::Builder::from_env().run(|| async {
        if let Ok(seed) = std::env::var("MADSIM_TEST_SEED") {
            assert_eq!(
                madsim::runtime::Handle::current().seed(),
                seed.parse::<u64>().expect("MADSIM_TEST_SEED must be u64"),
            );
        }

        let (tx, _keep_open) = broadcast::channel(16);
        let (funnel, mut events) = event_funnel::test_channel();
        let pump = notification_pump::spawn_session_notification_pump(
            tx.subscribe(),
            spur_acp::SessionId("madsim-session".to_string()),
            funnel,
        );

        let before_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(99)).await;
            let _ = before_tx.send(notification("before-grace"));
        });

        let after_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(101)).await;
            let _ = after_tx.send(notification("after-grace"));
        });

        let started = tokio::time::Instant::now();
        retire_notification_pump_with_grace(pump).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed >= Duration::from_millis(100) && elapsed < Duration::from_millis(101),
            "retire grace should return at the bounded 100 ms deadline",
        );

        tokio::time::sleep(Duration::from_millis(2)).await;
        tokio::task::yield_now().await;

        let delivered = drain_command_names(&mut events);
        assert_eq!(
            delivered,
            vec!["before-grace"],
            "only notifications emitted before the 100 ms grace deadline should be delivered",
        );
    });
}

async fn retire_notification_pump_with_grace(h: JoinHandle<()>) {
    let abort = h.abort_handle();
    if tokio::time::timeout(Duration::from_millis(100), h)
        .await
        .is_err()
    {
        abort.abort();
    }
}

fn notification(name: &str) -> SessionNotification {
    SessionNotification::new(
        SessionId::new("madsim-session".to_string()),
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
            AvailableCommand::new(name, "test marker"),
        ])),
    )
}

fn drain_command_names(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<SpurEventBody>,
) -> Vec<String> {
    let mut delivered = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let SpurEventBody::AgentNotification { notification, .. } = event {
            delivered.push(command_name(&notification).to_string());
        }
    }
    delivered
}

fn command_name(notif: &SessionNotification) -> &str {
    match &notif.update {
        SessionUpdate::AvailableCommandsUpdate(update) => {
            update.available_commands[0].name.as_str()
        }
        other => panic!("expected AvailableCommandsUpdate, got {other:?}"),
    }
}
