//! The NAT-rendezvous endpoints round-trip a subscriber's request and an
//! origin's answer through a live HTTP server, exactly as the two sides of a
//! relay-free hole-punch would use them.

use control_plane::{PeerInfo, RendezvousClient, RendezvousRegistry, rendezvous_router};

async fn serve(registry: RendezvousRegistry) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, rendezvous_router(registry)).await.unwrap();
    });
    format!("http://{addr}")
}

fn info(id: &str) -> PeerInfo {
    PeerInfo { peer_id: id.to_string(), addrs: vec![format!("/ip4/203.0.113.1/udp/4001/quic-v1/p2p/{id}")] }
}

#[tokio::test]
async fn a_subscriber_and_origin_meet_through_the_service() {
    let base = serve(RendezvousRegistry::new()).await;
    let subscriber = RendezvousClient::new(&base);
    let origin_client = RendezvousClient::new(&base);

    let origin_id = "origin-peer";
    let request_id = subscriber.register(origin_id, &info("sub-peer")).await.unwrap();

    // Not answered yet.
    assert!(subscriber.poll_answer(origin_id, &request_id).await.unwrap().is_none());

    // Origin polls, finds the request, and answers it.
    let pending = origin_client.pending(origin_id).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request_id, request_id);
    assert_eq!(pending[0].subscriber.peer_id, "sub-peer");
    origin_client.answer(origin_id, &request_id, &info(origin_id)).await.unwrap();

    // Now the subscriber's poll sees the answer, and the origin no longer
    // lists it as pending.
    let answer = subscriber.poll_answer(origin_id, &request_id).await.unwrap().unwrap();
    assert_eq!(answer.peer_id, origin_id);
    assert!(origin_client.pending(origin_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn polling_an_unknown_request_fails() {
    let base = serve(RendezvousRegistry::new()).await;
    let client = RendezvousClient::new(&base);
    assert!(client.poll_answer("nobody", "no-such-request").await.is_err());
}

#[tokio::test]
async fn answering_an_unknown_request_fails() {
    let base = serve(RendezvousRegistry::new()).await;
    let client = RendezvousClient::new(&base);
    assert!(client.answer("nobody", "no-such-request", &info("x")).await.is_err());
}

#[tokio::test]
async fn two_subscribers_can_wait_on_the_same_origin_at_once() {
    let base = serve(RendezvousRegistry::new()).await;
    let a = RendezvousClient::new(&base);
    let b = RendezvousClient::new(&base);
    let origin_client = RendezvousClient::new(&base);
    let origin_id = "origin-peer";

    let req_a = a.register(origin_id, &info("sub-a")).await.unwrap();
    let req_b = b.register(origin_id, &info("sub-b")).await.unwrap();
    assert_ne!(req_a, req_b);

    let pending = origin_client.pending(origin_id).await.unwrap();
    assert_eq!(pending.len(), 2);

    origin_client.answer(origin_id, &req_a, &info(origin_id)).await.unwrap();
    assert!(a.poll_answer(origin_id, &req_a).await.unwrap().is_some());
    assert!(b.poll_answer(origin_id, &req_b).await.unwrap().is_none());
}
