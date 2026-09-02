//! Verifies new executor lineage events round-trip through serde JSON.

use chrono::{TimeZone, Utc};
use spur_acp::{
    AgentKind, BrainInfo, Column, DatasourceEntry, DatasourceKind, IssueSummaryEvent,
    LoopDetailEvent, LoopRunRecordEvent, LoopSummaryEvent, PlanLifecycleEvent,
    PlanLoadWarningEvent, PlanLoopOriginEvent, PlanOwnerStateEvent, PlanSummaryCountsEvent,
    PlanSummaryEvent, ReviewDecision, ReviewKind, ReviewPayload, Role, SessionId, SpurAgentCaps,
    SpurEvent, SpurEventBody,
};

#[test]
fn dynamic_acp_evidence_failure_and_fallback_provenance_roundtrips() {
    use spur_acp::{InitializeResponse, NewSessionResponse, ProtocolVersion};

    let caps = SpurAgentCaps::new(
        &InitializeResponse::new(ProtocolVersion::LATEST),
        &NewSessionResponse::new("evidence-session"),
        AgentKind::Generic,
    );
    let mut encoded = serde_json::to_value(caps).expect("serialize baseline caps");
    encoded.as_object_mut().expect("caps object").insert(
        "capability_evidence".to_owned(),
        serde_json::json!({
            "epoch": {
                "id": 7,
                "identity": {
                    "resolved_executable": "/usr/bin/future-acp",
                    "upstream_version": "9.1.0",
                    "argv_fingerprint": "sha256:argv",
                    "environment_fingerprint": "sha256:env"
                },
                "records": [
                    {
                        "key": {"kind": "model", "upstream_id": "model"},
                        "claim": "candidate_observed",
                        "provenance": "vendor_advertisement",
                        "observed_at": 1,
                        "raw_digest": "sha256:candidate",
                        "session_scope": {"kind": "session", "id": "evidence-session"},
                        "choices": [{"id": "future-model", "label": "Future Model"}]
                    },
                    {
                        "key": {"kind": "model", "upstream_id": "model"},
                        "claim": "native_verified",
                        "provenance": "accepted_active_probe",
                        "observed_at": 2,
                        "raw_digest": "sha256:accepted",
                        "session_scope": {"kind": "isolated_probe"},
                        "choices": [{"id": "future-model", "label": "Future Model"}]
                    },
                    {
                        "key": {"kind": "model", "upstream_id": "model"},
                        "claim": "rejected",
                        "provenance": "rejected_active_probe",
                        "observed_at": 3,
                        "raw_digest": "sha256:rejected",
                        "session_scope": {"kind": "isolated_probe"},
                        "choices": []
                    },
                    {
                        "key": {"kind": "custom:failure", "upstream_id": "authentication"},
                        "claim": "inconclusive",
                        "provenance": "inconclusive_failure",
                        "observed_at": 4,
                        "raw_digest": "sha256:auth",
                        "session_scope": {"kind": "global"},
                        "choices": []
                    },
                    {
                        "key": {"kind": "custom:failure", "upstream_id": "timeout"},
                        "claim": "unknown",
                        "provenance": "inconclusive_failure",
                        "observed_at": 5,
                        "raw_digest": "sha256:timeout",
                        "session_scope": {"kind": "session", "id": "evidence-session"},
                        "choices": []
                    },
                    {
                        "key": {"kind": "custom:failure", "upstream_id": "malformed"},
                        "claim": "inconclusive",
                        "provenance": "inconclusive_failure",
                        "observed_at": 6,
                        "raw_digest": "sha256:malformed",
                        "session_scope": {"kind": "global"},
                        "choices": []
                    },
                    {
                        "key": {"kind": "command", "upstream_id": "session/prompt"},
                        "claim": "candidate_observed",
                        "provenance": "prompt_fallback",
                        "observed_at": 7,
                        "raw_digest": "sha256:prompt",
                        "session_scope": {"kind": "session", "id": "evidence-session"},
                        "choices": []
                    }
                ]
            },
            "reduced": [],
            "shadow_diffs": []
        }),
    );

    let round: SpurAgentCaps =
        serde_json::from_value(encoded).expect("evidence-bearing caps deserialize");
    let round = serde_json::to_value(round).expect("evidence-bearing caps reserialize");
    let evidence = &round["capability_evidence"];
    assert_eq!(evidence["completeness"], "incomplete");
    let records = evidence["epoch"]["records"]
        .as_array()
        .expect("evidence records must round-trip");

    assert!(records.iter().any(|record| {
        record["claim"] == "inconclusive"
            && record["provenance"] == "inconclusive_failure"
            && record["key"]["upstream_id"] == "authentication"
    }));
    assert!(records.iter().any(|record| {
        record["claim"] == "candidate_observed" && record["provenance"] == "prompt_fallback"
    }));
    assert!(records.iter().any(|record| {
        record["claim"] == "unknown"
            && record["provenance"] == "inconclusive_failure"
            && record["key"]["upstream_id"] == "timeout"
    }));
    assert!(records.iter().any(|record| {
        record["claim"] == "inconclusive"
            && record["provenance"] == "inconclusive_failure"
            && record["key"]["upstream_id"] == "malformed"
    }));
    assert!(evidence["reduced"].as_array().is_some_and(|reduced| {
        reduced.iter().any(|capability| {
            capability["key"]["upstream_id"] == "model"
                && capability["route"] == "prompt_only"
                && capability["sources"]["provenances"]
                    .as_array()
                    .is_some_and(|sources| {
                        sources
                            .iter()
                            .any(|source| source == "rejected_active_probe")
                    })
        })
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_acp_evidence_initialize_only_roundtrips_incomplete() {
    use spur_acp::connection::native::NativeAcpConnection;
    use spur_acp::connection::AgentConnection;
    use spur_acp::{InitializeRequest, NewSessionResponse, ProtocolVersion};

    const INITIALIZE_ONLY_AGENT: &str = r#"
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        result = {
            "protocolVersion": 1,
            "agentInfo": {"name": "initialize-only", "version": "1.0.0"},
            "agentCapabilities": {"promptCapabilities": {}},
            "authMethods": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": message.get("id"), "result": result}), flush=True)
"#;

    let mut connection = NativeAcpConnection::new_with_kind(
        "initialize-only",
        "python3",
        vec![
            "-u".to_owned(),
            "-c".to_owned(),
            INITIALIZE_ONLY_AGENT.to_owned(),
        ],
        AgentKind::Generic,
        None,
    );
    let initialize = connection
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("mock initialize");
    let caps = SpurAgentCaps::new(
        &initialize,
        &NewSessionResponse::new("session-not-started"),
        AgentKind::Generic,
    );
    let encoded = serde_json::to_value(caps).expect("serialize initialize-only evidence");
    assert_eq!(encoded["capability_evidence"]["completeness"], "incomplete");

    let round: SpurAgentCaps =
        serde_json::from_value(encoded).expect("initialize-only caps round-trip");
    let round = serde_json::to_value(round).expect("reserialize initialize-only caps");
    assert_eq!(round["capability_evidence"]["completeness"], "incomplete");

    let _ = connection.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_acp_evidence_captures_raw_frames_before_typed_projection() {
    use spur_acp::connection::native::NativeAcpConnection;
    use spur_acp::connection::AgentConnection;
    use spur_acp::{InitializeRequest, ProtocolVersion};

    const MOCK_AGENT: &str = r#"
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        result = {
            "protocolVersion": 1,
            "agentInfo": {"name": "future-acp", "version": "9.1.0"},
            "agentCapabilities": {"promptCapabilities": {}, "loadSession": True},
            "authMethods": [],
            "vendorPlane": {"apiToken": "must-not-survive"},
            "futureCapabilityPlane": {
                "availableFluxLevels": [{"id": "flux-9000", "label": "Flux 9000"}],
                "privateToken": "future-plane-secret"
            }
        }
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
    elif method == "session/new":
        notification = {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "raw-session",
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": [{"name": "future-command", "description": "future"}]
                }
            }
        }
        print(json.dumps(notification), flush=True)
        result = {
            "sessionId": "raw-session",
            "models": {
                "currentModelId": "future-model",
                "availableModels": [{
                    "modelId": "future-model",
                    "name": "Future Model",
                    "apiKey": "must-not-survive",
                    "_meta": {"reasoningEfforts": ["xhigh", "future-effort"]}
                }]
            }
        }
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
    elif method == "session/load":
        result = {
            "sessionId": "loaded-session",
            "models": {
                "currentModelId": "loaded-model",
                "availableModels": [{"modelId": "loaded-model", "name": "Loaded Model"}]
            }
        }
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#;

    let temp = tempfile::tempdir().expect("temp repo root");
    let mut connection = NativeAcpConnection::new_with_kind(
        "future-acp",
        "python3",
        vec!["-u".to_owned(), "-c".to_owned(), MOCK_AGENT.to_owned()],
        AgentKind::Generic,
        None,
    );
    connection.set_repo_root(temp.path().to_path_buf());

    let initialize = connection
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("mock initialize");
    let _session = connection
        .new_session(std::env::current_dir().expect("cwd"), Vec::new())
        .await
        .expect("mock session/new");
    let load = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        connection.load_session(spur_acp::LoadSessionRequest::new(
            "loaded-session".to_owned(),
            std::env::current_dir().expect("load cwd"),
        )),
    )
    .await
    .expect("mock session/load timeout")
    .expect("mock session/load");
    let (loaded, _) = load;
    let caps = SpurAgentCaps::from_loaded(&initialize, &loaded, AgentKind::Generic);
    let encoded = serde_json::to_value(&caps).expect("serialize evidence facade");
    let evidence = encoded["capability_evidence"].clone();
    assert_eq!(evidence["completeness"], "complete");
    let records = evidence["epoch"]["records"]
        .as_array()
        .expect("raw evidence records");

    assert!(records.iter().any(|record| {
        record["key"]["kind"] == "model"
            && record["choices"]
                .as_array()
                .is_some_and(|choices| choices.iter().any(|choice| choice["id"] == "future-model"))
            && record["provenance"] == "vendor_advertisement"
    }));
    assert!(records.iter().any(|record| {
        record["key"]["kind"] == "model"
            && record["choices"]
                .as_array()
                .is_some_and(|choices| choices.iter().any(|choice| choice["id"] == "loaded-model"))
            && record["provenance"] == "vendor_advertisement"
    }));
    assert!(records.iter().any(|record| {
        record["key"]["kind"] == "effort"
            && record["choices"]
                .as_array()
                .is_some_and(|choices| choices.iter().any(|choice| choice["id"] == "future-effort"))
    }));
    assert!(records.iter().any(|record| {
        record["key"]["kind"] == "command"
            && record["choices"].as_array().is_some_and(|choices| {
                choices
                    .iter()
                    .any(|choice| choice["id"] == "future-command")
            })
            && record["provenance"] == "observed_notification"
    }));
    assert!(records.iter().any(|record| {
        record["key"]["kind"] == "custom:unknown_acp_field"
            && record["key"]["upstream_id"] == "futureCapabilityPlane.availableFluxLevels"
            && record["claim"] == "unknown"
            && record["provenance"] == "vendor_advertisement"
    }));
    assert!(records.iter().all(|record| {
        record["raw_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:") && digest.len() == 71)
    }));
    assert!(!serde_json::to_string(&encoded)
        .expect("encode evidence")
        .contains("must-not-survive"));
    assert!(!serde_json::to_string(&encoded)
        .expect("encode evidence")
        .contains("future-plane-secret"));
    assert!(evidence["reduced"].as_array().is_some_and(|reduced| {
        reduced.iter().any(|capability| {
            capability["key"]["kind"] == "custom:unknown_acp_field"
                && capability["key"]["upstream_id"] == "futureCapabilityPlane.availableFluxLevels"
                && capability["route"] == "hidden"
        })
    }));
    assert!(evidence["shadow_diffs"].as_array().is_some_and(|diffs| {
        diffs.iter().any(|diff| {
            diff["key"]["kind"] == "model"
                && diff["legacy_route"] == "hidden"
                && diff["reduced_route"] == "prompt_only"
                && diff["unexplained"] == true
        })
    }));

    let round: SpurAgentCaps = serde_json::from_value(encoded).expect("caps round-trip");
    let round = serde_json::to_value(round).expect("reserialize caps");
    assert_eq!(round["capability_evidence"], evidence);

    let _ = connection.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_acp_evidence_later_overflow_roundtrips_incomplete() {
    use spur_acp::connection::native::NativeAcpConnection;
    use spur_acp::connection::AgentConnection;
    use spur_acp::{InitializeRequest, LoadSessionRequest, ProtocolVersion};

    const OVERFLOWING_AGENT: &str = r#"
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        result = {
            "protocolVersion": 1,
            "agentInfo": {"name": "overflowing-acp", "version": "1.0.0"},
            "agentCapabilities": {"promptCapabilities": {}, "loadSession": True},
            "authMethods": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
    elif method == "session/new":
        result = {
            "sessionId": "complete-before-overflow",
            "models": {
                "currentModelId": "conclusive-model",
                "availableModels": [{"modelId": "conclusive-model", "name": "Conclusive Model"}]
            }
        }
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
    elif method == "session/load":
        frames = []
        for index in range(4100):
            frames.append(json.dumps({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "overflowed-session",
                    "update": {
                        "sessionUpdate": "available_commands_update",
                        "availableCommands": [{"name": "command-" + str(index), "description": "bounded"}]
                    }
                }
            }))
        frames.append(json.dumps({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"sessionId": "overflowed-session"}
        }))
        sys.stdout.write("\n".join(frames) + "\n")
        sys.stdout.flush()
"#;

    let mut connection = NativeAcpConnection::new_with_kind(
        "overflowing-acp",
        "python3",
        vec![
            "-u".to_owned(),
            "-c".to_owned(),
            OVERFLOWING_AGENT.to_owned(),
        ],
        AgentKind::Generic,
        None,
    );
    let initialize = connection
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("mock initialize");
    let session = connection
        .new_session(std::env::current_dir().expect("cwd"), Vec::new())
        .await
        .expect("mock session/new");
    let complete = SpurAgentCaps::new(&initialize, &session, AgentKind::Generic);
    let complete = serde_json::to_value(complete).expect("serialize complete evidence");
    assert_eq!(complete["capability_evidence"]["completeness"], "complete");

    let (loaded, _) = connection
        .load_session(LoadSessionRequest::new(
            "overflowed-session".to_owned(),
            std::env::current_dir().expect("load cwd"),
        ))
        .await
        .expect("mock session/load after overflowing capture");
    assert!(connection.capability_evidence_overflowed());

    let caps = SpurAgentCaps::from_loaded(&initialize, &loaded, AgentKind::Generic);
    let encoded = serde_json::to_value(caps).expect("serialize overflowed evidence");
    let evidence = encoded["capability_evidence"].clone();
    assert_eq!(evidence["completeness"], "incomplete");
    assert!(evidence["epoch"]["records"]
        .as_array()
        .is_some_and(|records| records.iter().any(|record| {
            record["key"]["kind"] == "model"
                && record["choices"].as_array().is_some_and(|choices| {
                    choices
                        .iter()
                        .any(|choice| choice["id"] == "conclusive-model")
                })
        })));

    let round: SpurAgentCaps = serde_json::from_value(encoded).expect("overflowed caps round-trip");
    let round = serde_json::to_value(round).expect("reserialize overflowed caps");
    assert_eq!(round["capability_evidence"], evidence);

    let _ = connection.shutdown().await;
}

#[test]
fn dynamic_acp_evidence_shadows_current_grok_and_kiro_routes_without_drift() {
    use std::path::PathBuf;

    use spur_acp::capability_evidence::{
        CapabilityChoice, CapabilityKey, CapabilityKind, CliIdentity, DispatchRoute, EvidenceClaim,
        EvidenceEpoch, EvidenceEpochId, EvidenceProvenance, EvidenceRecord, EvidenceSessionScope,
        ObservationTime, RawEvidenceDigest,
    };
    use spur_acp::{InitializeResponse, NewSessionResponse, ProtocolVersion};

    let identity = CliIdentity {
        resolved_executable: PathBuf::from("/usr/bin/provider-acp"),
        upstream_version: Some("1.0.0".to_owned()),
        argv_fingerprint: "sha256:argv".to_owned(),
        environment_fingerprint: "sha256:env".to_owned(),
    };
    let epoch = |id| {
        EvidenceEpoch::new(
            EvidenceEpochId(id),
            identity.clone(),
            vec![EvidenceRecord {
                key: CapabilityKey {
                    kind: CapabilityKind::Model,
                    upstream_id: "model".to_owned(),
                },
                claim: EvidenceClaim::CandidateObserved,
                provenance: EvidenceProvenance::VendorAdvertisement,
                identity: identity.clone(),
                observed_at: ObservationTime(id),
                raw_digest: RawEvidenceDigest(format!("sha256:{id}")),
                session_scope: EvidenceSessionScope::Session("shadow-session".to_owned()),
                choices: vec![CapabilityChoice {
                    id: "provider-model".to_owned(),
                    label: "Provider Model".to_owned(),
                    description: None,
                }],
            }],
        )
        .expect("identity-bound evidence epoch")
    };

    let mut grok_initialize = InitializeResponse::new(ProtocolVersion::LATEST);
    grok_initialize.meta = serde_json::from_value(serde_json::json!({
        "modelState": {
            "currentModelId": "provider-model",
            "availableModels": [{"modelId": "provider-model", "name": "Provider Model"}]
        }
    }))
    .expect("Grok metadata");
    let mut grok = SpurAgentCaps::new(
        &grok_initialize,
        &NewSessionResponse::new("shadow-session"),
        AgentKind::Grok,
    );
    grok.apply_evidence_epoch(epoch(11), &identity);

    let kiro_initialize = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut kiro_session = NewSessionResponse::new("shadow-session");
    kiro_session.meta = serde_json::from_value(serde_json::json!({
        "spur.recoveredModels": {
            "currentModelId": "provider-model",
            "availableModels": [{"modelId": "provider-model", "name": "Provider Model"}]
        }
    }))
    .expect("Kiro metadata");
    let mut kiro = SpurAgentCaps::new(&kiro_initialize, &kiro_session, AgentKind::Kiro);
    kiro.apply_evidence_epoch(epoch(12), &identity);

    for caps in [&grok, &kiro] {
        assert!(caps.supports_direct_set_model());
        assert!(caps.capability_shadow_diffs().iter().any(|diff| {
            diff.key.kind == CapabilityKind::Model
                && diff.legacy_route == DispatchRoute::NativePreferred
                && diff.reduced_route == DispatchRoute::PromptOnly
                && diff.reason == "bounded legacy native fallback"
                && !diff.unexplained
        }));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_acp_evidence_native_rejection_does_not_resend_prompt_same_action() {
    use spur_acp::capability_evidence::{
        CapabilityKind, DispatchRoute, EvidenceClaim, EvidenceProvenance,
    };
    use spur_acp::connection::native::NativeAcpConnection;
    use spur_acp::connection::AgentConnection;
    use spur_acp::{
        AcpSessionId, AuthMethodId, AuthenticateRequest, InitializeRequest, LoadSessionRequest,
        ProtocolVersion,
    };

    const REJECTING_AGENT: &str = r#"
import json, sys
log_path = sys.argv[1]
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    with open(log_path, "a", encoding="utf-8") as log:
        log.write(method + "\n")
    request_id = message.get("id")
    if method == "initialize":
        result = {
            "protocolVersion": 1,
            "agentInfo": {"name": "grok-shadow", "version": "1.0.0"},
            "agentCapabilities": {"promptCapabilities": {}},
            "authMethods": [],
            "_meta": {
                "modelState": {
                    "currentModelId": "grok-model",
                    "availableModels": [{"modelId": "grok-model", "name": "Grok Model"}]
                }
            }
        }
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
    elif method == "session/new":
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": {"sessionId": "reject-session"}}), flush=True)
    elif method == "session/set_model":
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32602, "message": "rejected native model"}}), flush=True)
    elif method == "authenticate":
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32001, "message": "authentication required"}}), flush=True)
    elif method == "session/load":
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32000, "message": "request timeout"}}), flush=True)
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": {}}), flush=True)
"#;

    let temp = tempfile::tempdir().expect("temp repo root");
    let method_log = temp.path().join("methods.log");
    let mut connection = NativeAcpConnection::new_with_kind(
        "grok-shadow",
        "python3",
        vec![
            "-u".to_owned(),
            "-c".to_owned(),
            REJECTING_AGENT.to_owned(),
            method_log.display().to_string(),
        ],
        AgentKind::Grok,
        None,
    );
    connection.set_repo_root(temp.path().to_path_buf());

    let initialize = connection
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("mock initialize");
    let session = connection
        .new_session(std::env::current_dir().expect("cwd"), Vec::new())
        .await
        .expect("mock session/new");
    let mut caps = SpurAgentCaps::new(&initialize, &session, AgentKind::Grok);

    let error = connection
        .set_session_model(
            AcpSessionId::new("reject-session"),
            "grok-model".to_owned(),
            &caps,
        )
        .await
        .expect_err("native model rejection must surface");
    assert!(error.to_string().contains("rejected native model"));

    let auth_error = connection
        .authenticate(AuthenticateRequest::new(AuthMethodId::new("test")))
        .await
        .expect_err("authentication failure must surface");
    assert!(auth_error.to_string().contains("authentication required"));
    let load_error = match connection
        .load_session(LoadSessionRequest::new(
            "timeout-session".to_owned(),
            std::env::current_dir().expect("load cwd"),
        ))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("timeout failure must surface"),
    };
    assert!(load_error.to_string().contains("request timeout"));

    let methods = std::fs::read_to_string(&method_log).expect("method log");
    assert_eq!(methods.matches("session/set_model\n").count(), 1);
    assert!(!methods.contains("session/set_config_option"));
    assert!(!methods.contains("session/prompt"));

    let epoch = connection
        .capability_evidence_epoch()
        .expect("live evidence epoch");
    let identity = epoch.identity().clone();
    caps.apply_evidence_epoch(epoch, &identity);
    assert!(caps.reduced_capabilities().iter().any(|reduced| {
        reduced.key.kind == CapabilityKind::Model
            && reduced.route == DispatchRoute::PromptOnly
            && reduced.sources.record_count >= 2
    }));
    assert!(caps
        .capability_evidence
        .as_ref()
        .expect("snapshot")
        .epoch()
        .records()
        .iter()
        .any(|record| {
            record.key.kind == CapabilityKind::Model && record.claim == EvidenceClaim::NativeFailed
        }));
    let records = caps
        .capability_evidence
        .as_ref()
        .expect("snapshot")
        .epoch()
        .records();
    assert!(records.iter().any(|record| {
        record.key.upstream_id == "authentication"
            && record.claim == EvidenceClaim::Inconclusive
            && record.provenance == EvidenceProvenance::InconclusiveFailure
    }));
    assert!(records.iter().any(|record| {
        record.key.upstream_id == "timeout"
            && record.claim == EvidenceClaim::Unknown
            && record.provenance == EvidenceProvenance::InconclusiveFailure
    }));

    let _ = connection.shutdown().await;
}

