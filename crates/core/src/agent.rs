//! General-purpose Ed25519 identity for *agents* — the accelerator daemon and
//! the operator that manages it over the control-plane admin API.
//!
//! This is deliberately separate from [`identity`](crate::identity), whose
//! [`ShareKeypair`](crate::identity::ShareKeypair) is *per-share* and whose
//! sign/verify are crate-private. An [`AgentKeypair`] signs arbitrary request
//! bytes and an [`AgentId`] verifies them, both through the public API, so a
//! daemon can authenticate operators and an operator can pin a daemon.

use std::fmt;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};
use crate::identity::Signature;

/// A full Ed25519 signing keypair. The secret half never leaves its owner.
pub struct AgentKeypair(SigningKey);

impl AgentKeypair {
    /// Generate a fresh random keypair.
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("system RNG unavailable");
        Self(SigningKey::from_bytes(&seed))
    }

    /// Deterministic keypair from a 32-byte seed (persistence, key derivation).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }

    /// The 32-byte secret seed — persist this to keep the same identity.
    pub fn to_seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// The public identity to publish / authorise.
    pub fn public(&self) -> AgentId {
        AgentId(self.0.verifying_key().to_bytes())
    }

    /// Sign `msg`. The caller is responsible for any domain separation.
    pub fn sign(&self, msg: &[u8]) -> Signature {
        Signature::from_bytes(self.0.sign(msg).to_bytes())
    }
}

impl fmt::Debug for AgentKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AgentKeypair({})", self.public())
    }
}

/// The public identity of an agent. Cheap to copy, safe to publish.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentId([u8; 32]);

impl AgentId {
    pub const LEN: usize = 32;

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.len() != 64 {
            return Err(Error::Auth(format!("agent id hex must be 64 chars, got {}", s.len())));
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|e| Error::Auth(format!("bad agent id hex: {e}")))?;
        }
        Ok(Self(out))
    }

    /// Verify `sig` over `msg` against this key. Uses `verify_strict` — a
    /// malleable or non-canonical signature is rejected.
    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<()> {
        let vk = VerifyingKey::from_bytes(&self.0)
            .map_err(|e| Error::Auth(format!("not a valid Ed25519 public key: {e}")))?;
        let sig = ed25519_dalek::Signature::from_bytes(&sig.to_bytes());
        vk.verify_strict(msg, &sig)
            .map_err(|_| Error::Auth("signature does not verify".into()))
    }
}

impl fmt::Debug for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AgentId({}…)", &self.to_hex()[..10])
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for AgentId {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&self.to_hex())
        } else {
            s.serialize_bytes(&self.0)
        }
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        if d.is_human_readable() {
            let s = String::deserialize(d)?;
            Self::from_hex(&s).map_err(D::Error::custom)
        } else {
            let bytes = <Vec<u8>>::deserialize(d)?;
            let arr: [u8; 32] =
                bytes.try_into().map_err(|_| D::Error::custom("agent id must be 32 bytes"))?;
            Ok(Self(arr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_round_trip() {
        let kp = AgentKeypair::from_seed([7u8; 32]);
        let pk = kp.public();
        let sig = kp.sign(b"hello");
        assert!(pk.verify(b"hello", &sig).is_ok());
        assert!(pk.verify(b"hell0", &sig).is_err());

        let other = AgentKeypair::generate().public();
        assert!(other.verify(b"hello", &sig).is_err());
    }

    #[test]
    fn from_seed_is_deterministic() {
        let a = AgentKeypair::from_seed([1u8; 32]).public();
        let b = AgentKeypair::from_seed([1u8; 32]).public();
        assert_eq!(a, b);
        assert_ne!(a, AgentKeypair::from_seed([2u8; 32]).public());
    }

    #[test]
    fn id_hex_round_trips() {
        let pk = AgentKeypair::from_seed([9u8; 32]).public();
        assert_eq!(AgentId::from_hex(&pk.to_hex()).unwrap(), pk);
        assert!(AgentId::from_hex("zz").is_err());
    }

    #[test]
    fn seed_round_trips() {
        let kp = AgentKeypair::generate();
        let restored = AgentKeypair::from_seed(kp.to_seed());
        assert_eq!(kp.public(), restored.public());
    }

    #[test]
    fn id_json_round_trips() {
        let pk = AgentKeypair::from_seed([3u8; 32]).public();
        let json = serde_json::to_string(&pk).unwrap();
        assert_eq!(serde_json::from_str::<AgentId>(&json).unwrap(), pk);
    }
}
