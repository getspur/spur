use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

pub fn stable_symbol_id_for(
    relative_path: &str,
    fqn: &str,
    kind: NodeKind,
    byte_range_start: u64,
) -> String {
    stable_symbol_id_for_discriminator(relative_path, fqn, kind.discriminator(), byte_range_start)
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