#[test]
fn executor_phase_changed_rejects_invalid_variant() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"ExecutorPhaseChanged": {"id": "x", "phase": "running"}}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "lowercase 'running' must fail to deserialize"
    );
}

#[test]
fn executor_spawned_rejects_invalid_role() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"ExecutorSpawned": {
            "id": "x", "parent_id": null,
            "session_id": "s",
            "agent": "a", "role": "brain", "task_spec": ""
        }}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "lowercase 'brain' must fail to deserialize"
    );
}

#[test]
fn executor_spawned_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: Some("brain-1".into()),
        session_id: SessionId("s1".into()),
        agent: "worker".into(),
        role: Role::Executor,
        task_spec: "fix bug".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::ExecutorSpawned { .. }));
}

#[test]
fn executor_review_resolved_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
        id: "exec-1".into(),
        decision: ReviewDecision::Reject {
            reason: "tests fail".into(),
        },
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::ExecutorReviewResolved { .. }
    ));
}

#[test]
fn executor_review_requested_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "exec-1".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::ExecutorReviewRequested { .. }
    ));
}

#[test]
fn executor_review_requested_carries_attempt_n() {
    use spur_acp::{ReviewKind, ReviewPayload, SpurEvent, SpurEventBody};
    let body = SpurEventBody::ExecutorReviewRequested {
        id: "exec-1".into(),
        attempt_n: 2,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    };
    let event = SpurEvent::now(body);
    let j = serde_json::to_value(&event).unwrap();
    assert_eq!(j["body"]["ExecutorReviewRequested"]["attempt_n"], 2);
    let _back: SpurEvent = serde_json::from_value(j).expect("round-trip");
}

