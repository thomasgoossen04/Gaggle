//! [`ShareLink`] — the copy-pasteable string that lets one node subscribe to
//! another's share. It bundles a [`gaggle_core::Invite`] (for a private share)
//! with what an invite lacks: the network address(es) to reach a seed at.
//!
//! It lives in `net` rather than a frontend crate because the `accelerator`
//! daemon (its config file) and `control-plane` (the admin API body) both round
//! -trip these tokens.

use base64::Engine;
use gaggle_core::{Hash, Invite};
use serde::{Deserialize, Serialize};

use crate::Multiaddr;

const PREFIX: &str = "gaggleshare1";

/// Everything a subscriber needs, in one `gaggleshare1<base64url>` token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShareLink {
    pub name: String,
    pub manifest_id: Hash,
    /// Dialable `…/p2p/<id>` addresses of seeds.
    pub sources: Vec<Multiaddr>,
    /// Present for a private share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<Invite>,
}

impl ShareLink {
    pub fn new(name: impl Into<String>, manifest_id: Hash, sources: Vec<Multiaddr>) -> Self {
        Self { name: name.into(), manifest_id, sources, invite: None }
    }

    pub fn with_invite(mut self, invite: Invite) -> Self {
        self.invite = Some(invite);
        self
    }

    /// The capability token embedded in the link, if it is for a private share.
    pub fn credential(&self) -> Option<&gaggle_core::SignedCapability> {
        self.invite.as_ref().map(|i| &i.credential)
    }

    /// Encode as a single `gaggleshare1<base64url>` token.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("a ShareLink always serializes");
        format!("{PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
    }

    /// Parse a token produced by [`encode`](Self::encode).
    pub fn parse(token: &str) -> anyhow::Result<Self> {
        let body = token
            .trim()
            .strip_prefix(PREFIX)
            .ok_or_else(|| anyhow::anyhow!("not a Gaggle share link (missing `{PREFIX}` prefix)"))?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body.as_bytes())
            .map_err(|e| anyhow::anyhow!("share link is not valid base64url: {e}"))?;
        let link: ShareLink = serde_json::from_slice(&json)
            .map_err(|e| anyhow::anyhow!("share link payload is malformed: {e}"))?;
        anyhow::ensure!(!link.sources.is_empty(), "share link names no sources");
        Ok(link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gaggle_core::{Capability, ShareKeypair};

    fn addr() -> Multiaddr {
        let peer = crate::PeerId::random();
        format!("/ip4/127.0.0.1/udp/40521/quic-v1/p2p/{peer}").parse().unwrap()
    }

    #[test]
    fn round_trips_public() {
        let link = ShareLink::new("modpack", Hash::of(b"m"), vec![addr()]);
        let token = link.encode();
        assert!(token.starts_with("gaggleshare1"));
        assert_eq!(ShareLink::parse(&token).unwrap(), link);
    }

    #[test]
    fn round_trips_with_invite() {
        let kp = ShareKeypair::from_seed([1u8; 32]);
        let mid = Hash::of(b"m");
        let invite = Invite::new(kp.public(), mid, "mp", kp.issue(Capability::new(kp.public(), mid)));
        let link = ShareLink::new("mp", mid, vec![addr()]).with_invite(invite);

        let back = ShareLink::parse(&link.encode()).unwrap();
        assert_eq!(back, link);
        assert!(back.credential().is_some());
    }

    #[test]
    fn rejects_junk() {
        assert!(ShareLink::parse("hello").is_err());
        assert!(ShareLink::parse("gaggleshare1$$$").is_err());
    }
}
