//! The headless transfer manager drives a real loopback swarm
//! transfer and reports it through [`AppState`].

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use app_state::{
    AcceleratorRequest, AcceleratorRole, App, AppEvent, AppState, Scope, Settings, ShareLink,
    SubscribeRequest, TransferStatus,
};
use tempfile::TempDir;
use tokio::time::timeout;

fn sample_folder() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("cfg")).unwrap();
    fs::write(root.join("cfg/game.ini"), b"fov=110\nvsync=0\n").unwrap();
    fs::write(root.join("readme.txt"), b"a shared modpack\n").unwrap();

    // ~24 MiB so a download spans many chunks and pause lands mid-flight.
    let mut blob = Vec::with_capacity(24 * 1024 * 1024);
    let mut state = 0x1234_9abc_def0_5678u64;
    while blob.len() < 24 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(root.join("pack.bin"), &blob).unwrap();
    dir
}

/// Block until `pred` holds for a published snapshot, or fail after `secs`.
async fn wait_for(
    app: &App,
    secs: u64,
    mut pred: impl FnMut(&AppState) -> bool,
) -> AppState {
    let mut rx = app.state_watch();
    let fut = async {
        loop {
            {
                let s = rx.borrow_and_update();
                if pred(&s) {
                    return s.clone();
                }
            }
            rx.changed().await.expect("state channel closed");
        }
    };
    timeout(Duration::from_secs(secs), fut).await.expect("condition not reached in time")
}

fn dir_matches(original: &Path, produced: &Path) {
    for entry in walkdir(original) {
        let rel = entry.strip_prefix(original).unwrap();
        let got = produced.join(rel);
        assert!(got.is_file(), "missing {}", rel.display());
        assert_eq!(fs::read(&entry).unwrap(), fs::read(&got).unwrap(), "{}", rel.display());
    }
}

fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Bring up an app whose downloads land in `out_dir`.
async fn app_downloading_into(out_dir: &Path) -> App {
    let app = App::new(None).await.unwrap();
    app.update_settings(Settings {
        download_dir: out_dir.to_path_buf(),
        ..Settings::default()
    });
    wait_for(&app, 5, |s| s.settings.download_dir == out_dir).await;
    app
}

/// Like [`app_downloading_into`] but with a real config path, so its share
/// list persists across a restart.
async fn app_downloading_into_with_config(out_dir: &Path, config_path: PathBuf) -> App {
    let app = App::new(Some(config_path)).await.unwrap();
    app.update_settings(Settings {
        download_dir: out_dir.to_path_buf(),
        ..Settings::default()
    });
    wait_for(&app, 5, |s| s.settings.download_dir == out_dir).await;
    app
}

#[tokio::test]
async fn share_a_folder_then_subscribe_and_complete() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());

    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let addr = seed.share_addr.clone().unwrap();
    let manifest_id = seed.manifest_id;
    let seed_bytes = seed.total_bytes;
    let seed_files = seed.files;
    assert!(seed_bytes > 20 * 1024 * 1024 && seed_files == 3);

    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    leech.subscribe(SubscribeRequest {
        name: "modpack".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });

    let done = wait_for(&leech, 60, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    let row = done.downloads().next().unwrap();
    assert_eq!(row.done_bytes, row.total_bytes);
    assert_eq!(row.total_bytes, seed_bytes);
    assert_eq!(row.files, seed_files);
    assert!(!row.sources.is_empty(), "a source should be credited");
    assert_eq!(row.progress(), 1.0);

    let output = row.output_dir.clone().unwrap();
    dir_matches(folder.path(), &output);
}

