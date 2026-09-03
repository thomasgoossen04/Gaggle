//! Milestone 8: the headless transfer manager drives a real loopback swarm
//! transfer and reports it through [`AppState`].

use std::fs;
use std::path::Path;
use std::time::Duration;

use app_state::{App, AppEvent, AppState, Settings, SubscribeRequest, TransferStatus};
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
