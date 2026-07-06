#[cfg(test)]
mod agent_model_catalog_tests {
    use super::super::super::*;
    use spur_acp::agent_model_catalog::{cache_path, read};
    use spur_acp::types::{AgentKind, AgentRole, TransportKind};

    struct HomeEnvGuard {
        original: Option<std::ffi::OsString>,
    }

    impl HomeEnvGuard {
        fn set(home: &std::path::Path) -> Self {
            let original = std::env::var_os("HOME");
            std::env::set_var("HOME", home);
            Self { original }
        }
    }

    impl Drop for HomeEnvGuard {
        fn drop(&mut self) {
            if let Some(home) = self.original.take() {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn write_catalog_probe_agent_script(dir: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("catalog_probe_agent.sh");
        std::fs::write(
            &script,
            r#"#!/bin/bash
set -u

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{}},"authMethods":[]}}'
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"catalog-session","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"gpt-5","options":[{"value":"gpt-5","name":"GPT-5","description":"frontier"}]},{"id":"reasoning_effort","name":"Reasoning Effort","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"high","name":"High","description":"deep"}]}]}}'
            ;;
    esac
done
"#,
        )
        .expect("write catalog probe script");
        script
    }

    fn probe_config(name: &str, script: &std::path::Path) -> spur_acp::AgentConfig {
        let mut agent = spur_acp::AgentConfig::with_defaults(name);
        agent.command = "bash".to_string();
        agent.args = vec![script.display().to_string()];
        agent.transport = TransportKind::Acp;
        agent.kind = AgentKind::CodexAcp;
        agent.role = AgentRole::Worker;
        agent
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_model_catalog_probe_runs_in_background_and_reports_completion() {
        let home = tempfile::tempdir().expect("home tempdir");
        let _home = HomeEnvGuard::set(home.path());
        let repo = tempfile::tempdir().expect("repo tempdir");
        let script = write_catalog_probe_agent_script(repo.path());
        let mut config = spur_acp::SpurConfig::default();
        config.agents.entries = vec![probe_config("codex-probe", &script)];
        let mut app = App::new_with_config(
            None,
            false,
            std::sync::Arc::new(config),
            crate::landing::LandingDecision::ShowDashboard,
        );

        app.schedule_agent_model_catalog_probe_for_test("codex-probe".to_string());

        let action = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.background_action_rx.recv(),
        )
        .await
        .expect("background probe should report completion")
        .expect("background action channel should stay open");
        assert!(matches!(
            &action,
            crate::action::Action::AgentModelCatalogProbeCompleted { worker_name }
                if worker_name == "codex-probe"
        ));
        app.process_action(action);

        let catalog = read(&cache_path().expect("cache path")).expect("catalog should be written");
        let entry = catalog
            .entries
            .get("codex-probe")
            .expect("worker catalog entry");
        assert_eq!(entry.models[0].value, "gpt-5");
        assert_eq!(entry.efforts[0].value, "high");
        assert!(!app
            .agent_model_catalog_probes_in_flight
            .contains("codex-probe"));
    }
}
