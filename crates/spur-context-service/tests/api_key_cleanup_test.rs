use serde_json::json;
use spur_context_service::api_key_cleanup::{
    run_scheduled_cleanup, ApiKeyCleanupConfig, ApiKeyCleanupError, ApiKeyCleanupEvent,
};
use spur_context_service::api_keys::{
    generate_api_key, ApiKeyScope, ApiKeyScopes, ApiKeyStatus, ApiKeyStore, ApiKeyStoreError,
    CreateKeyRecord, FakeApiKeyStore, KeyEnvironment,
};

const NOW: u64 = 100 * 3_600 + 120;
const EXPIRES_AT: u64 = 99 * 3_600 + 30;

fn scheduled_event(operation: &str) -> ApiKeyCleanupEvent {
    serde_json::from_value(json!({
        "source": "aws.events",
        "detail-type": "Scheduled Event",
        "detail": { "operation": operation }
    }))
    .expect("fixture should match the cleanup event schema")
}

fn cleanup_config(page_limit: &str) -> ApiKeyCleanupConfig {
    ApiKeyCleanupConfig::parse("1", "4", "8", "100", page_limit)
        .expect("fixture config should be valid")
}

async fn insert_expired_key(store: &FakeApiKeyStore, name: &str) -> String {
    insert_expired_key_for_owner(store, "cognito:user:cleanup-test", name).await
}

async fn insert_expired_key_for_owner(
    store: &FakeApiKeyStore,
    owner_id: &str,
    name: &str,
) -> String {
    let generated = generate_api_key(
        KeyEnvironment::Live,
        owner_id,
        name,
        ApiKeyScopes::new([ApiKeyScope::ExternalRead]).expect("scope should be valid"),
        98 * 3_600,
        EXPIRES_AT,
    )
    .expect("expired fixture should be valid");
    let public_id = generated.public_id.clone();
    store
        .create_key(CreateKeyRecord::new(generated.record))
        .await
        .expect("fixture should persist");
    public_id
}

#[tokio::test]
async fn scheduled_cleanup_revokes_expired_keys_and_is_idempotent() {
    let store = FakeApiKeyStore::new();
    let public_id = insert_expired_key(&store, "cleanup one").await;

    let first = run_scheduled_cleanup(
        &scheduled_event("sweep_expired_api_keys"),
        &store,
        &cleanup_config("100"),
        NOW,
        "request-one",
    )
    .await
    .expect("scheduled cleanup should succeed");
    assert_eq!(first.processed, 1);
    assert_eq!(first.completed_hour, Some(99));
    assert!(!first.has_more);
    assert_eq!(first.cursor_lag_hours, 0);

    let record = store
        .get_key_consistent(&public_id)
        .await
        .expect("lookup should succeed")
        .expect("record should remain persisted");
    assert_eq!(record.status, ApiKeyStatus::Revoked);
    assert_eq!(record.revoked_at, Some(NOW));

    let retry = run_scheduled_cleanup(
        &scheduled_event("sweep_expired_api_keys"),
        &store,
        &cleanup_config("100"),
        NOW,
        "request-two",
    )
    .await
    .expect("cleanup retry should succeed");
    assert_eq!(retry.processed, 0);
    assert_eq!(retry.completed_hour, Some(99));
}

#[tokio::test]
async fn scheduled_cleanup_resumes_a_page_bounded_bucket_before_advancing_cursor() {
    let store = FakeApiKeyStore::new();
    insert_expired_key(&store, "cleanup first page").await;
    insert_expired_key(&store, "cleanup second page").await;
    let config = ApiKeyCleanupConfig::parse("1", "1", "3", "1", "1")
        .expect("single-page fixture config should be valid");

    let first = run_scheduled_cleanup(
        &scheduled_event("sweep_expired_api_keys"),
        &store,
        &config,
        NOW,
        "request-page-one",
    )
    .await
    .expect("first cleanup page should succeed");
    assert_eq!(first.processed, 1);
    assert_eq!(first.completed_hour, None);
    assert!(first.has_more);

    let second = run_scheduled_cleanup(
        &scheduled_event("sweep_expired_api_keys"),
        &store,
        &config,
        NOW,
        "request-page-two",
    )
    .await
    .expect("second cleanup page should succeed");
    assert_eq!(second.processed, 1);
    assert_eq!(second.completed_hour, Some(99));
    assert!(!second.has_more);
}

