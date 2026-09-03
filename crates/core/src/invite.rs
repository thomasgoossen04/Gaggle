//! Invites and capability tokens.
//!
//! A Gaggle swarm is private: peers only serve chunks to someone who presents a
//! [`SignedCapability`] issued by the share's [`ShareKeypair`]. The capability
//! is a *bearer* token — whoever holds it has the access it describes — so it is
//! scoped ([`Scope::All`] or [`Scope::Files`]) and may carry an expiry.
//!
//! An [`Invite`] is the shareable blob (link / QR / file): the share's public
//! key, the manifest's content id and name for display, and one capability. It
//! round-trips through a compact `gaggle1…` string.

use std::collections::BTreeSet;

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::identity::{ShareKeypair, SharePublicKey, Signature};

/// Domain-separation tag mixed into the bytes a capability is signed over, so a
/// signature can never be replayed as some other kind of message.
const CAP_DOMAIN: &[u8] = b"gaggle-capability-v1\0";

/// Prefix of the encoded [`Invite`] string.
const INVITE_PREFIX: &str = "gaggle1";

/// What a capability grants access to within a share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "paths")]
pub enum Scope {
    /// Every file in the share.
    All,
    /// Only these manifest paths (sorted, de-duplicated).
    Files(Vec<String>),
}

impl Scope {
    /// A [`Scope::Files`] with `paths` canonicalized (sorted + deduped). An
    /// empty set stays `Files([])` — a token that grants nothing but swarm
    /// membership.
    pub fn files<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set: BTreeSet<String> = paths.into_iter().map(Into::into).collect();
        Scope::Files(set.into_iter().collect())
    }

    /// Does this scope cover `path`?
    pub fn allows(&self, path: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Files(paths) => paths.iter().any(|p| p == path),
        }
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Scope::All)
    }
}

/// The unsigned body of a capability token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// Which share this is for.
    pub share: SharePublicKey,
    /// The exact manifest version this grants — pins the content so a token
    /// cannot silently follow a folder to a later revision.
    pub manifest_id: Hash,
    pub scope: Scope,
    /// Unix seconds after which the token is dead. `None` = no expiry.
    pub expires_at: Option<u64>,
    /// Random per-token bytes: makes every issued token unique so it can be
    /// named in a revocation list later.
    pub nonce: [u8; 16],
}

impl Capability {
    /// A capability for `manifest` (whole folder, no expiry). Fill in
    /// [`scope`](Self::scope) / [`expires_at`](Self::expires_at) afterwards.
    pub fn new(share: SharePublicKey, manifest_id: Hash) -> Self {
        let mut nonce = [0u8; 16];
        getrandom::getrandom(&mut nonce).expect("system RNG unavailable");
        Self { share, manifest_id, scope: Scope::All, expires_at: None, nonce }
    }

    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    pub fn expiring_at(mut self, unix_seconds: u64) -> Self {
        self.expires_at = Some(unix_seconds);
        self
    }

    /// Deterministic bytes to sign / verify: a domain tag followed by the
    /// capability's canonical JSON (all fields are scalars or sorted, so the
    /// encoding is stable).
    fn signing_bytes(&self) -> Vec<u8> {
        let mut buf = CAP_DOMAIN.to_vec();
        serde_json::to_writer(&mut buf, self).expect("a Capability always serializes");
        buf
    }
}

/// A [`Capability`] plus the origin's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCapability {
    pub capability: Capability,
    pub signature: Signature,
}

impl ShareKeypair {
    /// Sign `capability`, producing a token to hand out. Panics if the
    /// capability's `share` is not this keypair's public key (a programming
    /// error).
    pub fn issue(&self, capability: Capability) -> SignedCapability {
        assert_eq!(
            capability.share,
            self.public(),
            "issuing a capability for a different share's key"
        );
        let signature = self.sign(&capability.signing_bytes());
        SignedCapability { capability, signature }
    }
}

impl SignedCapability {
    /// Check the signature, the share it names, and expiry against `now` (unix
    /// seconds). Returns the inner [`Capability`] on success.
    pub fn verify(&self, now: u64) -> Result<&Capability> {
        self.capability
            .share
            .verify(&self.capability.signing_bytes(), &self.signature)?;
        if let Some(exp) = self.capability.expires_at
            && now >= exp
        {
            return Err(Error::Invite(format!("capability expired at {exp} (now {now})")));
        }
        Ok(&self.capability)
    }

    /// Like [`verify`](Self::verify) but also require the token to be for
    /// `share` and `manifest_id` — what a serving node checks.
    pub fn verify_for(
        &self,
        share: &SharePublicKey,
        manifest_id: &Hash,
        now: u64,
    ) -> Result<&Capability> {
        let cap = self.verify(now)?;
        if cap.share != *share {
            return Err(Error::Invite("capability is for a different share".into()));
        }
        if cap.manifest_id != *manifest_id {
            return Err(Error::Invite("capability is for a different manifest version".into()));
        }
        Ok(cap)
    }
}

