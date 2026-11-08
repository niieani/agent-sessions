//! `<archive>/digest-status.json`: the machine-readable state of the latest
//! digest or repair-digests run, and the lock that proves its writer lives.
//!
//! The running writer holds an exclusive lock on `.digest-status.lock` for
//! its whole run. Readers probe that lock without blocking and never touch
//! the Archive writer lock, so a file that still says `running` while nobody
//! holds the status lock was left by a process that died.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STATUS_FILE: &str = "digest-status.json";
const LOCK_FILE: &str = ".digest-status.lock";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    /// Selecting work; totals are not known yet.
    Planning,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Day,
    Title,
}

/// One unit of planned work currently being generated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentItem {
    pub source: String,
    pub session: String,
    pub workday: String,
    pub kind: ItemKind,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProgress {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestStatus {
    pub schema_version: u32,
    /// `digest` or `repair-digests`.
    pub command: String,
    pub pid: u32,
    pub host: String,
    pub started_at: String,
    pub updated_at: String,
    pub state: RunState,
    /// Planned items: Session Days plus Session titles.
    pub total: usize,
    /// Settled items, successful or failed.
    pub done: usize,
    pub failed: usize,
    /// Rejected Digests (model outputs refused and retried or failed).
    pub rejected: usize,
    pub sources: BTreeMap<String, SourceProgress>,
    /// The most recently started item still in flight.
    pub current: Option<CurrentItem>,
    pub in_flight: usize,
    /// Settled items per hour over the recent window.
    pub rate_per_hour: Option<f64>,
    pub eta_seconds: Option<u64>,
    pub eta_at: Option<String>,
    pub progress_interval_ms: u64,
    pub error: Option<String>,
}

/// Exclusive ownership of the status file for one run.
pub(super) struct StatusWriter {
    root: PathBuf,
    _lock: File,
}

impl StatusWriter {
    /// Callers hold the Archive writer lock, so only momentary reader probes
    /// can contend for the status lock.
    pub(super) fn acquire(archive_root: &Path) -> Result<Self> {
        let path = archive_root.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening Digest status lock {}", path.display()))?;
        lock.lock()
            .with_context(|| format!("locking Digest status lock {}", path.display()))?;
        Ok(Self {
            root: archive_root.to_path_buf(),
            _lock: lock,
        })
    }

    pub(super) fn write(&self, status: &DigestStatus) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(status)?;
        bytes.push(b'\n');
        let mut temp = tempfile::NamedTempFile::new_in(&self.root).with_context(|| {
            format!(
                "creating Digest status temporary file in {}",
                self.root.display()
            )
        })?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        let path = self.root.join(STATUS_FILE);
        temp.persist(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("committing {}", path.display()))?;
        Ok(())
    }
}

impl Drop for StatusWriter {
    fn drop(&mut self) {
        // Closing only our descriptor leaves flock held by a concurrent fork
        // until its exec/exit. Explicit unlock releases the shared description.
        if let Err(error) = self._lock.unlock() {
            tracing::error!(%error, "unlocking Digest status failed");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The writer holds the status lock and the file describes its run.
    Live,
    /// A writer holds the lock but has not written its first status yet;
    /// the file still describes the previous run.
    Starting,
    /// The file records a run that ended.
    Finished,
    /// The file says planning or running, but no process holds the status
    /// lock: its writer died without recording an outcome.
    Dead,
}

#[derive(Debug, Clone)]
pub struct StatusReport {
    pub status: DigestStatus,
    pub liveness: Liveness,
}

/// Read the status file and judge its writer's liveness, without writing
/// anything or touching the Archive writer lock.
pub fn inspect(archive_root: &Path) -> Result<StatusReport> {
    let path = archive_root.join(STATUS_FILE);
    // Re-read until the file is unchanged around the lock probe, so a run
    // starting or finishing mid-inspection is not misjudged.
    for _ in 0..5 {
        let before = read(&path)?;
        let active = writer_active(archive_root)?;
        if read(&path)? != before {
            continue;
        }
        let status: DigestStatus = serde_json::from_slice(&before)
            .with_context(|| format!("parsing {}", path.display()))?;
        anyhow::ensure!(
            status.schema_version == SCHEMA_VERSION,
            "{} has unsupported schema version {}",
            path.display(),
            status.schema_version
        );
        let unfinished = matches!(status.state, RunState::Planning | RunState::Running);
        let liveness = match (active, unfinished) {
            (true, true) => Liveness::Live,
            (true, false) => Liveness::Starting,
            (false, true) => Liveness::Dead,
            (false, false) => Liveness::Finished,
        };
        return Ok(StatusReport { status, liveness });
    }
    anyhow::bail!("{} kept changing while being read", path.display())
}

fn read(path: &Path) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == ErrorKind::NotFound => anyhow::bail!(
            "no Digest status at {}; no digest or repair-digests run has started in this Archive",
            path.display()
        ),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn writer_active(archive_root: &Path) -> Result<bool> {
    let path = archive_root.join(LOCK_FILE);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("opening {}", path.display()));
        }
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("probing {}", path.display()))
        }
    }
}

pub(super) fn new_status(command: &str, started_at: String, interval_ms: u64) -> DigestStatus {
    DigestStatus {
        schema_version: SCHEMA_VERSION,
        command: command.into(),
        pid: std::process::id(),
        host: crate::pull::local_hostname(),
        updated_at: started_at.clone(),
        started_at,
        state: RunState::Planning,
        total: 0,
        done: 0,
        failed: 0,
        rejected: 0,
        sources: BTreeMap::new(),
        current: None,
        in_flight: 0,
        rate_per_hour: None,
        eta_seconds: None,
        eta_at: None,
        progress_interval_ms: interval_ms,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_writer_releases_lock_even_with_inherited_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let writer = StatusWriter::acquire(root.path()).unwrap();
        // A fork inherits the same open file description until exec/exit.
        // A duplicate reproduces that lifetime deterministically.
        let inherited = writer._lock.try_clone().unwrap();
        let mut status = new_status("digest", "2026-11-07T00:00:00Z".into(), 1000);
        status.state = RunState::Completed;
        writer.write(&status).unwrap();
        drop(writer);
        assert_eq!(inspect(root.path()).unwrap().liveness, Liveness::Finished);
        drop(inherited);
    }
}
