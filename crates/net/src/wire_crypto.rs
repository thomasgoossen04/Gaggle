//! Per-chunk wire compression + encryption.
//!
//! [`seal`] / [`open`] transform one chunk's bytes at a time — compress (if it
//! actually shrinks) then encrypt on the way out, decrypt then decompress on
//! the way in — so a share never needs a whole-share pre-pass before it can
//! start streaming; this runs once per [`crate::proto::Response::Chunk`] as it
//! is framed by [`crate::codec`], on top of a transport (libp2p QUIC) that is
//! already TLS-encrypted connection-to-connection.
//!
//! The key is a fixed value baked into every build, not a per-share or
//! per-user secret. That means it is **not** confidentiality against anyone
//! who has (or reverse-engineers) the Gaggle binary — it does not hide content
//! from a relay or NAS accelerator either, which must decrypt to verify and
//! cache chunks anyway. Real access control for private shares is the
//! invite/capability system (`gaggle_core::invite`), which this does not
//! change. What this buys, cheaply: raw file bytes never sit in a wire frame
//! in the clear, so a packet capture, a naive proxy, or a logging tool sees
//! only ciphertext — and compressible content costs fewer bytes on the wire.
//!
//! [`XChaCha20Poly1305`]'s 192-bit nonce is generated fresh per chunk from the
//! OS RNG: with a random nonce this large, collisions across the lifetime of
//! every Gaggle chunk ever sent are not a realistic concern (unlike the 96-bit
//! nonce of plain ChaCha20-Poly1305/AES-GCM, which needs a counter to be safe
//! at this key's scale — one static key shared by every install).

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

/// Baked into every build; see the module docs for why that's fine here and
/// what it doesn't protect against. Not derived from anything — swap it for a
/// build-time secret only if the trust model above is revisited too.
const GLOBAL_KEY: [u8; 32] = [
    0x6e, 0xad, 0xb1, 0xff, 0x03, 0x16, 0x51, 0xe4, 0x90, 0x8d, 0x2e, 0x20, 0xc0, 0x00, 0x16, 0x97,
    0x44, 0x66, 0x1a, 0x48, 0x70, 0x23, 0x7b, 0x62, 0x8a, 0x48, 0x04, 0x95, 0x8e, 0x80, 0x40, 0x47,
];

/// Binds ciphertext to "this is a sealed chunk" so it can't be replayed as some
/// other framed value; bumping this invalidates every previously-sealed chunk,
/// which is the point if the format ever changes.
const AAD: &[u8] = b"gaggle/chunk/1";

const FLAG_COMPRESSED: u8 = 1 << 0;

const NONCE_LEN: usize = 24;

fn cipher() -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new((&GLOBAL_KEY).into())
}

/// Compress `plaintext` (if that would shrink it) and encrypt it for the wire.
/// Returns `flags(1 byte) || nonce(24 bytes) || ciphertext`.
pub fn seal(plaintext: &[u8]) -> Vec<u8> {
    let compressed = lz4_flex::compress_prepend_size(plaintext);
    let (flags, payload): (u8, &[u8]) =
        if compressed.len() < plaintext.len() { (FLAG_COMPRESSED, &compressed) } else { (0, plaintext) };

    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher()
        .encrypt(&nonce, Payload { msg: payload, aad: AAD })
        .expect("encryption with a fixed-size key and nonce cannot fail");

    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(flags);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

/// Reverse of [`seal`].
pub fn open(sealed: &[u8]) -> Result<Vec<u8>, String> {
    let (&flags, rest) = sealed.split_first().ok_or("empty sealed chunk")?;
    if rest.len() < NONCE_LEN {
        return Err("sealed chunk is missing its nonce".into());
    }
    let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce = XNonce::from_slice(nonce_bytes);

    let payload = cipher()
        .decrypt(nonce, Payload { msg: ciphertext, aad: AAD })
        .map_err(|_| "chunk failed authentication (corrupt or tampered in transit)".to_string())?;

    if flags & FLAG_COMPRESSED != 0 {
        lz4_flex::decompress_size_prepended(&payload).map_err(|e| format!("chunk decompression failed: {e}"))
    } else {
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use rand::RngCore;

    use super::*;

    fn random_bytes(len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        rand::thread_rng().fill_bytes(&mut buf);
        buf
    }

    #[test]
    fn round_trips_incompressible_data() {
        let data = random_bytes(10_000);
        assert_eq!(open(&seal(&data)).unwrap(), data);
    }

    #[test]
    fn round_trips_highly_compressible_data() {
        let data = vec![7u8; 1_000_000];
        let sealed = seal(&data);
        assert!(sealed.len() < data.len() / 10, "expected compression to shrink a uniform buffer");
        assert_eq!(open(&sealed).unwrap(), data);
    }

    #[test]
    fn round_trips_empty_chunk() {
        assert_eq!(open(&seal(&[])).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn skips_compression_when_it_does_not_help() {
        let data = random_bytes(4096);
        let sealed = seal(&data);
        assert_eq!(sealed[0] & FLAG_COMPRESSED, 0, "random data shouldn't be marked compressed");
    }

    #[test]
    fn each_seal_uses_a_fresh_nonce() {
        let data = b"same plaintext, different wire bytes each time";
        let a = seal(data);
        let b = seal(data);
        assert_ne!(a, b, "reusing a nonce with a static key would be a real vulnerability");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let mut sealed = seal(b"hello gaggle");
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(open(&sealed).is_err());
    }

    #[test]
    fn truncated_frame_is_rejected() {
        assert!(open(&[]).is_err());
        assert!(open(&[0u8; 5]).is_err());
    }
}
