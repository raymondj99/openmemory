//! Versioned, domain-separated BLAKE3 canonical hashes.
//!
//! Hashes never depend on serde encodings or collection insertion order.

use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::error::MergeError;

macro_rules! hash_type {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// Raw 32-byte digest.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("blake3:")?;
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl FromStr for $name {
            type Err = MergeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let Some(hex) = value.strip_prefix("blake3:") else {
                    return Err(MergeError::InvalidField { field: "hash" });
                };
                if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(MergeError::InvalidField { field: "hash" });
                }
                let mut bytes = [0_u8; 32];
                for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
                    let pair = std::str::from_utf8(pair)
                        .map_err(|_| MergeError::InvalidField { field: "hash" })?;
                    bytes[index] = u8::from_str_radix(pair, 16)
                        .map_err(|_| MergeError::InvalidField { field: "hash" })?;
                }
                let parsed = Self(bytes);
                if parsed.to_string() != value {
                    return Err(MergeError::InvalidField { field: "hash" });
                }
                Ok(parsed)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(de::Error::custom)
            }
        }
    };
}

hash_type!(SemanticHash, "Canonical semantic-object hash.");
hash_type!(SnapshotHash, "Canonical domain or space snapshot hash.");
hash_type!(PacketHash, "Identity evidence packet binding hash.");
hash_type!(ReceiptHash, "Reviewed identity resolution receipt hash.");
hash_type!(PlanHash, "Directional material-merge plan hash.");

/// Explicit length-prefixed canonical hash writer.
pub(crate) struct CanonicalHasher(blake3::Hasher);

impl CanonicalHasher {
    pub(crate) fn new(domain: &'static [u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(domain.len() as u64).to_be_bytes());
        hasher.update(domain);
        Self(hasher)
    }

    pub(crate) fn tag(&mut self, tag: &'static str) {
        self.bytes(tag.as_bytes());
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.0.update(&(value.len() as u64).to_be_bytes());
        self.0.update(value);
    }

    pub(crate) fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.0.update(&value.to_be_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.0.update(&value.to_be_bytes());
    }

    pub(crate) fn bool(&mut self, value: bool) {
        self.0.update(&[u8::from(value)]);
    }

    pub(crate) fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_text_is_canonical_and_roundtrips() {
        let hash = SemanticHash::from_bytes([0xab; 32]);
        let encoded = hash.to_string();
        assert_eq!(encoded.parse::<SemanticHash>().unwrap(), hash);
        assert_eq!(
            serde_json::from_str::<SemanticHash>(&format!("\"{encoded}\"")).unwrap(),
            hash
        );
        assert!(encoded.to_uppercase().parse::<SemanticHash>().is_err());
        assert!("ab".parse::<SemanticHash>().is_err());
    }
}