/// The shareable invite: everything a new subscriber needs to find the share,
/// authenticate its manifest, and prove swarm access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub share: SharePublicKey,
    /// The manifest's content id — also the DHT discovery key.
    pub manifest_id: Hash,
    /// Human-facing label (the folder name). Not authenticated; display only.
    pub name: String,
    pub credential: SignedCapability,
}

impl Invite {
    /// Build an invite. `manifest` supplies the id and name; `credential` must
    /// already be issued for that manifest.
    pub fn new(
        share: SharePublicKey,
        manifest_id: Hash,
        name: impl Into<String>,
        credential: SignedCapability,
    ) -> Self {
        Self { share, manifest_id, name: name.into(), credential }
    }

    /// Verify the embedded credential is well-formed, unexpired at `now`, and
    /// consistent with this invite's `share` / `manifest_id`.
    pub fn validate(&self, now: u64) -> Result<()> {
        self.credential.verify_for(&self.share, &self.manifest_id, now)?;
        Ok(())
    }

    /// Encode as a single `gaggle1<base64url>` token.
    pub fn to_url(&self) -> String {
        let json = serde_json::to_vec(self).expect("an Invite always serializes");
        format!(
            "{INVITE_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }

    /// Parse a token produced by [`to_url`](Self::to_url). Does **not** check
    /// the signature — call [`validate`](Self::validate) after.
    pub fn parse(token: &str) -> Result<Self> {
        let body = token
            .strip_prefix(INVITE_PREFIX)
            .ok_or_else(|| Error::Invite(format!("invite must start with `{INVITE_PREFIX}`")))?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body.as_bytes())
            .map_err(|e| Error::Invite(format!("invite is not valid base64url: {e}")))?;
        let invite: Invite = serde_json::from_slice(&json)
            .map_err(|e| Error::Invite(format!("invite payload is malformed: {e}")))?;
        Ok(invite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kp() -> ShareKeypair {
        ShareKeypair::from_seed([42u8; 32])
    }

    fn manifest_id() -> Hash {
        Hash::of(b"a manifest")
    }

    #[test]
    fn issued_capability_verifies_and_tampering_is_caught() {
        let kp = kp();
        let cap = Capability::new(kp.public(), manifest_id());
        let token = kp.issue(cap.clone());

        assert_eq!(token.verify(0).unwrap(), &cap);
        assert!(token.verify_for(&kp.public(), &manifest_id(), 0).is_ok());

        // Wrong manifest / wrong share.
        assert!(token.verify_for(&kp.public(), &Hash::of(b"other"), 0).is_err());
        let other = ShareKeypair::from_seed([1u8; 32]).public();
        assert!(token.verify_for(&other, &manifest_id(), 0).is_err());

        // Flip a scope bit after signing.
        let mut forged = token.clone();
        forged.capability.scope = Scope::files(["secret.txt"]);
        assert!(forged.verify(0).is_err());
    }

    #[test]
    fn expiry_is_enforced() {
        let kp = kp();
        let token = kp.issue(Capability::new(kp.public(), manifest_id()).expiring_at(100));
        assert!(token.verify(99).is_ok());
        assert!(token.verify(100).is_err());
        assert!(token.verify(1_000).is_err());
    }

    #[test]
    fn scope_files_is_canonical_and_enforced() {
        let scope = Scope::files(["b.txt", "a.txt", "b.txt"]);
        assert_eq!(scope, Scope::Files(vec!["a.txt".into(), "b.txt".into()]));
        assert!(scope.allows("a.txt"));
        assert!(!scope.allows("c.txt"));
        assert!(Scope::All.allows("anything"));
    }

    #[test]
    fn each_capability_gets_a_unique_nonce() {
        let kp = kp();
        let a = Capability::new(kp.public(), manifest_id());
        let b = Capability::new(kp.public(), manifest_id());
        assert_ne!(a.nonce, b.nonce);
    }

    #[test]
    fn invite_url_round_trips_and_validates() {
        let kp = kp();
        let cap = Capability::new(kp.public(), manifest_id())
            .with_scope(Scope::files(["mods/a.cfg"]));
        let invite = Invite::new(kp.public(), manifest_id(), "modpack", kp.issue(cap));

        let url = invite.to_url();
        assert!(url.starts_with("gaggle1"));
        let parsed = Invite::parse(&url).unwrap();
        assert_eq!(parsed, invite);
        parsed.validate(0).unwrap();
    }

    #[test]
    fn a_mismatched_invite_wrapper_is_rejected() {
        let kp = kp();
        // credential is for manifest A, invite claims manifest B.
        let token = kp.issue(Capability::new(kp.public(), Hash::of(b"A")));
        let bad = Invite::new(kp.public(), Hash::of(b"B"), "x", token);
        assert!(bad.validate(0).is_err());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(Invite::parse("http://not-an-invite").is_err());
        assert!(Invite::parse("gaggle1!!!!").is_err());
        assert!(Invite::parse("gaggle1YWJj").is_err()); // valid b64, not an invite
    }
}
