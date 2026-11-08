mod common;
use agent_sessions::{
    adapters::{Adapter, EnumerateGate, claude::ClaudeAdapter},
    archive::Archive,
    bundle::{export, ingest_bundle},
};
use std::{fs, path::Path};

fn sidecar(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
#[test]
fn full_capture_backfills_birth_time_without_replacing_unchanged_body_and_bundles_preserve_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    common::copy_tree(&common::fixtures_dir().join("projects"), &store);
    let refs = ClaudeAdapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    let session = refs.iter().find(|s| !s.native_id.contains('/')).unwrap();
    let captures = dir.path().join("captures");
    let capture = ClaudeAdapter.capture(session, &captures, "host").unwrap();
    let birth = expected_birth_time_ms(&session.path);
    assert_eq!(sidecar(&capture.meta_path)["nativeBirthTimeMs"], birth);
    let body = fs::read(&capture.body_path).unwrap();
    let mut old = sidecar(&capture.meta_path);
    old.as_object_mut().unwrap().remove("nativeBirthTimeMs");
    fs::write(&capture.meta_path, old.to_string()).unwrap();
    let backfilled = ClaudeAdapter.capture(session, &captures, "host").unwrap();
    assert_eq!(sidecar(&backfilled.meta_path)["nativeBirthTimeMs"], birth);
    assert_eq!(body, fs::read(&backfilled.body_path).unwrap());
    let archive = Archive::open(&dir.path().join("archive")).unwrap();
    let mut roots = common::empty_roots(&store);
    roots.claude = store;
    let mut bytes = Vec::new();
    export(&mut bytes, &roots, "host", Some("claude"), None).unwrap();
    ingest_bundle(&bytes[..], &archive, &mut archive.db().unwrap()).unwrap();
    let meta = archive
        .captures_dir()
        .join("host/claude")
        .join(format!("{}.meta.json", session.native_id));
    assert_eq!(sidecar(&meta)["nativeBirthTimeMs"], birth);
    let mut old = sidecar(&meta);
    old.as_object_mut().unwrap().remove("nativeBirthTimeMs");
    fs::write(&meta, old.to_string()).unwrap();
    ingest_bundle(&bytes[..], &archive, &mut archive.db().unwrap()).unwrap();
    assert_eq!(sidecar(&meta)["nativeBirthTimeMs"], birth);
}

#[test]
fn incremental_enumeration_backfills_missing_birth_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    common::copy_tree(&common::fixtures_dir().join("projects"), &store);
    let refs = ClaudeAdapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    let session = &refs[0];
    let captures = dir.path().join("captures");
    let mut cap = ClaudeAdapter.capture(session, &captures, "host").unwrap();
    cap.set_needs_retry(false).unwrap();
    let mut meta = sidecar(&cap.meta_path);
    meta.as_object_mut().unwrap().remove("nativeBirthTimeMs");
    fs::write(&cap.meta_path, meta.to_string()).unwrap();
    let gate = EnumerateGate::load(&captures, "host", "claude", Some(i64::MAX)).unwrap();
    let candidates = ClaudeAdapter.enumerate(&store, &gate).unwrap();
    assert!(candidates.iter().any(|s| s.native_id == session.native_id));
}

#[cfg(target_os = "linux")]
#[test]
fn filesystem_without_birth_time_records_explicit_unknown() {
    use rustix::fs::{AtFlags, CWD, StatxFlags, statx};
    let path = Path::new("/proc/version");
    let stat = statx(CWD, path, AtFlags::empty(), StatxFlags::BTIME).unwrap();
    assert_eq!(stat.stx_mask & StatxFlags::BTIME.bits(), 0);
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    common::copy_tree(&common::fixtures_dir().join("projects"), &store);
    let mut refs = ClaudeAdapter
        .enumerate(&store, &EnumerateGate::default())
        .unwrap();
    let session = &mut refs[0];
    session.path = path.to_path_buf();
    session.native_meta_path = None;
    let capture = ClaudeAdapter
        .capture(session, &dir.path().join("captures"), "host")
        .unwrap();
    let meta = sidecar(&capture.meta_path);
    assert!(meta.as_object().unwrap().contains_key("nativeBirthTimeMs"));
    assert!(meta["nativeBirthTimeMs"].is_null());
}

/// The test filesystem must expose birth time: APFS on macOS; ext4, btrfs,
/// xfs or overlayfs on modern Linux. Never skip the Linux regression.
fn expected_birth_time_ms(path: &Path) -> i64 {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{AtFlags, CWD, StatxFlags, statx};
        let stat = statx(CWD, path, AtFlags::empty(), StatxFlags::BTIME).unwrap();
        assert_ne!(
            stat.stx_mask & StatxFlags::BTIME.bits(),
            0,
            "test filesystem must return STATX_BTIME"
        );
        stat.stx_btime.tv_sec * 1_000 + i64::from(stat.stx_btime.tv_nsec / 1_000_000)
    }
    #[cfg(not(target_os = "linux"))]
    {
        fs::metadata(path)
            .unwrap()
            .created()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }
}
