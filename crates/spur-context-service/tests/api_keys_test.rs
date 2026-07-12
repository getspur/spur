use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::ExposeSecret;
use spur_context_service::api_keys::{
    generate_api_key, parse_api_key, verify_secret, ApiKeyRecord, ApiKeyScopes, ApiKeyStatus,
    ApiKeyStore, ApiKeyStoreError, CreateKeyRecord, FakeApiKeyStore, KeyEnvironment, RevokeResult,
    SweepRequest,
};

const BASE32_ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz234567";

#[test]
fn key_grammar_and_generation() {
    let scopes = ApiKeyScopes::parse(&["external.status", "external.read", "external.read"])
        .expect("valid scopes");
    assert_eq!(
        scopes.as_strings(),
        vec!["external.read", "external.status"]
    );
    assert!(ApiKeyScopes::parse(&["keys.manage"]).is_err());
    assert!(ApiKeyScopes::parse(&["external.unknown"]).is_err());

    let generated = generate_api_key(
        KeyEnvironment::Live,
        "owner-1",
        "production indexer",
        scopes.clone(),
        1_700_000_000,
        1_700_086_400,
    )
    .expect("key generation succeeds");
    let plaintext = generated.plaintext.expose_secret();
    let segments = plaintext.split('_').collect::<Vec<_>>();
    assert_eq!(segments.len(), 4);
    assert_eq!(segments[0], "spur");
    assert_eq!(segments[1], "live");
    assert_eq!(segments[2].len(), 26);
    assert_eq!(segments[3].len(), 52);
    assert_eq!(segments[2], generated.public_id);
    assert!(segments[2]
        .chars()
        .chain(segments[3].chars())
        .all(|character| BASE32_ALPHABET.contains(character)));

    let parsed = parse_api_key(plaintext).expect("generated key parses");
    assert_eq!(parsed.environment, KeyEnvironment::Live);
    assert_eq!(parsed.public_id, generated.public_id);
    assert!(!format!("{parsed:?}").contains(segments[3]));
    assert!(verify_secret(&parsed, &generated.record.secret_hash));
    assert!(!verify_secret(&parsed, &[0_u8; 32]));
    assert_eq!(generated.record.owner_id, "owner-1");
    assert_eq!(generated.record.name, "production indexer");
    assert_eq!(generated.record.scopes, scopes);
    assert_eq!(generated.record.secret_hash.len(), 32);
    assert_eq!(generated.record.created_at, 1_700_000_000);
    assert_eq!(generated.record.expires_at, 1_700_086_400);

    let record_debug = format!("{:?}", generated.record);
    assert!(!record_debug.contains(plaintext));
    assert!(!record_debug.contains(segments[3]));

    for malformed in [
        "spur_live_too-short_also-short",
        "spur_prod_aaaaaaaaaaaaaaaaaaaaaaaaaa_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1",
        "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa_extra",
        "SPUR_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        assert!(parse_api_key(malformed).is_err(), "accepted {malformed}");
    }

    assert!(
        generate_api_key(KeyEnvironment::Test, "owner-1", "", scopes.clone(), 10, 20,).is_err()
    );
    assert!(generate_api_key(
        KeyEnvironment::Test,
        "owner-1",
        &"n".repeat(101),
        scopes,
        10,
        20,
    )
    .is_err());
}

fn key_record(owner_id: &str, name: &str, created_at: u64, expires_at: u64) -> ApiKeyRecord {
    generate_api_key(
        KeyEnvironment::Test,
        owner_id,
        name,
        ApiKeyScopes::parse(&["external.read"]).expect("valid scope"),
        created_at,
        expires_at,
    )
    .expect("valid generated key")
    .record
}

#[tokio::test]
async fn owner_cursor_preserves_numeric_order_across_digit_boundaries() {
    let store = FakeApiKeyStore::new();
    for created_at in [9, 10, 99, 100] {
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-order",
                &format!("created-{created_at}"),
                created_at,
                1_000,
            )))
            .await
            .expect("create succeeds");
    }

    let mut cursor = None;
    let mut created_at = Vec::new();
    loop {
        let page = store
            .list_owner_keys("owner-order", cursor.as_deref(), 1)
            .await
            .expect("list succeeds");
        created_at.extend(page.keys.iter().map(|record| record.created_at));
        let Some(next_cursor) = page.next_cursor else {
            break;
        };
        assert_eq!(next_cursor.split('#').nth(1).map(str::len), Some(20));
        cursor = Some(next_cursor);
    }
    assert_eq!(created_at, [9, 10, 99, 100]);

    let public_id = key_record("owner-order", "cursor", 200, 1_000).public_id;
    assert_eq!(
        store
            .list_owner_keys("owner-order", Some(&format!("KEY#9#{public_id}")), 1,)
            .await,
        Err(ApiKeyStoreError::InvalidRequest)
    );
}

