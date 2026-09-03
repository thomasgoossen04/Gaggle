//! [`Hash`] — a 256-bit BLAKE3 digest. Every content address and Merkle node in
//! the crate is one of these.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;

/// A 32-byte BLAKE3 digest.
///
/// Equality and ordering are plain byte comparisons (lexicographic), *not*
/// constant-time — content addresses are not secrets. Capability tokens and MACs
/// (milestone 7) get their own constant-time types.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash([u8; 32]);

impl Hash {
    pub const LEN: usize = 32;

    /// Wrap raw digest bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub const fn to_array(self) -> [u8; 32] {
        self.0
    }

    /// The content address of `data`: `blake3(data)` with no domain separation.
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    /// 64-character lowercase hex.
    pub fn to_hex(self) -> String {
        blake3::Hash::from(self.0).to_hex().to_string()
    }

    /// Parse 64 hex characters.
    pub fn from_hex(s: &str) -> Result<Self, Error> {
        blake3::Hash::from_hex(s)
            .map(|h| Self(*h.as_bytes()))
            .map_err(|e| Error::BadHash(e.to_string()))
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short form keeps test output and logs readable.
        write!(f, "Hash({}…)", &self.to_hex()[..10])
    }
}

impl FromStr for Hash {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::from_hex(s)
    }
}

impl From<[u8; 32]> for Hash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<blake3::Hash> for Hash {
    fn from(h: blake3::Hash) -> Self {
        Self(*h.as_bytes())
    }
}

impl Serialize for Hash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&self.to_hex())
        } else {
            s.serialize_bytes(&self.0)
        }
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl serde::de::Visitor<'_> for V {
            type Value = Hash;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a BLAKE3 hash as 64 hex chars or 32 raw bytes")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Hash, E> {
                Hash::from_hex(s).map_err(E::custom)
            }

            fn visit_bytes<E: serde::de::Error>(self, b: &[u8]) -> Result<Hash, E> {
                let arr: [u8; 32] = b
                    .try_into()
                    .map_err(|_| E::invalid_length(b.len(), &"32 bytes"))?;
                Ok(Hash(arr))
            }
        }

        if d.is_human_readable() {
            d.deserialize_str(V)
        } else {
            d.deserialize_bytes(V)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let h = Hash::of(b"gaggle");
        let s = h.to_hex();
        assert_eq!(s.len(), 64);
        assert_eq!(Hash::from_hex(&s).unwrap(), h);
        assert_eq!(s.parse::<Hash>().unwrap(), h);
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(Hash::from_hex("nope").is_err());
        assert!(Hash::from_hex(&"a".repeat(63)).is_err());
        assert!(Hash::from_hex(&"g".repeat(64)).is_err());
    }

    #[test]
    fn json_is_a_hex_string() {
        let h = Hash::of(b"x");
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json, format!("\"{}\"", h.to_hex()));
        assert_eq!(serde_json::from_str::<Hash>(&json).unwrap(), h);
    }

    #[test]
    fn ordering_is_lexicographic() {
        let a = Hash::from_bytes([0u8; 32]);
        let mut b_bytes = [0u8; 32];
        b_bytes[0] = 1;
        let b = Hash::from_bytes(b_bytes);
        assert!(a < b);
    }
}
