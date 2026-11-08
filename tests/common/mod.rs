//! Shared helpers for integration tests. Everything runs against TEMP
//! directories built from committed fixtures — never against real Stores.

#![allow(dead_code)] // each integration test crate uses a subset

pub mod fake_remote;

use std::fs;
use std::path::{Path, PathBuf};

use agent_sessions::archive::{Archive, rebuild};
use agent_sessions::pull::{PullStats, StoreRoots, pull_localhost};

pub const HOST: &str = "testhost";

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude")
}

pub fn source_fixtures_dir(source: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{source}"))
}

/// Store roots that resolve nothing anywhere — every Source enumerates
/// empty. Tests override exactly the root under test, so a test run can
/// never touch a real Store.
pub fn empty_roots(base: &Path) -> StoreRoots {
    StoreRoots {
        claude: base.join("no-claude-store"),
        codex: base.join("no-codex-store"),
        pi: base.join("no-pi-store"),
        cursor: base.join("no-cursor-store/state.vscdb"),
        t3: base.join("no-t3-store"),
        chatgpt: base.join("no-chatgpt-store"),
        t3_explicit: false,
    }
}

pub fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Pin the Workday timezone to UTC so golden Views are byte-identical on
/// every machine (the default is the host's local zone).
pub fn pin_utc_workday(archive_dir: &Path) {
    fs::write(
        archive_dir.join("config.toml"),
        "[views]\ntimezone = \"UTC\"\n",
    )
    .unwrap();
}

/// Build a temp Store from the claude fixtures.
pub fn temp_store() -> tempfile::TempDir {
    let store = tempfile::tempdir().unwrap();
    copy_tree(&fixtures_dir().join("projects"), store.path());
    store
}

/// Pull a Claude `store` into the Archive at `archive_dir`.
pub fn pull(store: &Path, archive_dir: &Path) -> PullStats {
    let archive = Archive::open(archive_dir).unwrap();
    let mut roots = empty_roots(store);
    roots.claude = store.to_path_buf();
    pull_localhost(&archive, &roots, HOST).unwrap()
}

/// Pull a Codex `store` (root containing sessions/ + archived_sessions/)
/// into the Archive at `archive_dir`.
pub fn pull_codex(store: &Path, archive_dir: &Path) -> PullStats {
    let archive = Archive::open(archive_dir).unwrap();
    let mut roots = empty_roots(store);
    roots.codex = store.to_path_buf();
    pull_localhost(&archive, &roots, HOST).unwrap()
}

/// Pull a Pi `store` (the sessions root) into the Archive at `archive_dir`.
pub fn pull_pi(store: &Path, archive_dir: &Path) -> PullStats {
    let archive = Archive::open(archive_dir).unwrap();
    let mut roots = empty_roots(store);
    roots.pi = store.to_path_buf();
    pull_localhost(&archive, &roots, HOST).unwrap()
}

/// All files under `dir` as (relative path, contents) — for byte-identical
/// View comparisons and capture-directory snapshots.
pub fn tree_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    collect_bytes(dir, dir, &mut out);
    out.sort();
    out
}

/// Independent captures of the same Store legitimately have different
/// Capture times. Align that IO fact before comparing derived archive state.
pub fn align_capture_times(reference: &Archive, target: &Archive) {
    for (relative, bytes) in tree_bytes(&reference.captures_dir()) {
        if !relative.ends_with(".meta.json") {
            continue;
        }
        let reference_meta: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let target_path = target.captures_dir().join(&relative);
        let mut target_meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&target_path).unwrap()).unwrap();
        target_meta["capturedAtMs"] = reference_meta["capturedAtMs"].clone();
        fs::write(target_path, serde_json::to_vec(&target_meta).unwrap()).unwrap();
    }
    rebuild(target).unwrap();
}

fn collect_bytes(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect_bytes(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().into();
            out.push((rel, fs::read(&path).unwrap()));
        }
    }
}