#[tokio::test]
async fn consistent_lookup_returns_inactive_records_for_authorizer_checks() {
    let store = FakeApiKeyStore::new();
    let expired = key_record("owner-lookup", "expired", 1, 2);
    let expired_id = expired.public_id.clone();
    store
        .create_key(CreateKeyRecord::new(expired))
        .await
        .expect("expired record create succeeds");

    let expired = store
        .get_key_consistent(&expired_id)
        .await
        .expect("expired lookup succeeds")
        .expect("expired record remains distinguishable from an unknown ID");
    assert!(!expired.is_active_at(u64::MAX));

    let active = key_record("owner-lookup", "revoked", 100, u64::MAX);
    let revoked_id = active.public_id.clone();
    store
        .create_key(CreateKeyRecord::new(active))
        .await
        .expect("active record create succeeds");
    assert_eq!(
        store
            .revoke_key("owner-lookup", &revoked_id, 200)
            .await
            .expect("revoke succeeds"),
        RevokeResult::Revoked
    );

    let revoked = store
        .get_key_consistent(&revoked_id)
        .await
        .expect("revoked lookup succeeds")
        .expect("revoked record remains distinguishable from an unknown ID");
    assert_eq!(revoked.status, ApiKeyStatus::Revoked);
    assert!(!revoked.is_active_at(201));
}