#[test]
fn executor_review_cancelled_round_trips() {
    use spur_acp::{SpurEvent, SpurEventBody};
    let body = SpurEventBody::ExecutorReviewCancelled {
        id: "exec-1".into(),
        reason: "brain call cancelled".into(),
    };
    let event = SpurEvent::now(body);
    let j = serde_json::to_string(&event).expect("serialize");
    let _back: SpurEvent = serde_json::from_str(&j).expect("round-trip");
    assert!(j.contains("ExecutorReviewCancelled"));
    assert!(j.contains("brain call cancelled"));
}

#[test]
fn worker_notification_roundtrips() {
    use spur_acp::{ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent};

    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new("thinking...")));
    let notification =
        SessionNotification::new("acp-sess", SessionUpdate::AgentThoughtChunk(chunk));
    let ev = SpurEvent::now(SpurEventBody::WorkerNotification {
        brain_session_id: SessionId("brain-1".into()),
        executor_id: "exec-1".into(),
        notification: Box::new(notification),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::WorkerNotification { .. }
    ));
    assert!(json.contains("WorkerNotification"));
    assert!(json.contains("thinking..."));
}

#[test]
fn prompt_response_roundtrips_usage() {
    use spur_acp::{PromptResponse, StopReason, Usage};

    let response = PromptResponse::new(StopReason::EndTurn).usage(
        Usage::new(123, 45, 78)
            .thought_tokens(9)
            .cached_read_tokens(10)
            .cached_write_tokens(11),
    );

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"usage\""));
    assert!(json.contains("\"totalTokens\":123"));
    assert!(json.contains("\"thoughtTokens\":9"));

    let round: PromptResponse = serde_json::from_str(&json).unwrap();
    let usage = round.usage.expect("usage should round-trip");
    assert_eq!(usage.total_tokens, 123);
    assert_eq!(usage.input_tokens, 45);
    assert_eq!(usage.output_tokens, 78);
    assert_eq!(usage.thought_tokens, Some(9));
    assert_eq!(usage.cached_read_tokens, Some(10));
    assert_eq!(usage.cached_write_tokens, Some(11));
}