#[tokio::test]
async fn five_minute_schedule_drains_steady_state_bucket_without_double_decrement() {
    const STEADY_STATE_EXPIRIES_PER_HOUR: usize = 232;
    const INVOCATIONS_PER_HOUR: usize = 12;

    let store = FakeApiKeyStore::new();
    let mut cursor_lease = store
        .acquire_expiry_lease("cursor-prime", "cursor-token", NOW, 60)
        .expect("normal-operation cursor fixture should acquire a lease");
    store
        .save_expiry_cursor(&mut cursor_lease, 98, NOW)
        .expect("normal-operation cursor should begin at the prior hour");
    store
        .release_expiry_lease(&cursor_lease)
        .expect("cursor fixture lease should release");

    for index in 0..STEADY_STATE_EXPIRIES_PER_HOUR {
        insert_expired_key_for_owner(
            &store,
            &format!("cognito:user:capacity-{index}"),
            &format!("capacity-{index}"),
        )
        .await;
    }

    let config = ApiKeyCleanupConfig::parse("168", "4", "8", "100", "100")
        .expect("production-default capacity should be valid");
    let mut total_processed = 0;
    let mut invocations = 0;
    let mut final_result = None;
    for invocation in 0..INVOCATIONS_PER_HOUR {
        let result = run_scheduled_cleanup(
            &scheduled_event("sweep_expired_api_keys"),
            &store,
            &config,
            NOW + invocation as u64 * 300,
            &format!("scheduled-{invocation}"),
        )
        .await
        .expect("scheduled continuation should succeed");
        invocations += 1;
        assert!(result.processed <= 100);
        total_processed += result.processed;
        let complete = !result.has_more;
        final_result = Some(result);
        if complete {
            break;
        }
    }

    assert_eq!(total_processed, STEADY_STATE_EXPIRIES_PER_HOUR);
    assert_eq!(invocations, 3);
    let final_result = final_result.expect("the schedule should invoke cleanup");
    assert_eq!(final_result.completed_hour, Some(99));
    assert!(!final_result.has_more);
    assert_eq!(final_result.cursor_lag_hours, 0);

    let repeated = run_scheduled_cleanup(
        &scheduled_event("sweep_expired_api_keys"),
        &store,
        &config,
        NOW + 11 * 300,
        "scheduled-retry",
    )
    .await
    .expect("completed schedule retry should remain idempotent");
    assert_eq!(repeated.processed, 0);

    for index in 0..10 {
        let generated = generate_api_key(
            KeyEnvironment::Live,
            "cognito:user:capacity-0",
            &format!("replacement-{index}"),
            ApiKeyScopes::new([ApiKeyScope::ExternalRead]).expect("scope should be valid"),
            NOW + index as u64,
            NOW + 10 * 3_600,
        )
        .expect("replacement fixture should be valid");
        store
            .create_key(CreateKeyRecord::new(generated.record))
            .await
            .expect("cleanup must release exactly one owner-counter slot");
    }
    let eleventh = generate_api_key(
        KeyEnvironment::Live,
        "cognito:user:capacity-0",
        "replacement-eleven",
        ApiKeyScopes::new([ApiKeyScope::ExternalRead]).expect("scope should be valid"),
        NOW + 20,
        NOW + 10 * 3_600,
    )
    .expect("replacement fixture should be valid");
    assert_eq!(
        store
            .create_key(CreateKeyRecord::new(eleventh.record))
            .await,
        Err(ApiKeyStoreError::OwnerLimit)
    );
}

#[tokio::test]
async fn cleanup_rejects_wrong_events_before_store_access_and_propagates_store_failures() {
    let store = FakeApiKeyStore::new();
    let _lease = store
        .acquire_expiry_lease("other-worker", "held-token", NOW, 300)
        .expect("fixture lease should be acquired");

    assert_eq!(
        run_scheduled_cleanup(
            &scheduled_event("drain_queued_jobs"),
            &store,
            &cleanup_config("100"),
            NOW,
            "request-wrong-event",
        )
        .await,
        Err(ApiKeyCleanupError::InvalidEvent)
    );

    assert_eq!(
        run_scheduled_cleanup(
            &scheduled_event("sweep_expired_api_keys"),
            &store,
            &cleanup_config("100"),
            NOW,
            "request-store-failure",
        )
        .await,
        Err(ApiKeyCleanupError::Store(ApiKeyStoreError::LeaseBusy))
    );
}

#[test]
fn cleanup_config_and_metric_contract_are_bounded() {
    for (hours, buckets, pages, records, page_limit) in [
        ("0", "4", "8", "100", "100"),
        ("8761", "4", "8", "100", "100"),
        ("24", "0", "8", "100", "100"),
        ("24", "9", "8", "100", "100"),
        ("24", "4", "0", "100", "100"),
        ("24", "4", "5", "100", "100"),
        ("24", "4", "17", "100", "100"),
        ("24", "4", "8", "0", "100"),
        ("24", "4", "8", "101", "100"),
        ("24", "4", "8", "100", "0"),
        ("24", "4", "8", "100", "101"),
    ] {
        assert!(ApiKeyCleanupConfig::parse(hours, buckets, pages, records, page_limit).is_err());
    }

    let metric = serde_json::from_str::<serde_json::Value>(
        &spur_context_service::api_key_cleanup::ApiKeyCleanupResult {
            processed: 3,
            completed_hour: Some(97),
            has_more: true,
            cursor_lag_hours: 2,
        }
        .emf_document(1_700_000_000_000),
    )
    .expect("EMF document should be JSON");
    assert_eq!(metric["ApiKeyCleanupCursorLagHours"], 2);
    assert_eq!(metric["ApiKeyCleanupProcessed"], 3);
    assert_eq!(metric["ApiKeyCleanupHasMore"], 1);
    assert_eq!(metric["_aws"]["Timestamp"], 1_700_000_000_000_u64);
    assert_eq!(
        metric["_aws"]["CloudWatchMetrics"][0]["Namespace"],
        "SPUR/ContextServiceAuth"
    );
}