#[test]
fn debug_redacts_secret_hash_transitively() {
    const PUBLIC_ID_MARKER: &str = "PUBLIC-ID-MUST-NOT-LOG";
    const OWNER_ID_MARKER: &str = "OWNER-ID-MUST-NOT-LOG";
    const NAME_MARKER: &str = "NAME-MUST-NOT-LOG";
    let distinctive_hash = [0xa5_u8; 32];
    let digest_debug = format!("{distinctive_hash:?}");
    let mut generated = generate_api_key(
        KeyEnvironment::Test,
        "owner-debug",
        "debug key",
        ApiKeyScopes::parse(&["external.read"]).expect("valid scope"),
        1_700_000_000,
        1_800_000_000,
    )
    .expect("key generation succeeds");
    generated.record.public_id = PUBLIC_ID_MARKER.to_string();
    generated.record.owner_id = OWNER_ID_MARKER.to_string();
    generated.record.name = NAME_MARKER.to_string();
    generated.record.secret_hash = distinctive_hash;

    let record_debug = format!("{:?}", generated.record);
    let create_debug = format!("{:?}", CreateKeyRecord::new(generated.record.clone()));
    let generated_debug = format!("{generated:?}");
    for debug in [record_debug, create_debug, generated_debug] {
        assert!(!debug.contains(&digest_debug));
        assert!(!debug.contains(PUBLIC_ID_MARKER));
        assert!(!debug.contains(OWNER_ID_MARKER));
        assert!(!debug.contains(NAME_MARKER));
        assert!(debug.contains("[REDACTED]"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn store_enforces_owner_cap_and_duplicate_ids_atomically() {
    let store = FakeApiKeyStore::new();
    let mut creates = Vec::new();
    for index in 0..11 {
        let store = store.clone();
        creates.push(tokio::spawn(async move {
            let record = key_record(
                "owner-cap",
                &format!("key-{index}"),
                1_700_000_000 + index,
                1_800_000_000,
            );
            store.create_key(CreateKeyRecord::new(record)).await
        }));
    }

    let mut created = 0;
    let mut limited = 0;
    for create in creates {
        match create.await.expect("task joins") {
            Ok(()) => created += 1,
            Err(ApiKeyStoreError::OwnerLimit) => limited += 1,
            Err(error) => panic!("unexpected create error: {error}"),
        }
    }
    assert_eq!(created, 10);
    assert_eq!(limited, 1);
    assert_eq!(
        store
            .list_owner_keys("owner-cap", None, 20)
            .await
            .expect("list succeeds")
            .keys
            .len(),
        10
    );

    let duplicate_store = FakeApiKeyStore::new();
    let duplicate = key_record("owner-duplicate", "duplicate", 1_700_000_000, 1_800_000_000);
    duplicate_store
        .create_key(CreateKeyRecord::new(duplicate.clone()))
        .await
        .expect("first create succeeds");
    assert_eq!(
        duplicate_store
            .create_key(CreateKeyRecord::new(duplicate))
            .await,
        Err(ApiKeyStoreError::DuplicatePublicId)
    );
    assert_eq!(
        duplicate_store
            .list_owner_keys("owner-duplicate", None, 20)
            .await
            .expect("list succeeds")
            .keys
            .len(),
        1
    );
}

#[tokio::test]
async fn revoke_is_owner_scoped_idempotent_and_decrements_once() {
    let store = FakeApiKeyStore::new();
    let first = key_record("owner-revoke", "first", 1_700_000_000, 1_900_000_000);
    let first_id = first.public_id.clone();
    store
        .create_key(CreateKeyRecord::new(first))
        .await
        .expect("first create succeeds");
    for index in 1..10 {
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-revoke",
                &format!("key-{index}"),
                1_700_000_000 + index,
                1_900_000_000,
            )))
            .await
            .expect("cap fill succeeds");
    }

    assert_eq!(
        store
            .revoke_key("different-owner", &first_id, 1_700_000_100)
            .await
            .expect("cross-owner result"),
        RevokeResult::NotFound
    );
    assert_eq!(
        store
            .revoke_key("owner-revoke", &first_id, 1_700_000_100)
            .await
            .expect("revoke succeeds"),
        RevokeResult::Revoked
    );
    assert_eq!(
        store
            .revoke_key("owner-revoke", &first_id, 1_700_000_200)
            .await
            .expect("repeat revoke succeeds"),
        RevokeResult::AlreadyRevoked
    );

    store
        .create_key(CreateKeyRecord::new(key_record(
            "owner-revoke",
            "replacement",
            1_700_000_200,
            1_900_000_000,
        )))
        .await
        .expect("one replacement fits");
    assert_eq!(
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-revoke",
                "too-many",
                1_700_000_201,
                1_900_000_000,
            )))
            .await,
        Err(ApiKeyStoreError::OwnerLimit)
    );
}

#[tokio::test]
async fn expired_keys_are_denied_and_swept_with_persisted_catch_up_cursor() {
    let store = FakeApiKeyStore::new();
    let current_hour = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
        / 3_600;
    let start_hour = current_hour - 2;
    let first_expiry = start_hour * 3_600 + 10;
    let second_expiry = (start_hour + 1) * 3_600 + 10;
    let first = key_record("owner-expiry", "first", first_expiry - 100, first_expiry);
    let first_id = first.public_id.clone();
    let second = key_record("owner-expiry", "second", second_expiry - 100, second_expiry);
    store
        .create_key(CreateKeyRecord::new(first))
        .await
        .expect("first create succeeds");
    store
        .create_key(CreateKeyRecord::new(second))
        .await
        .expect("second create succeeds");

    let expired = store
        .get_key_consistent(&first_id)
        .await
        .expect("get succeeds")
        .expect("expired record remains available for authorizer checks");
    assert!(!expired.is_active_at(first_expiry));

    let now = (start_hour + 3) * 3_600;
    let request = SweepRequest {
        now_epoch_seconds: now,
        start_hour,
        max_buckets: 1,
        max_pages: 3,
        max_records: 100,
        page_limit: 100,
        lease_owner: "worker-a".to_string(),
        lease_duration_seconds: 60,
    };
    let first_page = store
        .sweep_expired(request.clone())
        .await
        .expect("first sweep succeeds");
    assert_eq!(first_page.processed, 1);
    assert_eq!(first_page.completed_hour, Some(start_hour));
    assert!(first_page.has_more);

    let other_worker = SweepRequest {
        lease_owner: "worker-b".to_string(),
        ..request.clone()
    };
    let second_page = store
        .sweep_expired(other_worker)
        .await
        .expect("released lease lets another worker resume immediately");
    assert_eq!(second_page.processed, 1);
    assert_eq!(second_page.completed_hour, Some(start_hour + 1));
    assert!(!second_page.has_more);

    let repeated_page = store
        .sweep_expired(request)
        .await
        .expect("completed sweep is idempotent");
    assert_eq!(repeated_page.processed, 0);

    let records = store
        .list_owner_keys("owner-expiry", None, 10)
        .await
        .expect("list succeeds")
        .keys;
    assert!(records
        .iter()
        .all(|record| record.status == ApiKeyStatus::Revoked));
}

