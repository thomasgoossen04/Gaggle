//! End-to-end: folder on disk -> chunks -> Merkle roots -> manifest,
//! with a deduping store underneath.

use std::fs;
use std::path::Path;

use gaggle_core::{
    DiskChunkStore, Manifest, MemoryChunkStore, snapshot_dir, sync_share, write_share,
};

/// Deterministic pseudo-random bytes (splitmix64).
fn pattern(len: usize, mut seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn write(root: &Path, rel: &str, data: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

#[test]
fn snapshot_builds_a_verifiable_manifest_and_dedups() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let a = pattern(3 * 1024 * 1024, 1);
    let mut a_plus = a.clone();
    a_plus.extend_from_slice(&pattern(1024 * 1024, 99));

    write(root, "a/x.bin", &a);
    write(root, "a/y.bin", &a); // exact-duplicate file
    write(root, "b/z.bin", &a_plus); // shares its leading region with x.bin
    write(root, "b/empty.bin", b"");
    fs::create_dir_all(root.join("c/empty-dir")).unwrap();

    let mut store = MemoryChunkStore::new();
    let snap = snapshot_dir(root, "share", 1, &mut store).unwrap();

    assert_eq!(
        snap.manifest.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["a/x.bin", "a/y.bin", "b/empty.bin", "b/z.bin"]
    );
    assert!(snap.manifest.dirs.iter().any(|d| d == "c/empty-dir"));
    assert!(snap.skipped.is_empty());
    snap.manifest.validate().unwrap();

    // Each chunk list reconstructs the exact root the manifest committed to.
    for f in &snap.manifest.files {
        snap.chunk_lists[&f.path]
            .verify(&f.root, f.size)
            .unwrap_or_else(|e| panic!("{}: {e}", f.path));
    }

    // Identical bytes -> identical root; empty file -> empty chunk list.
    assert_eq!(
        snap.manifest.file("a/x.bin").unwrap().root,
        snap.manifest.file("a/y.bin").unwrap().root
    );
    assert_eq!(snap.manifest.file("b/empty.bin").unwrap().size, 0);
    assert!(snap.chunk_lists["b/empty.bin"].is_empty());

    let stats = store.stats();
    assert!(
        stats.duplicate_bytes >= a.len() as u64,
        "y.bin should dedup entirely against x.bin: {stats:?}"
    );
    assert!(stats.unique_bytes < stats.logical_bytes);
    assert!(stats.dedup_ratio() > 0.0);

    let json = snap.manifest.to_json().unwrap();
    assert_eq!(Manifest::from_json(&json).unwrap(), snap.manifest);
}

#[test]
fn re_snapshotting_an_edited_folder_only_stores_the_delta() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let base = pattern(16 * 1024 * 1024, 7);
    write(root, "data/archive.bin", &base);
    write(root, "data/notes.txt", b"unchanged");

    let mut store = MemoryChunkStore::new();
    let first = snapshot_dir(root, "share", 1, &mut store).unwrap();
    let before = store.stats();

    // Append to the archive; leave notes.txt alone.
    let mut grown = base.clone();
    grown.extend_from_slice(&pattern(512 * 1024, 8));
    write(root, "data/archive.bin", &grown);

    let second = snapshot_dir(root, "share", 2, &mut store).unwrap();
    let after = store.stats();

    let diff = Manifest::diff(&first.manifest, &second.manifest);
    assert!(diff.added.is_empty() && diff.removed.is_empty());
    assert_eq!(diff.unchanged, 1, "notes.txt is untouched");
    assert_eq!(diff.changed.len(), 1);
    assert_eq!(diff.changed[0].0.path, "data/archive.bin");
    assert_ne!(diff.changed[0].0.root, diff.changed[0].1.root);

    let newly_stored = after.unique_bytes - before.unique_bytes;
    assert!(
        newly_stored > 0 && newly_stored < base.len() as u64 / 2,
        "content-defined chunking should have re-used most of the archive; stored {newly_stored} new bytes"
    );
    assert!(after.duplicate_bytes > before.duplicate_bytes);
}

