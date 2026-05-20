use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

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

pub fn stable_symbol_id_for(path: &Path, entity_name: &str, anchor_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().replace('\\', "/").as_bytes());
    hasher.update([0]);
    hasher.update(entity_name.as_bytes());
    hasher.update([0]);
    hasher.update(anchor_hash.as_bytes());
    let digest = hasher.finalize();
    format!(
        "{:016x}",
        u64::from_be_bytes(digest[..8].try_into().unwrap())
    )
}