#[tokio::test]
async fn list_cursor_and_partial_expiry_pages_do_not_double_decrement() {
    let store = FakeApiKeyStore::new();
    let hour = 600_000_u64;
    for index in 0..3 {
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-pages",
                &format!("key-{index}"),
                hour * 3_600 - 100 + index,
                hour * 3_600 + 10 + index,
            )))
            .await
            .expect("create succeeds");
    }

    let first_list = store
        .list_owner_keys("owner-pages", None, 2)
        .await
        .expect("first list succeeds");
    assert_eq!(first_list.keys.len(), 2);
    let second_list = store
        .list_owner_keys("owner-pages", first_list.next_cursor.as_deref(), 2)
        .await
        .expect("second list succeeds");
    assert_eq!(second_list.keys.len(), 1);
    assert!(second_list.next_cursor.is_none());

    let request = SweepRequest {
        now_epoch_seconds: (hour + 1) * 3_600 + 60,
        start_hour: hour,
        max_buckets: 1,
        max_pages: 1,
        max_records: 2,
        page_limit: 2,
        lease_owner: "worker-pages".to_string(),
        lease_duration_seconds: 60,
    };
    let first_sweep = store
        .sweep_expired(request.clone())
        .await
        .expect("first sweep succeeds");
    assert_eq!(first_sweep.processed, 2);
    assert_eq!(first_sweep.completed_hour, None);
    let second_sweep = store
        .sweep_expired(request.clone())
        .await
        .expect("second sweep succeeds");
    assert_eq!(second_sweep.processed, 1);
    assert_eq!(second_sweep.completed_hour, Some(hour));
    let third_sweep = store
        .sweep_expired(request)
        .await
        .expect("completed sweep is idempotent");
    assert_eq!(third_sweep.processed, 0);

    for index in 0..10 {
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-pages",
                &format!("replacement-{index}"),
                (hour + 1) * 3_600 + index,
                (hour + 10) * 3_600,
            )))
            .await
            .expect("exactly ten active replacements fit");
    }
    assert_eq!(
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-pages",
                "eleventh-replacement",
                (hour + 1) * 3_600 + 20,
                (hour + 10) * 3_600,
            )))
            .await,
        Err(ApiKeyStoreError::OwnerLimit)
    );
}

#[tokio::test]
async fn open_expiry_hour_is_revisited_after_it_closes() {
    let store = FakeApiKeyStore::new();
    let open_hour = 700_000_u64;
    let hour_start = open_hour * 3_600;
    for (name, expires_at) in [
        ("already-expired", hour_start + 10),
        ("expires-later", hour_start + 3_000),
    ] {
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-open-hour",
                name,
                hour_start,
                expires_at,
            )))
            .await
            .expect("create succeeds");
    }

    let open_request = SweepRequest {
        now_epoch_seconds: hour_start + 100,
        start_hour: open_hour,
        max_buckets: 1,
        max_pages: 3,
        max_records: 10,
        page_limit: 10,
        lease_owner: "worker-open-hour".to_string(),
        lease_duration_seconds: 60,
    };
    let open_page = store
        .sweep_expired(open_request.clone())
        .await
        .expect("open-hour sweep succeeds");
    assert_eq!(open_page.processed, 0);
    assert_eq!(open_page.completed_hour, None);
    assert!(!open_page.has_more);

    let closed_request = SweepRequest {
        now_epoch_seconds: (open_hour + 1) * 3_600 + 60,
        ..open_request
    };
    let closed_page = store
        .sweep_expired(closed_request.clone())
        .await
        .expect("closed-hour sweep succeeds");
    assert_eq!(closed_page.processed, 2);
    assert_eq!(closed_page.completed_hour, Some(open_hour));
    assert!(!closed_page.has_more);

    let repeated_page = store
        .sweep_expired(closed_request)
        .await
        .expect("repeated sweep succeeds");
    assert_eq!(repeated_page.processed, 0);
    assert_eq!(repeated_page.completed_hour, Some(open_hour));

    for index in 0..10 {
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-open-hour",
                &format!("replacement-{index}"),
                (open_hour + 1) * 3_600 + index,
                (open_hour + 10) * 3_600,
            )))
            .await
            .expect("exactly ten replacements fit");
    }
    assert_eq!(
        store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-open-hour",
                "eleventh-replacement",
                (open_hour + 1) * 3_600 + 20,
                (open_hour + 10) * 3_600,
            )))
            .await,
        Err(ApiKeyStoreError::OwnerLimit)
    );
}