#[test]
fn write_share_reconstructs_a_folder_from_a_disk_store() {
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    write(root, "mods/a.bin", &pattern(5 * 1024 * 1024, 3));
    write(root, "mods/loose/readme.txt", b"hello replica\n");
    write(root, "top.txt", b"root file");
    fs::create_dir_all(root.join("empty-dir")).unwrap();

    // Snapshot into a durable on-disk store, as the NAS accelerator would.
    let chunk_dir = tempfile::tempdir().unwrap();
    let mut store = DiskChunkStore::open(chunk_dir.path()).unwrap();
    let snap = snapshot_dir(root, "replica", 1, &mut store).unwrap();

    // A fresh process re-opens the same chunk directory and materializes the tree.
    let reopened = DiskChunkStore::open(chunk_dir.path()).unwrap();
    let out = tempfile::tempdir().unwrap();
    write_share(out.path(), &snap.manifest, &snap.chunk_lists, &reopened).unwrap();

    for f in &snap.manifest.files {
        let original = fs::read(root.join(&f.path)).unwrap();
        let rebuilt = fs::read(out.path().join(&f.path)).unwrap();
        assert_eq!(original, rebuilt, "{} did not round-trip through the disk store", f.path);
    }
    assert!(out.path().join("empty-dir").is_dir(), "empty dirs are recreated");

    // Re-snapshotting the materialized copy yields the same manifest identity.
    let mut check = MemoryChunkStore::new();
    let again = snapshot_dir(out.path(), "replica", 1, &mut check).unwrap();
    assert_eq!(again.manifest.id(), snap.manifest.id());
}

#[test]
fn sync_share_applies_only_the_delta_to_an_existing_tree() {
    // v1 of the share: three files + an empty dir, materialized to `out`.
    let src = tempfile::tempdir().unwrap();
    let sroot = src.path();
    write(sroot, "data/archive.bin", &pattern(12 * 1024 * 1024, 11));
    write(sroot, "data/keep.txt", b"unchanged across versions");
    write(sroot, "data/old-only.txt", b"present in v1, gone in v2");
    fs::create_dir_all(sroot.join("data/dropme")).unwrap();

    let mut store = MemoryChunkStore::new();
    let v1 = snapshot_dir(sroot, "share", 1, &mut store).unwrap();

    let out = tempfile::tempdir().unwrap();
    write_share(out.path(), &v1.manifest, &v1.chunk_lists, &store).unwrap();

    // v2: grow the archive, delete a file and its dir, add a new file.
    let mut grown = pattern(12 * 1024 * 1024, 11);
    grown.extend_from_slice(&pattern(768 * 1024, 22));
    write(sroot, "data/archive.bin", &grown);
    fs::remove_file(sroot.join("data/old-only.txt")).unwrap();
    fs::remove_dir(sroot.join("data/dropme")).unwrap();
    write(sroot, "data/new.txt", b"fresh in v2");

    let v2 = snapshot_dir(sroot, "share", 2, &mut store).unwrap();

    // Freeze the untouched file's mtime so we can prove it was never rewritten.
    let keep_before = fs::metadata(out.path().join("data/keep.txt")).unwrap().modified().unwrap();

    let outcome =
        sync_share(out.path(), &v1.manifest, &v2.manifest, &v2.chunk_lists, &store).unwrap();

    assert_eq!(outcome.written, ["data/archive.bin", "data/new.txt"]);
    assert_eq!(outcome.removed, ["data/old-only.txt"]);

    // The output tree now matches a fresh v2 materialization exactly.
    for f in &v2.manifest.files {
        assert_eq!(
            fs::read(sroot.join(&f.path)).unwrap(),
            fs::read(out.path().join(&f.path)).unwrap(),
            "{}",
            f.path
        );
    }
    assert!(!out.path().join("data/old-only.txt").exists(), "removed file is gone");
    assert!(!out.path().join("data/dropme").exists(), "emptied dir is pruned");
    assert_eq!(
        fs::metadata(out.path().join("data/keep.txt")).unwrap().modified().unwrap(),
        keep_before,
        "the unchanged file was not rewritten"
    );

    // Re-snapshotting the synced tree reproduces v2's identity.
    let mut check = MemoryChunkStore::new();
    let again = snapshot_dir(out.path(), "share", 2, &mut check).unwrap();
    assert_eq!(again.manifest.id(), v2.manifest.id());
}
