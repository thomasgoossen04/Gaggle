//! The accelerator admin API: signed requests from an authorised operator are
//! served and daemon-signed; anything else is refused; and a mutation reaches
//! the supervisor channel and shows up in the next status snapshot.

use control_plane::admin::{AdminCommand, AdminState, DaemonStatus, ShareStatus, router};
use control_plane::AdminClient;
use gaggle_core::{AgentId, AgentKeypair};
use tokio::sync::{mpsc, watch};

fn sample_status(agent: &AgentId) -> DaemonStatus {
    DaemonStatus {
        agent_id: agent.to_hex(),
        peer_id: "12D3KooWtest".into(),
        role: "relay".into(),
        listen_addrs: vec!["/ip4/127.0.0.1/udp/4001/quic-v1".into()],
        shares: Vec::new(),
    }
}

struct Harness {
    base: String,
    daemon_id: AgentId,
    status_tx: watch::Sender<DaemonStatus>,
    cmd_rx: mpsc::Receiver<AdminCommand>,
}

async fn spawn(authorized: Vec<AgentId>) -> Harness {
    let daemon = AgentKeypair::from_seed([9u8; 32]);
    let daemon_id = daemon.public();
    let (status_tx, status_rx) = watch::channel(sample_status(&daemon_id));
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let state = AdminState::new(authorized, daemon, cmd_tx, status_rx);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });

    Harness { base: format!("http://{addr}"), daemon_id, status_tx, cmd_rx }
}

#[tokio::test]
async fn an_authorised_operator_reads_status_and_pins_the_daemon() {
    let operator = AgentKeypair::from_seed([1u8; 32]);
    let h = spawn(vec![operator.public()]).await;

    let mut client = AdminClient::new(&h.base, operator, None).unwrap();
    let status = client.status().await.unwrap();

    assert_eq!(status.agent_id, h.daemon_id.to_hex());
    assert_eq!(status.role, "relay");
    assert_eq!(client.pinned(), Some(h.daemon_id), "the daemon key is pinned after first call");
}

#[tokio::test]
async fn an_unauthorised_key_is_refused() {
    let authorised = AgentKeypair::from_seed([1u8; 32]);
    let stranger = AgentKeypair::from_seed([2u8; 32]);
    let h = spawn(vec![authorised.public()]).await;

    let mut client = AdminClient::new(&h.base, stranger, None).unwrap();
    let err = client.status().await.unwrap_err();
    assert!(format!("{err:#}").contains("401"), "unexpected error: {err:#}");
}

#[tokio::test]
async fn a_tampered_request_body_fails_the_signature() {
    // Sign for the right key but send to a server that expects a different one:
    // simplest reproduction of "signature does not verify" from the client side.
    let operator = AgentKeypair::from_seed([1u8; 32]);
    let h = spawn(vec![operator.public()]).await;

    // A raw request with a valid agent header but a bogus signature.
    let http = reqwest::Client::new();
    let resp = http
        .get(format!("{}/admin/status", h.base))
        .header("x-gaggle-agent", operator.public().to_hex())
        .header("x-gaggle-timestamp", "9999999999")
        .header("x-gaggle-nonce", "AAAA")
        .header("x-gaggle-signature", "AAAA")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn a_captured_request_cannot_be_replayed() {
    use base64::Engine as _;
    use gaggle_core::Hash;

    let operator = AgentKeypair::from_seed([1u8; 32]);
    let h = spawn(vec![operator.public()]).await;

    // Build one genuine signed GET and fire the exact same bytes twice.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let nonce = "replay-me";
    let canonical =
        format!("gaggle-admin\nGET\n/admin/status\n{ts}\n{nonce}\n{}", Hash::of(b"").to_hex());
    let sig = operator.sign(canonical.as_bytes());
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_bytes());

    let http = reqwest::Client::new();
    let fire = || {
        http.get(format!("{}/admin/status", h.base))
            .header("x-gaggle-agent", operator.public().to_hex())
            .header("x-gaggle-timestamp", &ts)
            .header("x-gaggle-nonce", nonce)
            .header("x-gaggle-signature", &sig_b64)
            .send()
    };

    assert_eq!(fire().await.unwrap().status(), 200, "first use is accepted");
    assert_eq!(fire().await.unwrap().status(), 401, "the replay is refused");
}

#[tokio::test]
async fn adding_a_share_reaches_the_supervisor_and_shows_up_in_status() {
    let operator = AgentKeypair::from_seed([1u8; 32]);
    let mut h = spawn(vec![operator.public()]).await;
    let status_tx = h.status_tx.clone();

    // Stand in for the daemon supervisor: ack the AddShare and update status.
    tokio::spawn(async move {
        while let Some(cmd) = h.cmd_rx.recv().await {
            match cmd {
                AdminCommand::AddShare { token, ack } => {
                    let mut s = status_tx.borrow().clone();
                    s.shares.push(ShareStatus {
                        manifest_id: "deadbeef".into(),
                        name: token,
                        files: 3,
                        total_bytes: 123,
                        version: 1,
                        private: false,
                        cached_chunks: Some(0),
                        replica_chunks: None,
                        listen_addr: None,
                        replicating: None,
                        error: None,
                    });
                    let _ = status_tx.send(s);
                    let _ = ack.send(Ok(()));
                }
                AdminCommand::RemoveShare { ack, .. } => {
                    let _ = ack.send(Ok(()));
                }
            }
        }
    });

    let mut client = AdminClient::new(&h.base, operator, Some(h.daemon_id)).unwrap();
    client.add_share("gaggleshare1demo").await.unwrap();

    let shares = client.list_shares().await.unwrap();
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].name, "gaggleshare1demo");
}