#[test]
fn issue_updated_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::IssueUpdated {
        source: "beads".into(),
        id: "BEADS-123".into(),
        status: Some("in_progress".into()),
        assignee: Some("alice".into()),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::IssueUpdated { .. }));
}

#[test]
fn issue_created_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::IssueCreated {
        issue: IssueSummaryEvent {
            id: "BEADS-124".into(),
            source: "beads".into(),
            title: "Add issue-created ACP event".into(),
            status: "open".into(),
            labels: vec!["feature".into()],
            priority: Some(2),
            issue_type: Some("task".into()),
            assignee: Some("alice".into()),
            description: Some("Track one created issue on ACP stream".into()),
        },
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::IssueCreated { .. }));
}

#[test]
fn datasources_changed_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::DatasourcesChanged {
        session: SessionId("brain-data".into()),
        entries: vec![DatasourceEntry {
            name: "sales".into(),
            path: "/tmp/sales.csv".into(),
            kind: DatasourceKind::Csv,
            group: Some("quarterly".into()),
            columns: vec![Column {
                name: "region".into(),
                sql_type: "VARCHAR".into(),
            }],
            row_count: Some(2),
            tables: Vec::new(),
        }],
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    assert!(matches!(
        round.body,
        SpurEventBody::DatasourcesChanged { .. }
    ));
    assert!(json.contains("DatasourcesChanged"));
    assert!(json.contains("sales"));
}

