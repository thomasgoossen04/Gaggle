//! The admin API served for real, over TLS (`admin::serve`, not the bare
//! `axum::serve` the other admin tests use to isolate the signature scheme):
//! an `AdminClient` pins the daemon's TLS-presented identity on first
//! contact, refuses a connection to an impostor, and (via `serve_daemon`)
//! shares its listener with the unauthenticated rendezvous routes without
//! either leaking the other's trust model.

use control_plane::admin::{AdminState, DaemonStatus};
use control_plane::{AdminClient, PeerInfo, RendezvousClient, RendezvousRegistry, serve_daemon};
use gaggle_core::{AgentId, AgentKeypair};
use tokio::sync::{mpsc, watch};

fn sample_status(agent: &AgentId) -> DaemonStatus {
    DaemonStatus {
        agent_id: agent.to_hex(),
        peer_id: "12D3KooWtest".into(),
        role: "relay".into(),
        listen_addrs: vec![],
        shares: Vec::new(),
    }
}

/// Spin up the real TLS-terminated admin API (`admin::serve`) and return its
/// `https://` base plus the daemon's identity.
async fn spawn_admin_tls(authorized: Vec<AgentId>) -> (String, AgentId) {
    let daemon = AgentKeypair::from_seed([9u8; 32]);
    let daemon_id = daemon.public();
    let (_status_tx, status_rx) = watch::channel(sample_status(&daemon_id));
    let (cmd_tx, _cmd_rx) = mpsc::channel(8);
    let state = AdminState::new(authorized, daemon, cmd_tx, status_rx);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        control_plane::admin::serve(listener, state).await.unwrap();
    });
    (format!("https://{addr}"), daemon_id)
}

#[tokio::test]
async fn a_client_pins_the_daemon_over_a_real_tls_connection() {
    let operator = AgentKeypair::from_seed([1u8; 32]);
    let (base, daemon_id) = spawn_admin_tls(vec![operator.public()]).await;

    let mut client = AdminClient::new(&base, operator, None).unwrap();
    let status = client.status().await.unwrap();

    assert_eq!(status.agent_id, daemon_id.to_hex());
    assert_eq!(client.pinned(), Some(daemon_id), "TLS handshake pinned the daemon's identity");
}

#[tokio::test]
async fn a_pin_for_a_different_daemon_is_refused() {
    let operator = AgentKeypair::from_seed([1u8; 32]);
    let (base, _real_daemon_id) = spawn_admin_tls(vec![operator.public()]).await;
    let impostor = AgentKeypair::generate().public();

    let mut client = AdminClient::new(&base, operator, Some(impostor)).unwrap();
    let err = client.status().await.unwrap_err();
    assert!(
        format!("{err:#}").contains("identity changed"),
        "expected a pin mismatch, got: {err:#}"
    );
}

#[tokio::test]
async fn admin_and_rendezvous_share_one_tls_listener_with_separate_trust_models() {
    let operator = AgentKeypair::from_seed([1u8; 32]);
    let daemon = AgentKeypair::from_seed([9u8; 32]);
    let daemon_id = daemon.public();
    let (_status_tx, status_rx) = watch::channel(sample_status(&daemon_id));
    let (cmd_tx, _cmd_rx) = mpsc::channel(8);
    let state = AdminState::new(vec![operator.public()], daemon, cmd_tx, status_rx);
    let rendezvous = RendezvousRegistry::new();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        serve_daemon(listener, state, rendezvous, None).await.unwrap();
    });
    let base = format!("https://{addr}");

    // Admin: signed, pinned.
    let mut admin = AdminClient::new(&base, operator, None).unwrap();
    assert_eq!(admin.status().await.unwrap().agent_id, daemon_id.to_hex());

    // Rendezvous: unauthenticated, unpinned, same TLS port.
    let rendezvous_client = RendezvousClient::new(&base);
    let me = PeerInfo { peer_id: "sub".into(), addrs: vec!["/ip4/203.0.113.1/udp/1/quic-v1".into()] };
    let request_id = rendezvous_client.register("origin", &me).await.unwrap();
    let pending = rendezvous_client.pending("origin").await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request_id, request_id);
}

/// The split-listener case: admin on one address, rendezvous on a different
/// one entirely (e.g. admin behind a VPN, rendezvous on a public port) — the
/// operator's actual motivating use case for `rendezvous_listener`.
#[tokio::test]
async fn admin_and_rendezvous_can_run_on_separate_listeners() {
    let operator = AgentKeypair::from_seed([1u8; 32]);
    let daemon = AgentKeypair::from_seed([9u8; 32]);
    let daemon_id = daemon.public();
    let (_status_tx, status_rx) = watch::channel(sample_status(&daemon_id));
    let (cmd_tx, _cmd_rx) = mpsc::channel(8);
    let state = AdminState::new(vec![operator.public()], daemon, cmd_tx, status_rx);
    let rendezvous = RendezvousRegistry::new();

    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_addr = admin_listener.local_addr().unwrap();
    let rendezvous_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rendezvous_addr = rendezvous_listener.local_addr().unwrap();
    assert_ne!(admin_addr.port(), rendezvous_addr.port(), "test needs two distinct ports");

    tokio::spawn(async move {
        serve_daemon(admin_listener, state, rendezvous, Some(rendezvous_listener)).await.unwrap();
    });

    let mut admin = AdminClient::new(format!("https://{admin_addr}"), operator, None).unwrap();
    assert_eq!(admin.status().await.unwrap().agent_id, daemon_id.to_hex());

    let rendezvous_client = RendezvousClient::new(format!("https://{rendezvous_addr}"));
    let me = PeerInfo { peer_id: "sub".into(), addrs: vec!["/ip4/203.0.113.1/udp/1/quic-v1".into()] };
    let request_id = rendezvous_client.register("origin", &me).await.unwrap();
    let pending = rendezvous_client.pending("origin").await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request_id, request_id);

    // The admin port doesn't answer rendezvous requests, and vice versa.
    assert!(
        RendezvousClient::new(format!("https://{admin_addr}")).pending("origin").await.is_err(),
        "the admin listener should not also serve rendezvous"
    );
}