#[tokio::test]
async fn adding_a_share_passes_through_a_scanning_phase() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    let mut events = seeder.events();

    seeder.add_local_share(folder.path());

    // The row is created with `Scanning` before the background scan/serve
    // work has even started — `TransferAdded` fires at exactly that point, so
    // checking the snapshot right after it (no other `.await` in between)
    // deterministically catches the scanning phase rather than racing it.
    let added_id = timeout(Duration::from_secs(20), async {
        loop {
            if let AppEvent::TransferAdded(id) = events.recv().await.expect("events channel closed") {
                return id;
            }
        }
    })
    .await
    .expect("TransferAdded not observed in time");

    let scanning = seeder.snapshot().get(added_id).cloned().expect("row exists");
    assert_eq!(scanning.status, TransferStatus::Scanning);

    let done = wait_for(&seeder, 20, |s| {
        s.get(added_id).is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    let seed = done.get(added_id).unwrap();
    assert!(seed.total_bytes > 20 * 1024 * 1024 && seed.files == 3);
    assert_eq!(seed.done_bytes, seed.total_bytes);
}

#[tokio::test]
async fn a_seed_streams_from_disk_under_a_small_ram_budget() {
    // A source folder several times the seed's RAM buffer: the streaming store
    // must evict and re-read from the source files and still serve every chunk.
    let folder = TempDir::new().unwrap();
    let mut blob = Vec::with_capacity(96 * 1024 * 1024);
    let mut state = 0xdead_beef_0123_4567u64;
    while blob.len() < 96 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(folder.path().join("big.bin"), &blob).unwrap();
    fs::write(folder.path().join("note.txt"), b"streamed\n").unwrap();

    let seeder = App::new(None).await.unwrap();
    // Clamped up to SourceChunkStore::MIN_BUDGET_BYTES (32 MiB) — well under the
    // 96 MiB share, so the cache is forced to evict during the download.
    seeder.update_settings(Settings { seed_cache_bytes: 1, ..Settings::default() });
    wait_for(&seeder, 5, |s| s.settings.seed_cache_bytes == 1).await;
    seeder.add_local_share(folder.path());

    let seeded = wait_for(&seeder, 30, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let (addr, manifest_id, seed_bytes) =
        (seed.share_addr.clone().unwrap(), seed.manifest_id, seed.total_bytes);
    assert!(seed_bytes > 96 * 1024 * 1024);

    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    leech.subscribe(SubscribeRequest {
        name: "streamed".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });

    let done = wait_for(&leech, 90, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    let row = done.downloads().next().unwrap();
    assert_eq!(row.done_bytes, seed_bytes);
    dir_matches(folder.path(), &row.output_dir.clone().unwrap());
}

#[tokio::test]
async fn completing_a_download_removes_its_partial_dir() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let addr = seed.share_addr.clone().unwrap();
    let manifest_id = seed.manifest_id;

    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    leech.subscribe(SubscribeRequest {
        name: "modpack".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });

    wait_for(&leech, 60, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;

    // Files landed in the output tree…
    dir_matches(folder.path(), &out.path().join("modpack"));

    // …and the scratch chunk store is cleaned up (off-thread, so poll for it).
    let partial_root = out.path().join(".gaggle-partial");
    let gone = timeout(Duration::from_secs(5), async {
        while partial_root.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "{} not removed after completion", partial_root.display());
}

#[tokio::test]
async fn progress_is_reported_before_completion() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let (addr, manifest_id) = (seed.share_addr.clone().unwrap(), seed.manifest_id);

    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    let mut events = leech.events();
    leech.subscribe(SubscribeRequest {
        name: "mp".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });

    // We must observe at least one progress tick with 0 < done < total.
    let saw_partial = timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(AppEvent::TransferProgress(id)) = events.recv().await {
                let s = leech.snapshot();
                if let Some(r) = s.get(id)
                    && r.done_bytes > 0
                    && r.done_bytes < r.total_bytes
                {
                    return true;
                }
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(saw_partial, "no mid-flight progress was reported");

    let done = wait_for(&leech, 60, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    assert_eq!(done.downloads().next().unwrap().progress(), 1.0);
}

#[tokio::test]
async fn pause_keeps_partial_progress_and_resume_completes() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let (addr, manifest_id) = (seed.share_addr.clone().unwrap(), seed.manifest_id);

    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    leech.subscribe(SubscribeRequest {
        name: "mp".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });

    // Let a little data flow, then pause.
    let id = wait_for(&leech, 30, |s| s.downloads().next().is_some())
        .await
        .downloads()
        .next()
        .unwrap()
        .id;
    tokio::time::sleep(Duration::from_millis(150)).await;
    leech.pause(id);

    let paused = wait_for(&leech, 20, |s| {
        s.get(id).is_some_and(|r| r.status == TransferStatus::Paused)
    })
    .await;
    let at_pause = paused.get(id).unwrap();
    assert!(at_pause.done_bytes < at_pause.total_bytes || at_pause.total_bytes == 0);
    assert_eq!(at_pause.speed_bps, 0);

    leech.resume(id);
    let done = wait_for(&leech, 60, |s| {
        s.get(id).is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    let row = done.get(id).unwrap();
    assert_eq!(row.done_bytes, row.total_bytes);
    dir_matches(folder.path(), row.output_dir.as_deref().unwrap());
}

#[tokio::test]
async fn settings_persist_across_a_restart() {
    let cfg = TempDir::new().unwrap();
    let path = cfg.path().join("settings.json");

    {
        let app = App::new(Some(path.clone())).await.unwrap();
        app.update_settings(Settings {
            download_cap_bps: Some(5_000_000),
            storage_cap_bytes: Some(100 << 30),
            ..Settings::default()
        });
        wait_for(&app, 5, |s| s.settings.download_cap_bps == Some(5_000_000)).await;
    }

    let reopened = App::new(Some(path)).await.unwrap();
    let s = reopened.snapshot().settings;
    assert_eq!(s.download_cap_bps, Some(5_000_000));
    assert_eq!(s.storage_cap_bytes, Some(100 << 30));
}

#[tokio::test]
async fn a_seeded_share_is_restored_after_a_restart() {
    let folder = sample_folder();
    let seed_cfg = TempDir::new().unwrap();
    let seed_path = seed_cfg.path().join("settings.json");

    let manifest_id = {
        let seeder = App::new(Some(seed_path.clone())).await.unwrap();
        seeder.add_local_share(folder.path());
        let seeded = wait_for(&seeder, 20, |s| {
            s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
        })
        .await;
        seeded.seeds().next().unwrap().manifest_id
    }; // seeder dropped without ever calling `remove` — persistence is the only reason it can come back.

    // Restarting against the same config re-seeds the folder on its own, with
    // the same identity (no `add_local_share` call this time).
    let reseeded_app = App::new(Some(seed_path)).await.unwrap();
    let reseeded = wait_for(&reseeded_app, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let reseed = reseeded.seeds().next().unwrap();
    assert_eq!(reseed.manifest_id, manifest_id, "a restored share keeps the same identity");
}

#[tokio::test]
async fn a_download_is_restored_after_a_restart() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let (addr, manifest_id) = (seed.share_addr.clone().unwrap(), seed.manifest_id);

    // A completed download restores the same way a seed does — a fresh row
    // reappears and finishes, without ever calling `subscribe` again. Two real
    // transfers happen here (the original, then the restored one), so this
    // gets a generous budget for a heavily loaded `cargo test --workspace` run.
    let out = TempDir::new().unwrap();
    let dl_cfg = TempDir::new().unwrap();
    let dl_path = dl_cfg.path().join("settings.json");
    {
        let leech = app_downloading_into_with_config(out.path(), dl_path.clone()).await;
        leech.subscribe(SubscribeRequest {
            name: "modpack".into(),
            manifest_id,
            sources: vec![addr],
            credential: None,
        });
        wait_for(&leech, 120, |s| {
            s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
        })
        .await;
    }

    let releech = App::new(Some(dl_path)).await.unwrap();
    let restored = wait_for(&releech, 120, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    assert_eq!(restored.downloads().next().unwrap().manifest_id, manifest_id);
}

#[tokio::test]
async fn persist_shares_false_skips_restoring() {
    let folder = sample_folder();
    let cfg = TempDir::new().unwrap();
    let path = cfg.path().join("settings.json");

    {
        let app = App::new(Some(path.clone())).await.unwrap();
        app.update_settings(Settings { persist_shares: false, ..Settings::default() });
        wait_for(&app, 5, |s| !s.settings.persist_shares).await;
        app.add_local_share(folder.path());
        wait_for(&app, 20, |s| {
            s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete)
        })
        .await;
    }

    let reopened = App::new(Some(path)).await.unwrap();
    // Persistence, if it were going to restore anything, kicks in on the very
    // first tick of the manager task — give it a generous window and confirm
    // nothing shows up.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reopened.snapshot().seeds().count(), 0, "persistence was off — nothing should come back");
}

#[tokio::test]
async fn removing_a_seed_makes_later_subscribers_fail() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let (addr, manifest_id, seed_id) =
        (seed.share_addr.clone().unwrap(), seed.manifest_id, seed.id);

    seeder.remove(seed_id);
    wait_for(&seeder, 10, |s| s.get(seed_id).is_none()).await;

    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    leech.subscribe(SubscribeRequest {
        name: "mp".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });

    let failed = wait_for(&leech, 30, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Failed)
    })
    .await;
    assert!(failed.downloads().next().unwrap().error.is_some());
}

// --- Delta sync ------------------------------------------------------------

#[tokio::test]
async fn rescan_then_resync_pulls_only_the_delta() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let (addr, manifest_id, seed_id) = (seed.share_addr.clone().unwrap(), seed.manifest_id, seed.id);
    assert_eq!(seed.version, 1);

    // Subscribe and complete v1.
    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    leech.subscribe(SubscribeRequest {
        name: "modpack".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });
    let done = wait_for(&leech, 60, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    let dl_id = done.downloads().next().unwrap().id;
    let output = done.downloads().next().unwrap().output_dir.clone().unwrap();
    dir_matches(folder.path(), &output);

    // Edit the shared folder: append to pack.bin, drop readme.txt, add a file.
    let mut grown = fs::read(folder.path().join("pack.bin")).unwrap();
    grown.extend_from_slice(&vec![0xABu8; 2 * 1024 * 1024]);
    fs::write(folder.path().join("pack.bin"), &grown).unwrap();
    fs::remove_file(folder.path().join("readme.txt")).unwrap();
    fs::write(folder.path().join("cfg/extra.ini"), b"added=1\n").unwrap();

    seeder.rescan_share(seed_id);
    let rescanned = wait_for(&seeder, 20, |s| {
        s.get(seed_id).is_some_and(|r| r.version == 2 && r.status == TransferStatus::Complete)
    })
    .await;
    assert_eq!(rescanned.get(seed_id).unwrap().files, 3); // game.ini, extra.ini, pack.bin

    // Subscriber notices the newer version.
    leech.check_updates(dl_id);
    wait_for(&leech, 20, |s| {
        s.get(dl_id).is_some_and(|r| r.update_available == Some(2))
    })
    .await;

    // Resync applies the delta on top of the existing tree.
    leech.resync(dl_id);
    let synced = wait_for(&leech, 60, |s| {
        s.get(dl_id)
            .is_some_and(|r| r.status == TransferStatus::Complete && r.version == 2)
    })
    .await;
    let row = synced.get(dl_id).unwrap();
    assert_eq!(row.update_available, None);
    assert_eq!(row.done_bytes, row.total_bytes);

    dir_matches(folder.path(), &output);
    assert!(!output.join("readme.txt").exists(), "removed file is gone after resync");
    assert!(output.join("cfg/extra.ini").is_file(), "added file arrived");
}

// --- Private shares, benchmark, accelerator ------------------------------

#[tokio::test]
async fn private_share_needs_a_minted_invite() {
    let folder = sample_folder();
    let origin = App::new(None).await.unwrap();
    origin.add_private_share(folder.path());
    let up = wait_for(&origin, 20, |s| {
        s.seeds().next().is_some_and(|r| {
            r.status == TransferStatus::Complete && r.private && r.share_addr.is_some()
        })
    })
    .await;
    let seed = up.seeds().next().unwrap();
    let (addr, manifest_id, seed_id) = (seed.share_addr.clone().unwrap(), seed.manifest_id, seed.id);

    // Without an invite the download is refused.
    let out_no = TempDir::new().unwrap();
    let stranger = app_downloading_into(out_no.path()).await;
    stranger.subscribe(SubscribeRequest {
        name: "mp".into(),
        manifest_id,
        sources: vec![addr.clone()],
        credential: None,
    });
    let refused = wait_for(&stranger, 30, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Failed)
    })
    .await;
    assert!(refused.downloads().next().unwrap().error.is_some());

    // Mint an invite and hand it over.
    origin.mint_invite(seed_id, Scope::All, None);
    let minted = wait_for(&origin, 10, |s| {
        s.minted_invite.as_ref().is_some_and(|m| m.transfer == seed_id)
    })
    .await;
    let token = minted.minted_invite.unwrap().token;
    let link = ShareLink::parse(&token).unwrap();
    assert!(link.invite.is_some());

    let out_ok = TempDir::new().unwrap();
    let guest = app_downloading_into(out_ok.path()).await;
    guest.subscribe(SubscribeRequest::from(link));
    let done = wait_for(&guest, 60, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    dir_matches(folder.path(), done.downloads().next().unwrap().output_dir.as_deref().unwrap());
}

#[tokio::test]
async fn remove_and_delete_wipes_the_output_folder() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let (addr, manifest_id) = (seed.share_addr.clone().unwrap(), seed.manifest_id);

    let out = TempDir::new().unwrap();
    let leech = app_downloading_into(out.path()).await;
    leech.subscribe(SubscribeRequest {
        name: "modpack".into(),
        manifest_id,
        sources: vec![addr],
        credential: None,
    });
    let done = wait_for(&leech, 60, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    let row = done.downloads().next().unwrap();
    let (dl_id, output) = (row.id, row.output_dir.clone().unwrap());
    assert!(output.is_dir());

    leech.remove_and_delete(dl_id);
    wait_for(&leech, 10, |s| s.get(dl_id).is_none()).await;

    let gone = timeout(Duration::from_secs(5), async {
        while output.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "{} still present after remove_and_delete", output.display());
}

#[tokio::test]
async fn a_scoped_invite_downloads_only_its_files() {
    let folder = sample_folder(); // cfg/game.ini, readme.txt, pack.bin
    let origin = App::new(None).await.unwrap();
    origin.add_private_share(folder.path());
    let up = wait_for(&origin, 20, |s| {
        s.seeds().next().is_some_and(|r| {
            r.status == TransferStatus::Complete && r.private && !r.file_paths.is_empty()
        })
    })
    .await;
    let seed = up.seeds().next().unwrap();
    let seed_id = seed.id;
    assert!(seed.file_paths.iter().any(|p| p == "pack.bin"));

    // Grant only the two small files — the big pack.bin is excluded.
    origin.mint_invite(seed_id, Scope::files(["cfg/game.ini", "readme.txt"]), None);
    let minted = wait_for(&origin, 10, |s| {
        s.minted_invite.as_ref().is_some_and(|m| m.transfer == seed_id)
    })
    .await;
    let link = ShareLink::parse(&minted.minted_invite.unwrap().token).unwrap();

    let out = TempDir::new().unwrap();
    let guest = app_downloading_into(out.path()).await;
    guest.subscribe(SubscribeRequest::from(link));
    let done = wait_for(&guest, 60, |s| {
        s.downloads().next().is_some_and(|r| r.status == TransferStatus::Complete)
    })
    .await;
    let row = done.downloads().next().unwrap();
    assert!(row.error.is_none(), "excluded file must not fault the download: {:?}", row.error);
    assert_eq!(row.files, 2, "only the two granted files are pulled");

    let output = row.output_dir.clone().unwrap();
    assert_eq!(
        fs::read(output.join("readme.txt")).unwrap(),
        fs::read(folder.path().join("readme.txt")).unwrap()
    );
    assert!(output.join("cfg/game.ini").is_file());
    assert!(!output.join("pack.bin").exists(), "the excluded file is not materialized");
}

#[tokio::test]
async fn benchmark_reports_disk_throughput_and_free_space() {
    let out = TempDir::new().unwrap();
    let app = app_downloading_into(out.path()).await;
    app.benchmark();
    let s = wait_for(&app, 30, |s| s.benchmark.is_some()).await;
    let b = s.benchmark.unwrap();
    assert!(b.disk_write_bps > 0, "measured a write rate");
    assert!(b.free_bytes > 0, "measured free space");
    assert!(matches!(b.suggested, AcceleratorRole::Relay | AcceleratorRole::Nas));
}

#[tokio::test]
async fn nas_accelerator_replicates_a_share() {
    let folder = sample_folder();
    let seeder = App::new(None).await.unwrap();
    seeder.add_local_share(folder.path());
    let seeded = wait_for(&seeder, 20, |s| {
        s.seeds().next().is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = seeded.seeds().next().unwrap();
    let link = ShareLink::new(seed.name.clone(), seed.manifest_id, vec![seed.share_addr.clone().unwrap()]);

    let replica_dir = TempDir::new().unwrap();
    let node = App::new(None).await.unwrap();
    node.start_accelerator(AcceleratorRequest::Nas {
        dir: replica_dir.path().to_path_buf(),
        shares: vec![link],
    });

    let up = wait_for(&node, 60, |s| {
        s.accelerator
            .as_ref()
            .is_some_and(|a| a.replica_chunks.unwrap_or(0) > 0)
    })
    .await;
    let acc = up.accelerator.unwrap();
    assert_eq!(acc.role, AcceleratorRole::Nas);
    assert!(!acc.listen_addrs.is_empty());

    node.stop_accelerator();
    wait_for(&node, 10, |s| s.accelerator.is_none()).await;
}

/// A second folder with distinct content, so two shares have distinct manifests.
fn other_folder() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("notes.txt"), b"a different share entirely\n").unwrap();
    let mut blob = Vec::with_capacity(6 * 1024 * 1024);
    let mut state = 0xfeed_face_dead_beefu64;
    while blob.len() < 6 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        blob.extend_from_slice(&state.to_le_bytes());
    }
    fs::write(dir.path().join("data.bin"), &blob).unwrap();
    dir
}

async fn seed_and_link(app: &App) -> ShareLink {
    let s = wait_for(app, 20, |s| {
        s.seeds()
            .next()
            .is_some_and(|r| r.status == TransferStatus::Complete && r.share_addr.is_some())
    })
    .await;
    let seed = s.seeds().next().unwrap();
    ShareLink::new(seed.name.clone(), seed.manifest_id, vec![seed.share_addr.clone().unwrap()])
}

#[tokio::test]
async fn relay_accelerator_carries_multiple_shares() {
    let folder_a = sample_folder();
    let folder_b = other_folder();
    let seeder_a = App::new(None).await.unwrap();
    let seeder_b = App::new(None).await.unwrap();
    seeder_a.add_local_share(folder_a.path());
    seeder_b.add_local_share(folder_b.path());
    let link_a = seed_and_link(&seeder_a).await;
    let link_b = seed_and_link(&seeder_b).await;
    assert_ne!(link_a.manifest_id, link_b.manifest_id);

    let node = App::new(None).await.unwrap();
    node.start_accelerator(AcceleratorRequest::Relay {
        cache_bytes: 64 << 20,
        shares: vec![link_a.clone(), link_b.clone()],
    });

    let up = wait_for(&node, 60, |s| {
        s.accelerator
            .as_ref()
            .is_some_and(|a| a.shares.iter().filter(|r| r.error.is_none()).count() == 2)
    })
    .await;
    let acc = up.accelerator.unwrap();
    assert_eq!(acc.role, AcceleratorRole::Relay);
    let ids: Vec<&str> = acc.shares.iter().map(|r| r.manifest_id.as_str()).collect();
    assert!(ids.contains(&link_a.manifest_id.to_hex().as_str()));
    assert!(ids.contains(&link_b.manifest_id.to_hex().as_str()));

    // Drop one share from the running accelerator.
    node.accel_remove_share(link_a.manifest_id.to_hex());
    let after = wait_for(&node, 15, |s| {
        s.accelerator.as_ref().is_some_and(|a| a.shares.len() == 1)
    })
    .await;
    assert_eq!(
        after.accelerator.unwrap().shares[0].manifest_id,
        link_b.manifest_id.to_hex()
    );

    node.stop_accelerator();
}

#[tokio::test]
async fn settings_round_trip_remote_accelerators() {
    use app_state::RemoteAccelerator;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.json");

    let mut s = Settings::default();
    s.remote_accelerators.push(RemoteAccelerator {
        label: "vps".into(),
        admin_url: "http://accel.example:8749".into(),
        daemon_key: Some("aa".repeat(32)),
    });
    s.save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap(), s);
}

#[tokio::test]
async fn a_registered_remote_accelerator_shows_up_and_reports_unreachable() {
    let dir = TempDir::new().unwrap();
    let app = App::new(Some(dir.path().join("settings.json"))).await.unwrap();
    assert!(!app.operator_public_key().is_empty());

    app.add_remote_accelerator("vps", "http://127.0.0.1:59999");
    let s = wait_for(&app, 15, |s| {
        s.remote_accelerators.iter().any(|r| r.label == "vps" && r.error.is_some())
    })
    .await;
    let r = s.remote_accelerators.iter().find(|r| r.label == "vps").unwrap();
    assert!(!r.reachable);

    // Persisted to settings.
    let reloaded = Settings::load(&dir.path().join("settings.json")).unwrap();
    assert!(reloaded.remote_accelerators.iter().any(|r| r.label == "vps"));
}
