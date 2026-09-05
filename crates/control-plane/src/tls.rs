//! TLS for the admin API.
//!
//! The daemon terminates TLS with a self-signed certificate generated
//! directly from its own [`AgentKeypair`] — one identity, not a second PKI to
//! generate, persist, and keep in sync. [`admin::AdminClient`](crate::admin::AdminClient)
//! pins that certificate's embedded key the same way it already pins the
//! daemon's signing key on responses: trust it on the first connection, then
//! refuse to talk to anyone else. TLS closes the confidentiality gap plain
//! signed-HTTP left open — request/response bodies (including private-share
//! invite tokens passed to `POST /admin/shares`) used to travel in the clear;
//! the existing request/response signature scheme (`admin.rs`) stays in place
//! on top, so a compromised or misconfigured TLS terminator still can't forge
//! a mutation or a status response.
//!
//! [`PinningVerifier`] shares one `Arc<Mutex<Option<AgentId>>>` with
//! `AdminClient`'s app-layer check, so exactly one pin governs both layers —
//! a TLS-valid-but-wrong-signer response (or vice versa) is rejected by
//! whichever layer notices first.
//!
//! Both sides build their `rustls` config with an explicit
//! [`rustls::crypto::CryptoProvider`] (`ring`, matching what `rcgen` already
//! uses internally to sign the certificate) rather than touching the
//! process-wide default: this crate can end up in the same process as a
//! `reqwest` client that installs its own (`aws-lc-rs`) default, and two
//! independently, explicitly-scoped configs coexist fine as long as neither
//! one reaches for the ambient default.

use std::sync::{Arc, Mutex};

use gaggle_core::{AgentId, AgentKeypair};
use rcgen::{CertificateParams, KeyPair};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, ServerConfig, SignatureScheme};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// RFC 8410 PKCS#8 v1 encoding of a raw 32-byte Ed25519 seed: a fixed header
/// (Ed25519's `AlgorithmIdentifier` carries no parameters, and both nested
/// octet-string lengths are fixed by the key size, so every encoder produces
/// exactly these 16 bytes) followed by the seed itself.
const PKCS8_ED25519_PREFIX: [u8; 16] =
    [0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20];

fn ed25519_pkcs8_der(seed: [u8; 32]) -> Vec<u8> {
    let mut der = Vec::with_capacity(PKCS8_ED25519_PREFIX.len() + seed.len());
    der.extend_from_slice(&PKCS8_ED25519_PREFIX);
    der.extend_from_slice(&seed);
    der
}

/// Build a self-signed end-entity certificate whose public key *is* `agent`'s
/// identity, plus its matching private key.
fn self_signed_cert(agent: &AgentKeypair) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let der = ed25519_pkcs8_der(agent.to_seed());
    let key_pair =
        KeyPair::from_pkcs8_der_and_sign_algo(&PrivatePkcs8KeyDer::from(der.clone()), &rcgen::PKCS_ED25519)?;
    let params = CertificateParams::new(vec!["gaggle-accelerator".to_string()])?;
    let cert = params.self_signed(&key_pair)?;
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der))))
}

/// A `rustls::ServerConfig` for `axum_server::tls_rustls::RustlsConfig::from_config`,
/// terminating TLS with `agent`'s own identity key.
pub fn server_config(agent: &AgentKeypair) -> anyhow::Result<ServerConfig> {
    let (cert, key) = self_signed_cert(agent)?;
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    Ok(config)
}

/// Read the raw Ed25519 public key out of a certificate's SPKI. Errors only
/// on an unparseable certificate — a key *mismatch* is the caller's concern.
fn cert_agent_id(cert: &CertificateDer<'_>) -> Result<AgentId, TlsError> {
    let bad = || TlsError::InvalidCertificate(CertificateError::BadEncoding);
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref()).map_err(|_| bad())?;
    let spki = parsed.public_key().subject_public_key.data.as_ref();
    let bytes: [u8; 32] = spki.try_into().map_err(|_| bad())?;
    Ok(AgentId::from_bytes(bytes))
}

/// Trust-on-first-use server-certificate verification: the first certificate
/// seen is accepted and becomes the pin; every later one must match exactly.
#[derive(Debug)]
struct PinningVerifier {
    pinned: Arc<Mutex<Option<AgentId>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let observed = cert_agent_id(end_entity)?;
        let mut pinned = self.pinned.lock().unwrap_or_else(|e| e.into_inner());
        match *pinned {
            Some(expected) if expected != observed => Err(TlsError::General(format!(
                "daemon TLS identity changed — expected {expected}, saw {observed}"
            ))),
            Some(_) => Ok(ServerCertVerified::assertion()),
            None => {
                *pinned = Some(observed);
                Ok(ServerCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A `rustls::ClientConfig` for `reqwest::ClientBuilder::use_preconfigured_tls`
/// that trusts no CA at all and instead pins the server's identity key,
/// sharing `pinned` with the caller's own app-layer check.
pub(crate) fn client_config(pinned: Arc<Mutex<Option<AgentId>>>) -> anyhow::Result<ClientConfig> {
    let verifier = PinningVerifier { pinned, provider: provider() };
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(config)
}

/// Accepts any certificate that carries a genuine Ed25519 signature, but pins
/// nothing — for the NAT-rendezvous endpoints (`rendezvous.rs`), which are
/// deliberately unauthenticated by design (any subscriber may need to reach
/// one with no prior relationship to its operator) and share the admin API's
/// TLS listener only because they share its port. Still gives
/// eavesdropper-proof transport for the candidate addresses exchanged there;
/// gives no protection against an active on-path attacker, same as the
/// endpoints it fronts had none before TLS either.
#[derive(Debug)]
struct AnyServerVerifier {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AnyServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        cert_agent_id(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A `rustls::ClientConfig` for [`RendezvousClient`](crate::rendezvous::RendezvousClient) — see [`AnyServerVerifier`].
pub(crate) fn rendezvous_client_config() -> anyhow::Result<ClientConfig> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyServerVerifier { provider: provider() }))
        .with_no_client_auth();
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_key_matches_agent_identity() {
        let agent = AgentKeypair::generate();
        let (cert, _key) = self_signed_cert(&agent).unwrap();
        assert_eq!(cert_agent_id(&cert).unwrap(), agent.public());
    }

    #[test]
    fn server_config_builds() {
        let agent = AgentKeypair::generate();
        server_config(&agent).unwrap();
    }

    #[test]
    fn pinning_verifier_trusts_first_then_pins() {
        let a = AgentKeypair::generate();
        let b = AgentKeypair::generate();
        let (cert_a, _) = self_signed_cert(&a).unwrap();
        let (cert_b, _) = self_signed_cert(&b).unwrap();
        let verifier = PinningVerifier { pinned: Arc::new(Mutex::new(None)), provider: provider() };
        let name = ServerName::try_from("gaggle-accelerator").unwrap();

        verifier.verify_server_cert(&cert_a, &[], &name, &[], UnixTime::now()).unwrap();
        assert!(
            verifier
                .verify_server_cert(&cert_b, &[], &name, &[], UnixTime::now())
                .is_err()
        );
        verifier.verify_server_cert(&cert_a, &[], &name, &[], UnixTime::now()).unwrap();
    }
}
