//! Release-descriptor signature verification.
//!
//! Every `latest.json` the launcher fetches over the network is accompanied by
//! a detached Ed25519 signature at `<descriptor-url>.sig` (128 lowercase-hex
//! chars), produced in CI from the `GAGGLE_RELEASE_SIGNING_KEY` secret. The
//! matching public key is compiled in below. A remote descriptor whose
//! signature is missing or does not verify is rejected *before* it can decide
//! which archive gets downloaded, unzipped, and executed — TLS to GitHub alone
//! doesn't cover a compromised release asset, a tampered descriptor, or an
//! `http://` override.

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, VerifyingKey};

/// Ed25519 public key (hex) whose secret half signs `latest.json` in CI.
/// Rotating it means replacing this constant and cutting a release signed with
/// the new key; clients on the old build keep trusting the old key until they
/// update through a build that carries the new one.
pub const RELEASE_PUBLIC_KEY_HEX: &str =
    "19a931eafebeab938ec964b209d38f42cf4410ac1753bd2ca3910c83720e7b2b";

/// Domain tag prepended to the descriptor bytes before signing / verifying, so
/// a signature over these bytes can't be replayed as anything else.
const DOMAIN: &[u8] = b"gaggle-release-descriptor-v1\n";

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn verifying_key() -> VerifyingKey {
    let bytes: [u8; 32] = unhex(RELEASE_PUBLIC_KEY_HEX)
        .expect("compiled-in release key is hex")
        .try_into()
        .expect("compiled-in release key is 32 bytes");
    VerifyingKey::from_bytes(&bytes).expect("compiled-in release key is a valid Ed25519 point")
}

/// Verify `sig_hex` (128 hex chars) as a detached signature over `descriptor`
/// (the raw `latest.json` bytes) under the embedded release key.
pub fn verify(descriptor: &[u8], sig_hex: &str) -> Result<()> {
    let raw = unhex(sig_hex).ok_or_else(|| anyhow!("release signature is not valid hex"))?;
    let bytes: [u8; 64] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("release signature is {} bytes, expected 64", raw.len()))?;
    let sig = Signature::from_bytes(&bytes);

    let mut msg = Vec::with_capacity(DOMAIN.len() + descriptor.len());
    msg.extend_from_slice(DOMAIN);
    msg.extend_from_slice(descriptor);

    verifying_key()
        .verify_strict(&msg, &sig)
        .context("release descriptor signature does not verify")
}

/// Reject any signature requirement bypass early: a remote descriptor URL must
/// be `https`. `file://` and bare local paths are for local testing and are
/// treated as trusted (they aren't a network attack surface).
pub fn require_https_if_remote(url: &str) -> Result<()> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.contains("://") && !lower.starts_with("https://") && !lower.starts_with("file://") {
        bail!("refusing to fetch {url} — the update descriptor and assets must be served over https");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A pinned real vector: the signature of `DOMAIN || SAMPLE_BODY` under the
    // secret half of `RELEASE_PUBLIC_KEY_HEX`. Regenerate with
    // `.github/scripts/sign_latest.py` if the release key is ever rotated.
    const SAMPLE_BODY: &[u8] = b"{\"hello\":\"world\"}";
    const SAMPLE_SIG: &str = "9ebc53a31eded7276861341f9bcde7318b1825050f13dff89c5683c62cb457c1481fec7fc04b1ab2ff2eea1354cb68f5eff41721ca9d323e6e66a5ed6487270f";

    #[test]
    fn accepts_a_genuine_signature() {
        verify(SAMPLE_BODY, SAMPLE_SIG).unwrap();
    }

    #[test]
    fn rejects_tampered_body_wrong_length_and_non_hex() {
        assert!(verify(b"{\"hello\":\"WORLD\"}", SAMPLE_SIG).is_err());
        assert!(verify(SAMPLE_BODY, "zzzz").is_err());
        assert!(verify(SAMPLE_BODY, "abcd").is_err());
        assert!(verify(SAMPLE_BODY, &"00".repeat(64)).is_err());
    }

    #[test]
    fn require_https_if_remote_allows_local_and_https_only() {
        assert!(require_https_if_remote("https://example.com/latest.json").is_ok());
        assert!(require_https_if_remote("file:///tmp/latest.json").is_ok());
        assert!(require_https_if_remote("/tmp/latest.json").is_ok());
        assert!(require_https_if_remote("latest.json").is_ok());
        assert!(require_https_if_remote("http://example.com/latest.json").is_err());
        assert!(require_https_if_remote("ftp://example.com/latest.json").is_err());
    }
}