#[test]
fn plan_snapshot_updated_roundtrips() {
    use spur_acp::{
        PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent, SpurEventBody,
    };

    let ev = SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "p-123".into(),
            epic_id: None,
            status: "running".into(),
            progress: "1/3 reviewed, 1 running, 1 pending".into(),
            next_action: "Workers still running. Poll get_plan_status to monitor.".into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                pending: 1,
                ready: 0,
                dispatched: 1,
                awaiting_review: 1,
                approved: 0,
                rejected: 0,
                failed: 0,
                cancelled: 0,
                escalated: 0,
                auto_retried: 0,
            },
            tasks: vec![PlanSnapshotTask {
                task_id: "task-projection".into(),
                task_name: "Build PlanProjection".into(),
                agent: "claude-code".into(),
                issue_id: Some("BEADS-42".into()),
                issue_title: None,
                status: "awaiting_review".into(),
                attempt: 1,
                max_attempts: 3,
                depends_on: vec!["task-contract".into()],
                blocked_by: Vec::new(),
                unblocks: vec!["task-app".into()],
                summary: Some("projects plan status into UI".into()),
                feedback: None,
                error: None,
                worker_branch: Some("spur/worker-123".into()),
                delegation_id: Some("del-123".into()),
                diff_summary: None,
                mutation_id: None,
                superseded_by: Vec::new(),
                next_action: "review".into(),
            }],
            owner_brain_session_id: None,
            owner_token: None,
            owner_acquired_at: None,
        }),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::PlanSnapshotUpdated { .. }
    ));
}

#[test]
fn plan_snapshot_updated_rejects_malformed_payload() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanSnapshotUpdated": {
            "session_id": "brain-1",
            "snapshot": {
                "status": "running",
                "progress": "1/3 reviewed, 1 running, 1 pending",
                "next_action": "review",
                "ready_to_merge": false,
                "counts": {
                    "pending": 1,
                    "ready": 0,
                    "dispatched": 1,
                    "awaiting_review": 1,
                    "approved": 0,
                    "rejected": 0,
                    "failed": 0,
                    "cancelled": 0
                },
                "tasks": []
            }
        }}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "missing required plan_id must fail to deserialize"
    );
}

#[test]
fn plan_snapshot_deserializes_without_owner_fields_for_backward_compat() {
    // Pre-feature snapshots persisted in NDJSON event logs (~/.kiro/sessions/cli/*.jsonl)
    // omit the owner_* fields entirely. They must continue to deserialize cleanly,
    // with the owner_* fields defaulting to None.
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanSnapshotUpdated": {
            "session_id": "brain-1",
            "snapshot": {
                "plan_id": "plan-pre-feature",
                "status": "running",
                "progress": "0/1 done",
                "next_action": "wait",
                "ready_to_merge": false,
                "counts": {
                    "pending": 1,
                    "ready": 0,
                    "dispatched": 0,
                    "awaiting_review": 0,
                    "approved": 0,
                    "rejected": 0,
                    "failed": 0,
                    "cancelled": 0
                },
                "tasks": []
            }
        }}
    }"#;
    let event: SpurEvent = serde_json::from_str(json)
        .expect("pre-feature PlanSnapshot without owner fields must deserialize");
    let spur_acp::SpurEventBody::PlanSnapshotUpdated { snapshot, .. } = event.body else {
        panic!("expected PlanSnapshotUpdated body");
    };
    assert_eq!(snapshot.plan_id, "plan-pre-feature");
    assert!(snapshot.owner_brain_session_id.is_none());
    assert!(snapshot.owner_token.is_none());
    assert!(snapshot.owner_acquired_at.is_none());
}

#[test]
fn plan_task_failed_roundtrips_from_json() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanTaskFailed": {
            "plan_id": "plan-1",
            "task_id": "task-1",
            "attempt": 2,
            "max_attempts": 3,
            "error": "worker failed",
            "delegation_id": "del-1"
        }}
    }"#;

    let event: SpurEvent = serde_json::from_str(json).expect("PlanTaskFailed must deserialize");
    let encoded = serde_json::to_value(&event).expect("serialize PlanTaskFailed");
    assert_eq!(
        encoded["body"]["PlanTaskFailed"]["plan_id"],
        serde_json::json!("plan-1")
    );
    let _round: SpurEvent = serde_json::from_value(encoded).expect("round-trip PlanTaskFailed");
}

#[test]
fn plan_task_awaiting_review_roundtrips_from_json() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"PlanTaskAwaitingReview": {
            "plan_id": "plan-1",
            "task_id": "task-1",
            "delegation_id": "del-1"
        }}
    }"#;

    let event: SpurEvent =
        serde_json::from_str(json).expect("PlanTaskAwaitingReview must deserialize");
    let encoded = serde_json::to_value(&event).expect("serialize PlanTaskAwaitingReview");
    assert_eq!(
        encoded["body"]["PlanTaskAwaitingReview"]["plan_id"],
        serde_json::json!("plan-1")
    );
    let _round: SpurEvent =
        serde_json::from_value(encoded).expect("round-trip PlanTaskAwaitingReview");
}

#[test]
fn issue_subgraph_loaded_roundtrips() {
    use spur_acp::{GraphEdgeEvent, GraphNodeEvent, SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::IssueSubgraphLoaded {
        requested_id: "bd-1".into(),
        nodes: vec![GraphNodeEvent {
            id: "bd-1".into(),
            title: Some("Root issue".into()),
            status: Some("open".into()),
            priority: Some(1),
            labels: vec!["epic".into()],
            pagerank: Some(0.9),
        }],
        edges: vec![GraphEdgeEvent {
            from: "bd-1".into(),
            to: "bd-2".into(),
            edge_type: Some("blocks".into()),
        }],
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::IssueSubgraphLoaded { .. }
    ));
    assert!(json.contains("IssueSubgraphLoaded"));
    assert!(json.contains("Root issue"));
}

#[test]
fn issue_command_error_with_id_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::IssueCommandError {
        operation: "GetIssueGraph".into(),
        error: "bv failed".into(),
        id: Some("bd-root".into()),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::IssueCommandError {
            operation,
            error,
            id,
        } => {
            assert_eq!(operation, "GetIssueGraph");
            assert_eq!(error, "bv failed");
            assert_eq!(id, Some("bd-root".into()));
        }
        other => panic!("expected IssueCommandError, got {other:?}"),
    }
}

