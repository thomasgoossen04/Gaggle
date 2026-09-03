//! Milestone 2 end-to-end: snapshot a folder on one side, pull it back over a
//! loopback QUIC connection on the other, and prove every file reconstructs
//! byte-for-byte from the downloaded chunks.

use std::fs;

use gaggle_core::{ChunkStore, Hash, MemoryChunkStore, snapshot_dir};
use net::{Catalog, Client, Request, Response, ServerHandle, download_share};
use tempfile::TempDir;

/// A small share with an empty dir, a couple of tiny files, and one file large
/// enough to split into several content-defined chunks.
fn sample_share() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("mods")).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("readme.txt"), b"gaggle loopback transfer test\n").unwrap();
    fs::write(root.join("mods/game.cfg"), b"fov=110\nvsync=0\n").unwrap();

    let mut blob = Vec::with_capacity(6 * 1024 * 1024);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    while blob.len() < 6 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(root.join("mods/textures.pak"), &blob).unwrap();
    dir
}

async fn serve(share: &TempDir) -> (ServerHandle, gaggle_core::Manifest) {
    let mut store = MemoryChunkStore::new();
    let snapshot = snapshot_dir(share.path(), "loopback-share", 1, &mut store).unwrap();
    let manifest = snapshot.manifest.clone();
    let catalog = Catalog::new(snapshot.manifest, snapshot.chunk_lists, store);
    (ServerHandle::spawn(catalog).await.unwrap(), manifest)
}

#[tokio::test]
async fn loopback_quic_transfer_reconstructs_the_share() {
    let share = sample_share();
    let (server, origin_manifest) = serve(&share).await;

    let client = Client::connect(server.listen_addr.clone()).await.unwrap();
    let mut store = MemoryChunkStore::new();
    let got = download_share(&client, &mut store).await.unwrap();

    assert_eq!(got.manifest, origin_manifest, "manifest survived the round trip");
    assert_eq!(got.chunk_lists.len(), origin_manifest.files.len());
    assert!(
        got.chunk_lists.values().any(|list| list.len() > 1),
        "the large file should have chunked into more than one piece"
    );

    for file in &origin_manifest.files {
        let list = &got.chunk_lists[&file.path];
        let mut rebuilt = Vec::new();
        for chunk in &list.chunks {
            let bytes = store.get(&chunk.hash).expect("chunk missing from the store after download");
            assert_eq!(Hash::of(&bytes), chunk.hash, "stored chunk content-addresses wrong");
            rebuilt.extend_from_slice(&bytes);
        }
        assert_eq!(rebuilt.len() as u64, file.size);
        let original = fs::read(share.path().join(&file.path)).unwrap();
        assert_eq!(rebuilt, original, "{} did not reconstruct byte-for-byte", file.path);
    }

    client.shutdown().await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_second_download_reuses_the_local_store() {
    let share = sample_share();
    let (server, _) = serve(&share).await;

    let client = Client::connect(server.listen_addr.clone()).await.unwrap();

    let mut store = MemoryChunkStore::new();
    download_share(&client, &mut store).await.unwrap();
    let after_first = store.stats().total_puts;

    // Nothing new to fetch: every chunk is already content-addressed locally.
    download_share(&client, &mut store).await.unwrap();
    assert_eq!(store.stats().total_puts, after_first, "second pass re-fetched chunks");

    client.shutdown().await;
    server.shutdown().await;
}

#[tokio::test]
async fn unknown_content_comes_back_as_not_found() {
    let share = sample_share();
    let (server, _) = serve(&share).await;

    let client = Client::connect(server.listen_addr.clone()).await.unwrap();
    let missing = Hash::of(b"this address is not in the share");

    assert!(matches!(
        client.request(Request::GetChunk(missing)).await.unwrap(),
        Response::NotFound
    ));
    assert!(matches!(
        client.request(Request::GetChunkList(missing)).await.unwrap(),
        Response::NotFound
    ));

    client.shutdown().await;
    server.shutdown().await;
}