#[tokio::test]
async fn late_expiry_index_arrival_is_reclaimed_without_starving_forward_work() {
    let store = FakeApiKeyStore::new();
    let late_hour = 800_000_u64;
    let late = key_record(
        "owner-late-index",
        "late-index-entry",
        late_hour * 3_600,
        late_hour * 3_600 + 10,
    );
    let late_id = late.public_id.clone();
    store
        .create_key(CreateKeyRecord::new(late))
        .await
        .expect("late record create succeeds");
    store
        .set_expiry_index_visible(&late_id, false)
        .expect("hide simulated GSI entry");

    let initial = SweepRequest {
        now_epoch_seconds: (late_hour + 1) * 3_600 + 60,
        start_hour: late_hour,
        max_buckets: 1,
        max_pages: 3,
        max_records: 10,
        page_limit: 10,
        lease_owner: "late-worker".to_string(),
        lease_duration_seconds: 60,
    };
    let empty_page = store
        .sweep_expired(initial.clone())
        .await
        .expect("initial empty query succeeds");
    assert_eq!(empty_page.processed, 0);
    assert_eq!(empty_page.completed_hour, Some(late_hour));

    store
        .set_expiry_index_visible(&late_id, true)
        .expect("reveal simulated late GSI entry");
    store
        .create_key(CreateKeyRecord::new(key_record(
            "owner-late-index",
            "forward-entry",
            (late_hour + 1) * 3_600,
            (late_hour + 1) * 3_600 + 10,
        )))
        .await
        .expect("forward record create succeeds");

    let catch_up = SweepRequest {
        now_epoch_seconds: (late_hour + 2) * 3_600 + 60,
        ..initial
    };
    let catch_up_page = store
        .sweep_expired(catch_up.clone())
        .await
        .expect("forward and overlap sweep succeeds");
    assert_eq!(catch_up_page.processed, 2);
    assert_eq!(catch_up_page.completed_hour, Some(late_hour + 1));
    assert!(!catch_up_page.has_more);

    let repeated = store
        .sweep_expired(catch_up)
        .await
        .expect("overlap rescan is idempotent");
    assert_eq!(repeated.processed, 0);
    assert_eq!(repeated.completed_hour, Some(late_hour + 1));
}

