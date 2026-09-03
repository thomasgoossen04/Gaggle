//! Milestone 7: an invite published to the control-plane service comes back
//! intact by its code, and the embedded credential still validates.

use std::time::{SystemTime, UNIX_EPOCH};

use control_plane::{InviteClient, InviteRegistry, invite_router};
use gaggle_core::{Capability, Hash, Invite, ShareKeypair};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn sample_invite() -> (Invite, ShareKeypair) {
    let kp = ShareKeypair::from_seed([5u8; 32]);
    let manifest_id = Hash::of(b"a private modpack manifest");
    let cred = kp.issue(Capability::new(kp.public(), manifest_id));
    (Invite::new(kp.public(), manifest_id, "modpack", cred), kp)
}

async fn serve(registry: InviteRegistry) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, invite_router(registry)).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn invite_round_trips_through_the_service() {
    let registry = InviteRegistry::new();
    let base = serve(registry.clone()).await;
    let client = InviteClient::new(&base);

    let (invite, _kp) = sample_invite();
    let code = client.publish(&invite).await.unwrap();
    assert_eq!(registry.len(), 1);

    let fetched = client.fetch(&code).await.unwrap();
    assert_eq!(fetched, invite);
    fetched.validate(now()).unwrap();
}

#[tokio::test]
async fn an_unknown_code_is_a_404() {
    let base = serve(InviteRegistry::new()).await;
    let client = InviteClient::new(&base);
    assert!(client.fetch("deadbeef00000000deadbeef").await.is_err());
}

#[tokio::test]
async fn an_invite_with_a_bad_signature_is_rejected() {
    let base = serve(InviteRegistry::new()).await;
    let client = InviteClient::new(&base);

    let (mut invite, _kp) = sample_invite();
    // Tamper after signing.
    invite.credential.capability.manifest_id = Hash::of(b"a different manifest");
    assert!(client.publish(&invite).await.is_err());
}
