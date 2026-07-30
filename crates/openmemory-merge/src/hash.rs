//! Typed canonical semantic hashes.

use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{MergeError, MergeErrorCode};

macro_rules! semantic_hash {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub(crate) [u8; 32]);

        impl $name {
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_string())
                    .finish()
            }
        }

        impl FromStr for $name {
            type Err = MergeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value.len() != 64
                    || value
                        .bytes()
                        .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
                {
                    return Err(MergeError::new(
                        MergeErrorCode::InvalidInput,
                        concat!(stringify!($name), " must be 64 lowercase hex characters"),
                    ));
                }
                let mut bytes = [0_u8; 32];
                for (index, slot) in bytes.iter_mut().enumerate() {
                    let offset = index * 2;
                    *slot = u8::from_str_radix(&value[offset..offset + 2], 16).map_err(|_| {
                        MergeError::new(
                            MergeErrorCode::InvalidInput,
                            concat!(stringify!($name), " contains invalid hex"),
                        )
                    })?;
                }
                Ok(Self(bytes))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }
    };
}

semantic_hash!(SemanticHash);
semantic_hash!(SnapshotHash);
semantic_hash!(PacketHash);
semantic_hash!(CandidateHash);
semantic_hash!(ReceiptHash);
semantic_hash!(ActionStreamHash);
semantic_hash!(PredictedResultHash);
semantic_hash!(PlanHash);