#[tokio::test]
async fn sweep_limits_historical_catchup_by_pages_buckets_and_records() {
    let record_store = FakeApiKeyStore::new();
    let hour = 850_000_u64;
    for index in 0..3 {
        record_store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-record-budget",
                &format!("bounded-{index}"),
                hour * 3_600,
                hour * 3_600 + 10 + index,
            )))
            .await
            .expect("bounded fixture should persist");
    }

    let record_bounded = SweepRequest {
        now_epoch_seconds: (hour + 1) * 3_600 + 60,
        start_hour: hour,
        max_buckets: 8,
        max_pages: 8,
        max_records: 2,
        page_limit: 100,
        lease_owner: "record-budget".to_string(),
        lease_duration_seconds: 60,
    };
    let first = record_store
        .sweep_expired(record_bounded.clone())
        .await
        .expect("record-bounded sweep should succeed");
    assert_eq!(first.processed, 2);
    assert!(first.has_more);

    let second = record_store
        .sweep_expired(record_bounded)
        .await
        .expect("record-bounded continuation should succeed");
    assert_eq!(second.processed, 1);

    for index in 0..10 {
        record_store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-record-budget",
                &format!("replacement-{index}"),
                (hour + 1) * 3_600 + index,
                (hour + 10) * 3_600,
            )))
            .await
            .expect("exactly ten replacements should fit after cleanup");
    }
    assert_eq!(
        record_store
            .create_key(CreateKeyRecord::new(key_record(
                "owner-record-budget",
                "eleventh-replacement",
                (hour + 1) * 3_600 + 20,
                (hour + 10) * 3_600,
            )))
            .await,
        Err(ApiKeyStoreError::OwnerLimit)
    );

    let horizon_store = FakeApiKeyStore::new();
    let page_bounded = SweepRequest {
        now_epoch_seconds: (hour + 4) * 3_600 + 60,
        start_hour: hour,
        max_buckets: 4,
        max_pages: 2,
        max_records: 100,
        page_limit: 100,
        lease_owner: "page-budget".to_string(),
        lease_duration_seconds: 60,
    };
    let page_limited = horizon_store
        .sweep_expired(page_bounded)
        .await
        .expect("page-bounded sweep should succeed");
    assert_eq!(page_limited.completed_hour, Some(hour + 1));
    assert!(page_limited.has_more);

    let bucket_bounded = SweepRequest {
        now_epoch_seconds: (hour + 4) * 3_600 + 60,
        start_hour: hour + 2,
        max_buckets: 1,
        max_pages: 8,
        max_records: 100,
        page_limit: 100,
        lease_owner: "bucket-budget".to_string(),
        lease_duration_seconds: 60,
    };
    let bucket_limited = horizon_store
        .sweep_expired(bucket_bounded)
        .await
        .expect("bucket-bounded sweep should succeed");
    assert_eq!(bucket_limited.completed_hour, Some(hour + 2));
    assert!(bucket_limited.has_more);

    for invalid in [
        SweepRequest {
            max_buckets: 9,
            lease_owner: "invalid-buckets".to_string(),
            ..bucket_limited_request(hour)
        },
        SweepRequest {
            max_pages: 17,
            lease_owner: "invalid-pages".to_string(),
            ..bucket_limited_request(hour)
        },
        SweepRequest {
            max_records: 101,
            lease_owner: "invalid-records".to_string(),
            ..bucket_limited_request(hour)
        },
    ] {
        assert_eq!(
            horizon_store.sweep_expired(invalid).await,
            Err(ApiKeyStoreError::InvalidRequest)
        );
    }
}

fn bucket_limited_request(hour: u64) -> SweepRequest {
    SweepRequest {
        now_epoch_seconds: (hour + 4) * 3_600 + 60,
        start_hour: hour + 3,
        max_buckets: 1,
        max_pages: 1,
        max_records: 1,
        page_limit: 1,
        lease_owner: "invalid-base".to_string(),
        lease_duration_seconds: 60,
    }
}

#[test]
fn expiry_lease_is_fenced_and_cursor_is_monotonic() {
    let store = FakeApiKeyStore::new();
    let mut old = store
        .acquire_expiry_lease("shared-owner", "old-token", 100, 10)
        .expect("first lease acquired");
    assert_eq!(
        store.acquire_expiry_lease("shared-owner", "overlap-token", 105, 10),
        Err(ApiKeyStoreError::LeaseBusy)
    );

    let mut current = store
        .acquire_expiry_lease("shared-owner", "current-token", 111, 10)
        .expect("expired lease replaced");
    assert_eq!(
        store.save_expiry_cursor(&mut old, 41, 112),
        Err(ApiKeyStoreError::LeaseBusy)
    );
    assert_eq!(
        store.release_expiry_lease(&old),
        Err(ApiKeyStoreError::LeaseBusy)
    );
    store
        .save_expiry_cursor(&mut current, 42, 112)
        .expect("current fenced writer advances cursor");
    assert_eq!(
        store.expiry_completed_hour().expect("cursor read"),
        Some(42)
    );
    store
        .release_expiry_lease(&current)
        .expect("current lease releases");

    let mut next = store
        .acquire_expiry_lease("shared-owner", "next-token", 113, 10)
        .expect("next lease acquired");
    assert_eq!(
        store.save_expiry_cursor(&mut next, 41, 114),
        Err(ApiKeyStoreError::Conflict)
    );
    assert_eq!(
        store.expiry_completed_hour().expect("cursor read"),
        Some(42)
    );
    store
        .release_expiry_lease(&next)
        .expect("next lease releases");
}