#[test]
fn plans_loaded_roundtrips_plan_summary_contract() {
    let ev = SpurEvent::now(SpurEventBody::PlansLoaded {
        plans: vec![
            PlanSummaryEvent {
                plan_id: "plan-a1".into(),
                epic_id: "bd-120".into(),
                title: "Auth migration".into(),
                source_body_preview: Some("Move auth persistence behind the new adapter.".into()),
                owner_state: PlanOwnerStateEvent::Mine,
                lifecycle: PlanLifecycleEvent::Running,
                loop_origin: Some(PlanLoopOriginEvent {
                    loop_id: "loop-daily-triage".into(),
                    generation: 4,
                }),
                counts: Some(PlanSummaryCountsEvent {
                    total: 7,
                    pending: 1,
                    ready: 2,
                    running: 1,
                    awaiting_review: 1,
                    approved: 2,
                    rejected: 0,
                    failed: 0,
                    cancelled: 0,
                }),
                updated_at: Some(Utc.with_ymd_and_hms(2026, 5, 2, 10, 0, 0).unwrap()),
                created_at: Some(Utc.with_ymd_and_hms(2026, 5, 1, 9, 30, 0).unwrap()),
            },
            PlanSummaryEvent {
                plan_id: "plan-c3".into(),
                epic_id: "bd-130".into(),
                title: "Owned elsewhere".into(),
                source_body_preview: None,
                owner_state: PlanOwnerStateEvent::Other {
                    owner: "other-brain".into(),
                },
                lifecycle: PlanLifecycleEvent::AwaitingReview,
                loop_origin: None,
                counts: None,
                updated_at: None,
                created_at: None,
            },
            PlanSummaryEvent {
                plan_id: "plan-d4".into(),
                epic_id: "bd-140".into(),
                title: "Ambiguous ownership".into(),
                source_body_preview: None,
                owner_state: PlanOwnerStateEvent::Ambiguous {
                    owners: vec!["brain-a".into(), "brain-b".into()],
                },
                lifecycle: PlanLifecycleEvent::Unknown,
                loop_origin: None,
                counts: None,
                updated_at: None,
                created_at: None,
            },
        ],
        warnings: vec![PlanLoadWarningEvent {
            plan_id: "plan-a1".into(),
            canonical_epic_id: Some("bd-120".into()),
            stale_epic_ids: vec!["bd-stale".into()],
            canonical_owner_state: Some(PlanOwnerStateEvent::Mine),
            message: "Plan plan-a1 has duplicate stale epic bd-stale; using canonical epic bd-120."
                .into(),
        }],
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::PlansLoaded { plans, warnings } => {
            assert_eq!(plans.len(), 3);
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].plan_id, "plan-a1");
            assert_eq!(warnings[0].canonical_epic_id.as_deref(), Some("bd-120"));
            assert_eq!(warnings[0].stale_epic_ids, vec!["bd-stale"]);
            assert!(matches!(
                warnings[0].canonical_owner_state,
                Some(PlanOwnerStateEvent::Mine)
            ));
            assert!(matches!(plans[0].owner_state, PlanOwnerStateEvent::Mine));
            assert!(matches!(
                plans[1].owner_state,
                PlanOwnerStateEvent::Other { .. }
            ));
            assert!(matches!(
                plans[2].owner_state,
                PlanOwnerStateEvent::Ambiguous { .. }
            ));
            assert_eq!(plans[0].counts.as_ref().unwrap().total, 7);
            assert_eq!(
                plans[0]
                    .loop_origin
                    .as_ref()
                    .map(|origin| { (origin.loop_id.as_str(), origin.generation) }),
                Some(("loop-daily-triage", 4))
            );
            assert!(plans[1].loop_origin.is_none());
            assert!(plans[2].loop_origin.is_none());
        }
        other => panic!("expected PlansLoaded, got {other:?}"),
    }
}

#[test]
fn plans_loaded_deserializes_without_loop_origin_for_backward_compat() {
    let json = serde_json::json!({
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {
            "PlansLoaded": {
                "plans": [{
                    "plan_id": "plan-legacy",
                    "epic_id": "bd-legacy",
                    "title": "Legacy plan summary",
                    "owner_state": "Mine",
                    "lifecycle": "Running"
                }],
                "warnings": []
            }
        }
    });

    let round: SpurEvent = serde_json::from_value(json).unwrap();

    match round.body {
        SpurEventBody::PlansLoaded { plans, warnings } => {
            assert_eq!(plans.len(), 1);
            assert!(warnings.is_empty());
            assert!(plans[0].loop_origin.is_none());
        }
        other => panic!("expected PlansLoaded, got {other:?}"),
    }
}

#[test]
fn plan_command_error_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::PlanCommandError {
        operation: "ResumePlan".into(),
        plan_id: Some("plan-b2".into()),
        error: "resume_plan is not supported by this backend".into(),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::PlanCommandError {
            operation,
            plan_id,
            error,
        } => {
            assert_eq!(operation, "ResumePlan");
            assert_eq!(plan_id, Some("plan-b2".into()));
            assert_eq!(error, "resume_plan is not supported by this backend");
        }
        other => panic!("expected PlanCommandError, got {other:?}"),
    }
}

#[test]
fn agent_config_update_result_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::AgentConfigUpdateResult {
        name: "codex".into(),
        ok: false,
        message: "additional_directories entry is not absolute".into(),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::AgentConfigUpdateResult { name, ok, message } => {
            assert_eq!(name, "codex");
            assert!(!ok);
            assert_eq!(message, "additional_directories entry is not absolute");
        }
        other => panic!("expected AgentConfigUpdateResult, got {other:?}"),
    }
}

#[test]
fn config_update_result_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::ConfigUpdateResult {
        section: "graph".into(),
        ok: false,
        message: "unsupported embedding model alias 'not-a-model'".into(),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    match round.body {
        SpurEventBody::ConfigUpdateResult {
            section,
            ok,
            message,
        } => {
            assert_eq!(section, "graph");
            assert!(!ok);
            assert_eq!(message, "unsupported embedding model alias 'not-a-model'");
        }
        other => panic!("expected ConfigUpdateResult, got {other:?}"),
    }
}

