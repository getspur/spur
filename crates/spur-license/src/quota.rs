use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaKey {
    MaxConcurrentWorkers,
    EventRetentionBytes,
    MaxTeamMembers,
    MinSeats,
}

impl QuotaKey {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MaxConcurrentWorkers => "max_concurrent_workers",
            Self::EventRetentionBytes => "event_retention_bytes",
            Self::MaxTeamMembers => "max_team_members",
            Self::MinSeats => "min_seats",
        }
    }
}

impl fmt::Display for QuotaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaValue {
    Unlimited,
    Count(u64),
    Bytes(u64),
}

impl QuotaValue {
    pub const fn as_count(&self) -> Option<u64> {
        match self {
            Self::Count(n) => Some(*n),
            _ => None,
        }
    }

    pub const fn as_bytes(&self) -> Option<u64> {
        match self {
            Self::Bytes(n) => Some(*n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
