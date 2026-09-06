//! [`ReachLink`] — a short, copy-pasteable token carrying just the "reachability"
//! settings (the public relay `…/p2p/<id>` address and the rendezvous / tracker
//! URL), so a user can hand those from one device to another the same way a
//! [`net::ShareLink`](crate::ShareLink) moves a subscription.
//!
//! Same shape as a share link: a `gagglenet1` prefix over a `postcard`-encoded,
//! base64url body — `postcard` keeps it to a few bytes plus the two strings,
//! rather than the overhead a JSON-based token would carry.

use base64::Engine;
use serde::{Deserialize, Serialize};

const PREFIX: &str = "gagglenet1";

/// The subset of [`Settings`](crate::Settings) that describes how other peers
/// reach this node and find each other. Both fields are optional; an all-empty
/// link is rejected on parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachLink {
    /// A relay's dialable `…/p2p/<id>` address — see
    /// [`Settings::public_relay`](crate::Settings::public_relay).
    pub public_relay: Option<String>,
    /// An accelerator's control-plane base URL — see
    /// [`Settings::rendezvous_url`](crate::Settings::rendezvous_url).
    pub rendezvous_url: Option<String>,
}

impl ReachLink {
    /// Build from two raw field values, treating an empty / whitespace-only
    /// string as "not set".
    pub fn from_fields(public_relay: &str, rendezvous_url: &str) -> Self {
        let norm = |s: &str| {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        };
        Self { public_relay: norm(public_relay), rendezvous_url: norm(rendezvous_url) }
    }

    /// True when the link carries neither setting.
    pub fn is_empty(&self) -> bool {
        self.public_relay.is_none() && self.rendezvous_url.is_none()
    }

    /// Encode as a single `gagglenet1<base64url>` token.
    pub fn encode(&self) -> String {
        let bytes = postcard::to_allocvec(self).expect("a ReachLink always serializes");
        format!("{PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Parse a token produced by [`encode`](Self::encode).
    pub fn parse(token: &str) -> anyhow::Result<Self> {
        let body = token.trim().strip_prefix(PREFIX).ok_or_else(|| {
            anyhow::anyhow!("not a Gaggle reachability link (missing `{PREFIX}` prefix)")
        })?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body.as_bytes())
            .map_err(|e| anyhow::anyhow!("reachability link is not valid base64url: {e}"))?;
        let link: ReachLink = postcard::from_bytes(&bytes)
            .map_err(|e| anyhow::anyhow!("reachability link payload is malformed: {e}"))?;
        anyhow::ensure!(!link.is_empty(), "reachability link carries no settings");
        Ok(link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let link = ReachLink {
            public_relay: Some("/ip4/203.0.113.4/udp/4001/quic-v1/p2p/12D3KooExample".into()),
            rendezvous_url: Some("https://relay.example:8749".into()),
        };
        let token = link.encode();
        assert!(token.starts_with("gagglenet1"));
        assert_eq!(ReachLink::parse(&token).unwrap(), link);
    }

    #[test]
    fn round_trips_with_one_field() {
        let link = ReachLink { public_relay: None, rendezvous_url: Some("host:8749".into()) };
        assert_eq!(ReachLink::parse(&link.encode()).unwrap(), link);
    }

    #[test]
    fn from_fields_treats_blank_as_unset() {
        let link = ReachLink::from_fields("  ", "host:8749");
        assert_eq!(link.public_relay, None);
        assert_eq!(link.rendezvous_url.as_deref(), Some("host:8749"));
    }

    #[test]
    fn rejects_junk_and_empty() {
        assert!(ReachLink::parse("hello").is_err());
        assert!(ReachLink::parse("gagglenet1$$$").is_err());
        // A valid encoding of an all-empty link still fails the parse guard.
        assert!(ReachLink::parse(&ReachLink::default().encode()).is_err());
    }
}
