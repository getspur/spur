use spur_license::{QuotaKey, QuotaValue};

#[test]
fn quota_key_as_str_roundtrips() {
    assert_eq!(
        QuotaKey::MaxConcurrentWorkers.as_str(),
        "max_concurrent_workers"
    );
    assert_eq!(
        QuotaKey::EventRetentionBytes.as_str(),
        "event_retention_bytes"
    );
    assert_eq!(QuotaKey::MaxTeamMembers.as_str(), "max_team_members");
    assert_eq!(QuotaKey::MinSeats.as_str(), "min_seats");
}

#[test]
fn quota_value_as_count() {
    assert_eq!(QuotaValue::Count(5).as_count(), Some(5));
    assert_eq!(QuotaValue::Unlimited.as_count(), None);
}

#[test]
fn quota_value_as_bytes() {
    assert_eq!(QuotaValue::Bytes(1024).as_bytes(), Some(1024));
    assert_eq!(QuotaValue::Count(1).as_bytes(), None);
}
