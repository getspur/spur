use std::fmt;

/// Stable per-install identifier. Generated once on first run, persisted in
/// ~/.spur/install-id. Used for deterministic rollout hashing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstallId(uuid::Uuid);

impl InstallId {
    pub fn load_or_create() -> Self {
        let path = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".spur").join("install-id"));

        if let Some(p) = path.as_ref() {
            if let Ok(s) = std::fs::read_to_string(p) {
                if let Ok(uuid) = s.trim().parse::<uuid::Uuid>() {
                    return Self(uuid);
                }
            }
        }

        let new_id = Self(uuid::Uuid::new_v4());
        if let Some(p) = path.as_ref() {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(p, new_id.0.to_string());
        }
        new_id
    }

    /// Construct from a known UUID. Test-only; production paths should use `load_or_create`.
    pub fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }
}

impl fmt::Display for InstallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