#[test]
fn loop_observability_events_roundtrip_with_bounded_payloads() {
    let loops_loaded = SpurEvent::now(SpurEventBody::LoopsLoaded {
        loops: vec![LoopSummaryEvent {
            loop_id: "loop-daily-triage".into(),
            issue_id: "bd-loop".into(),
            title: "Daily triage loop".into(),
            autonomy: Some("l2".into()),
            paused: false,
            retired: false,
            backoff_active: true,
            cadence_secs: 3600,
            effective_interval_secs: 7200,
            next_run: Some(1_783_036_800),
            last_generation: Some(7),
            last_outcome: Some("partial".into()),
            last_cost_micros: Some(42_000),
            consecutive_failures: 2,
            goal_preview: Some("Keep the issue queue under control.".into()),
            updated_at: Some(Utc.with_ymd_and_hms(2026, 5, 3, 10, 15, 0).unwrap()),
        }],
        warnings: vec!["Loop list truncated at 200 rows.".into()],
    });

    let encoded = serde_json::to_value(&loops_loaded).unwrap();
    assert_eq!(
        encoded["body"]["LoopsLoaded"]["loops"][0]["effective_interval_secs"],
        serde_json::json!(7200)
    );
    assert_eq!(
        encoded["body"]["LoopsLoaded"]["loops"][0]["goal_preview"],
        serde_json::json!("Keep the issue queue under control.")
    );
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, loops_loaded.body);

    let detail_loaded = SpurEvent::now(SpurEventBody::LoopDetailLoaded {
        detail: LoopDetailEvent {
            loop_id: "loop-daily-triage".into(),
            issue_id: "bd-loop".into(),
            title: "Daily triage loop".into(),
            goal_preview: Some("Keep the issue queue under control.".into()),
            cadence_secs: 3600,
            effective_interval_secs: 7200,
            backoff_active: true,
            paused: false,
            next_run: Some(1_783_036_800),
            consecutive_failures: 2,
            budget_micros_per_generation: Some(100_000),
            max_generations_per_day: Some(8),
            max_tasks: Some(12),
            recent_runs: vec![LoopRunRecordEvent {
                generation: 7,
                outcome: "partial".into(),
                cost_micros: 42_000,
                autonomy: Some("l2".into()),
            }],
        },
    });

    let encoded = serde_json::to_value(&detail_loaded).unwrap();
    assert_eq!(
        encoded["body"]["LoopDetailLoaded"]["detail"]["recent_runs"][0]["generation"],
        serde_json::json!(7)
    );
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, detail_loaded.body);

    let command_error_with_loop = SpurEvent::now(SpurEventBody::LoopCommandError {
        operation: "PauseLoop".into(),
        loop_id: Some("loop-daily-triage".into()),
        error: "loop issue not found".into(),
    });
    let encoded = serde_json::to_value(&command_error_with_loop).unwrap();
    assert_eq!(
        encoded["body"]["LoopCommandError"]["loop_id"],
        serde_json::json!("loop-daily-triage")
    );
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, command_error_with_loop.body);

    let command_error_without_loop = SpurEvent::now(SpurEventBody::LoopCommandError {
        operation: "RefreshLoops".into(),
        loop_id: None,
        error: "backend unavailable".into(),
    });
    let encoded = serde_json::to_value(&command_error_without_loop).unwrap();
    let payload = encoded["body"]["LoopCommandError"]
        .as_object()
        .expect("loop command error payload");
    assert!(!payload.contains_key("loop_id"));
    let round: SpurEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(round.body, command_error_without_loop.body);
}

#[test]
fn loop_events_roundtrip_with_bounded_payloads() {
    let cases = [
        SpurEventBody::LoopArmed {
            loop_id: "loop-daily-triage".into(),
            generation: 7,
            next_run: 1_783_036_800,
        },
        SpurEventBody::LoopGenerationStarted {
            loop_id: "loop-daily-triage".into(),
            generation: 7,
            plan_id: "plan-7".into(),
        },
        SpurEventBody::LoopRunRecorded {
            loop_id: "loop-daily-triage".into(),
            generation: 7,
            outcome: "partial".into(),
            cost_micros: 42_000,
        },
        SpurEventBody::LoopPaused {
            loop_id: "loop-daily-triage".into(),
            by: "auto_paused".into(),
        },
    ];

    for body in cases {
        let event = SpurEvent::now(body);
        let encoded = serde_json::to_value(&event).expect("serialize loop event");
        let round: SpurEvent =
            serde_json::from_value(encoded.clone()).expect("round-trip loop event");
        assert_eq!(round.body, event.body);

        let payload = encoded["body"].as_object().expect("body object");
        let payload = payload.values().next().expect("loop event payload");
        assert_eq!(payload["loop_id"], serde_json::json!("loop-daily-triage"));
    }
}

#[test]
fn plan_completed_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanCompleted {
        plan_id: "p1".into(),
        approved: 3,
        rejected: 1,
        failed: 0,
        cancelled: 0,
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanCompleted { .. }));
}

#[test]
fn plan_ready_to_merge_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanReadyToMerge {
        plan_id: "p1".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanReadyToMerge { .. }));
}

#[test]
fn plan_pending_sweep_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanPendingSweep {
        plan_id: Some("p1".into()),
        epic_id: "bd-epic".into(),
        action: "quarantined".into(),
        child_count: 2,
        age_secs: 3601,
        reason: "stale pending plan exceeded grace".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanPendingSweep { .. }));
    assert!(json.contains("PlanPendingSweep"));
    assert!(json.contains("quarantined"));
}

#[test]
fn dispatch_lease_expired_roundtrips() {
    use spur_acp::{SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::DispatchLeaseExpired {
        plan_id: "p1".into(),
        task_id: "t1".into(),
        issue_id: "bd-1".into(),
        delegation_id: "del-A".into(),
        expired_at: 1_777_777_777,
        age_secs: 42,
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::DispatchLeaseExpired { .. }
    ));
    assert!(json.contains("DispatchLeaseExpired"));
}

