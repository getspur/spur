use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::schema::NodeKind;

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
    };
}

id_newtype!(NodeId);
id_newtype!(EdgeId);
id_newtype!(FileId);
id_newtype!(SpanId);
id_newtype!(RunId);
id_newtype!(EvidenceId);

pub const EXTERNAL_FILE_PATH: &str = "external://";

pub fn stable_symbol_id_for(
    relative_path: &str,
    fqn: &str,
    kind: NodeKind,
    byte_range_start: u64,
) -> String {
    stable_symbol_id_for_discriminator(relative_path, fqn, kind.discriminator(), byte_range_start)
}

/// Derive the stable id for a synthetic external node from its full import path.
///
/// The scheme is equivalent to hashing `(external://, full_path, external, 0)`,
/// so every import site that names the same external path deduplicates naturally.
pub fn stable_symbol_id_for_external_path(full_path: &str) -> String {
    stable_symbol_id_for(EXTERNAL_FILE_PATH, full_path, NodeKind::External, 0)
}

pub(crate) fn stable_symbol_id_for_discriminator(
    relative_path: &str,
    fqn: &str,
    kind_discriminator: &str,
    byte_range_start: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(relative_path.as_bytes());
    hasher.update([0]);
    hasher.update(fqn.as_bytes());
    hasher.update([0]);
    hasher.update(kind_discriminator.as_bytes());
    hasher.update([0]);
    hasher.update(byte_range_start.to_le_bytes());
    let digest = hasher.finalize();
    format!(
        "{:016x}",
        u64::from_be_bytes(digest[..8].try_into().unwrap())
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{stable_symbol_id_for, stable_symbol_id_for_external_path};
    use crate::schema::NodeKind;

    #[test]
    fn stable_symbol_id_for_external_path_is_deterministic() {
        let full_path = "serde::Deserialize";

        assert_eq!(
            stable_symbol_id_for_external_path(full_path),
            stable_symbol_id_for_external_path(full_path)
        );
        assert_eq!(
            stable_symbol_id_for_external_path(full_path),
            stable_symbol_id_for("external://", full_path, NodeKind::External, 0)
        );
    }

    #[test]
    fn stable_symbol_id_for_external_path_distinguishes_language_path_shapes() {
        let full_paths = [
            "serde::Deserialize",
            "std::path::Path",
            "react",
            "@scope/pkg/Button",
            "numpy.linalg",
            "django.http.HttpResponse",
        ];

        let ids: HashSet<_> = full_paths
            .iter()
            .map(|full_path| stable_symbol_id_for_external_path(full_path))
            .collect();

        assert_eq!(ids.len(), full_paths.len());
    }
}
