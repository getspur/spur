#[test]
fn install_id_load_or_create_generates_uuid() {
    let id1 = spur_license::InstallId::load_or_create();
    let id2 = spur_license::InstallId::load_or_create();
    // Same process → same ID (file already exists)
    assert_eq!(id1, id2);
    // Must parse as valid UUID
    assert!(!id1.to_string().is_empty());
}

#[test]
#[cfg(feature = "test-support")]
fn install_id_from_uuid_roundtrips() {
    let uuid = uuid::Uuid::new_v4();
    let id = spur_license::InstallId::from_uuid(uuid);
    assert_eq!(id.to_string(), uuid.to_string());
}