#[test]
fn plan_task_blocked_on_setup_conflict_roundtrips() {
    use spur_acp::domain::continuation::SetupConflictTopology;
    use spur_acp::domain::events::DiffSummary;
    use spur_acp::{SpurEvent, SpurEventBody};

    let topology = SetupConflictTopology {
        base_oid: "2779409d".into(),
        blocked_task_id: "task-9".into(),
        conflict_dep_task_id: "task-7".into(),
        conflict_files: vec!["src/main.rs".into()],
        approved_chain: vec![spur_acp::domain::continuation::ApprovedTaskGitNode {
            task_id: "task-5".into(),
            worker_branch: "spur/worker/v2/codex/owner/task-5".into(),
            tip_oid: "b786d770".into(),
            parent_oid: "2779409d".into(),
            cumulative_diff_stat: DiffSummary {
                files_changed: 2,
                insertions: 10,
                deletions: 3,
                files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            },
            incremental_diff_stat: DiffSummary {
                files_changed: 2,
                insertions: 10,
                deletions: 3,
                files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            },
            appears_flattened: false,
        }],
    };

    let ev = SpurEvent::now(SpurEventBody::PlanTaskBlockedOnSetupConflict {
        plan_id: "plan-9".into(),
        task_id: "task-9".into(),
        delegation_id: "del-9".into(),
        dep_task_id: "task-7".into(),
        files: vec!["src/main.rs".into()],
        topology: Some(topology),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::PlanTaskBlockedOnSetupConflict {
            plan_id,
            task_id,
            delegation_id,
            dep_task_id,
            files,
            topology,
        } => {
            assert_eq!(plan_id, "plan-9");
            assert_eq!(task_id, "task-9");
            assert_eq!(delegation_id, "del-9");
            assert_eq!(dep_task_id, "task-7");
            assert_eq!(files, vec!["src/main.rs"]);
            assert!(topology.is_some());
            let topo = topology.unwrap();
            assert_eq!(topo.base_oid, "2779409d");
            assert_eq!(topo.approved_chain.len(), 1);
            assert_eq!(topo.approved_chain[0].task_id, "task-5");
        }
        other => panic!("expected PlanTaskBlockedOnSetupConflict, got {other:?}"),
    }
    assert!(json.contains("PlanTaskBlockedOnSetupConflict"));
    assert!(json.contains("topology"));
}

#[test]
fn worker_session_configured_roundtrips() {
    use spur_acp::domain::delegation::ResolvedSessionConfig;
    use std::collections::BTreeMap;

    let mut config_overrides_applied = BTreeMap::new();
    config_overrides_applied.insert("mode".to_string(), "plan".to_string());

    let ev = SpurEvent::now(SpurEventBody::WorkerSessionConfigured {
        brain_session_id: SessionId("brain-1".into()),
        executor_id: "exec-1".into(),
        config: ResolvedSessionConfig {
            agent: "codex".into(),
            profile: Some("rust-pro".into()),
            model: Some("gpt-5-codex".into()),
            effort: Some("high".into()),
            config_overrides_applied,
            skipped: vec![
                "effort: agent exposed no thought-level option (requested 'high')".into(),
            ],
            outcome_warning: Some("worktree normalized".into()),
        },
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::WorkerSessionConfigured {
            brain_session_id,
            executor_id,
            config,
        } => {
            assert_eq!(brain_session_id, SessionId("brain-1".into()));
            assert_eq!(executor_id, "exec-1");
            assert_eq!(config.agent, "codex");
            assert_eq!(config.profile.as_deref(), Some("rust-pro"));
            assert_eq!(config.model.as_deref(), Some("gpt-5-codex"));
            assert_eq!(config.effort.as_deref(), Some("high"));
            assert_eq!(
                config
                    .config_overrides_applied
                    .get("mode")
                    .map(String::as_str),
                Some("plan")
            );
            assert_eq!(config.skipped.len(), 1);
            assert_eq!(
                config.outcome_warning.as_deref(),
                Some("worktree normalized")
            );
        }
        other => panic!("expected WorkerSessionConfigured, got {other:?}"),
    }
    assert!(json.contains("WorkerSessionConfigured"));
    assert!(json.contains("gpt-5-codex"));
}

#[test]
fn worker_session_configured_defaults_are_compact_when_absent() {
    // A resolved config with no profile/model/effort/overrides/skips should
    // serialize its Option/collection fields away entirely (they're all
    // `#[serde(default, skip_serializing_if = ...)]`), keeping the wire
    // payload small for the common "nothing overridden" case.
    use spur_acp::domain::delegation::ResolvedSessionConfig;

    let ev = SpurEvent::now(SpurEventBody::WorkerSessionConfigured {
        brain_session_id: SessionId("brain-1".into()),
        executor_id: "exec-1".into(),
        config: ResolvedSessionConfig {
            agent: "claude-code".into(),
            ..Default::default()
        },
    });

    let value = serde_json::to_value(&ev).unwrap();
    let config = &value["body"]["WorkerSessionConfigured"]["config"];
    assert_eq!(config["agent"], serde_json::json!("claude-code"));
    let obj = config.as_object().expect("config object");
    assert!(!obj.contains_key("profile"));
    assert!(!obj.contains_key("model"));
    assert!(!obj.contains_key("effort"));
    assert!(!obj.contains_key("config_overrides_applied"));
    assert!(!obj.contains_key("skipped"));
    assert!(!obj.contains_key("outcome_warning"));

    let _round: SpurEvent = serde_json::from_value(value).expect("round-trip");
}

#[test]
fn delegation_result_deserializes_without_resolved_config_for_backward_compat() {
    // Pre-existing outcome artifacts persisted before `resolved_config`
    // existed must still deserialize cleanly, with the field defaulting to
    // `None` — this is the additive/optional backward-compat contract for
    // `fetch_outcome_artifact` consumers reading older blobs.
    use spur_acp::domain::DelegationResult;

    let json = serde_json::json!({
        "status": "Success",
        "diff": null,
        "summary": "done",
        "estimated_cost_usd": 0.01,
        "worker_branch": "spur/worker-x",
    });

    let result: DelegationResult =
        serde_json::from_value(json).expect("pre-existing DelegationResult must deserialize");
    assert!(result.resolved_config.is_none());
}

#[test]
fn brain_switched_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainSwitched {
        from: "grok".into(),
        to: "opencode".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainSwitched { from, to } => {
            assert_eq!(from, "grok");
            assert_eq!(to, "opencode");
        }
        other => panic!("expected BrainSwitched, got {other:?}"),
    }
}

#[test]
fn brain_switch_noop_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainSwitchNoop {
        name: "grok".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round.body,
        SpurEventBody::BrainSwitchNoop { name } if name == "grok"
    ));
}

#[test]
fn brain_switch_error_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainSwitchError {
        name: "nope".into(),
        available: vec!["grok".into(), "opencode".into()],
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainSwitchError { name, available } => {
            assert_eq!(name, "nope");
            assert_eq!(available, vec!["grok", "opencode"]);
        }
        other => panic!("expected BrainSwitchError, got {other:?}"),
    }
}

#[test]
fn brains_listed_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainsListed {
        brains: vec![BrainInfo {
            name: "grok".into(),
            kind: AgentKind::Grok,
            is_default: true,
        }],
        active: "grok".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainsListed { brains, active } => {
            assert_eq!(active, "grok");
            assert_eq!(brains.len(), 1);
            assert_eq!(brains[0].name, "grok");
            assert_eq!(brains[0].kind, AgentKind::Grok);
            assert!(brains[0].is_default);
        }
        other => panic!("expected BrainsListed, got {other:?}"),
    }
}

#[test]
fn brain_picker_open_roundtrips() {
    let ev = SpurEvent::now(SpurEventBody::BrainPickerOpen {
        brains: vec![BrainInfo {
            name: "opencode".into(),
            kind: AgentKind::OpenCode,
            is_default: false,
        }],
        active: "grok".into(),
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainPickerOpen { brains, active } => {
            assert_eq!(active, "grok");
            assert_eq!(brains[0].name, "opencode");
            assert_eq!(brains[0].kind, AgentKind::OpenCode);
        }
        other => panic!("expected BrainPickerOpen, got {other:?}"),
    }
}

#[test]
fn brain_retired_brain_switch_reason_roundtrips() {
    use spur_acp::domain::events::BrainRetireReason;
    let ev = SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("s1".into()),
        reason: BrainRetireReason::BrainSwitch,
    });
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    match round.body {
        SpurEventBody::BrainRetired { reason, .. } => {
            assert_eq!(reason, BrainRetireReason::BrainSwitch);
        }
        other => panic!("expected BrainRetired, got {other:?}"),
    }
}
