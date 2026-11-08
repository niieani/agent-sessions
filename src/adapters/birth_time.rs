//! Native birth-time IO: Linux statx works independently of the libc target;
//! other platforms use std. Missing evidence is never replaced with mtime/ctime.

use std::fs::{File, Metadata};
use std::path::Path;

use anyhow::{Context, Result};

pub(crate) fn native_birth_time_ms(path: &Path, metadata: &Metadata) -> Result<Option<i64>> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{AtFlags, CWD, StatxFlags, statx};
        let _ = metadata;
        statx_birth_time_ms(statx(CWD, path, AtFlags::empty(), StatxFlags::BTIME))
            .with_context(|| format!("reading native birth time for {}", path.display()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        created_time_ms(metadata.created())
    }
}

/// Query the opened inode, even if its native path was replaced after open.
pub(crate) fn native_file_birth_time_ms(file: &File, metadata: &Metadata) -> Result<Option<i64>> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{AtFlags, StatxFlags, statx};
        let _ = metadata;
        statx_birth_time_ms(statx(file, "", AtFlags::EMPTY_PATH, StatxFlags::BTIME))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        created_time_ms(metadata.created())
    }
}

#[cfg(target_os = "linux")]
fn statx_birth_time_ms(stat: rustix::io::Result<rustix::fs::Statx>) -> Result<Option<i64>> {
    use rustix::{fs::StatxFlags, io::Errno};
    let stat = match stat {
        Ok(stat) => stat,
        Err(Errno::NOSYS) => return Ok(None),
        Err(error) => return Err(error).context("reading native file birth time via statx"),
    };
    if stat.stx_mask & StatxFlags::BTIME.bits() == 0 {
        return Ok(None);
    }
    let birth = stat.stx_btime;
    anyhow::ensure!(birth.tv_sec >= 0, "native birth time predates Unix epoch");
    anyhow::ensure!(
        birth.tv_nsec < 1_000_000_000,
        "invalid native birth nanoseconds"
    );
    Ok(Some(
        birth
            .tv_sec
            .checked_mul(1_000)
            .and_then(|ms| ms.checked_add(i64::from(birth.tv_nsec / 1_000_000)))
            .context("native birth time overflows epoch milliseconds")?,
    ))
}

#[cfg(not(target_os = "linux"))]
fn created_time_ms(created: std::io::Result<std::time::SystemTime>) -> Result<Option<i64>> {
    match created {
        Ok(created) => Ok(Some(i64::try_from(
            created
                .duration_since(std::time::UNIX_EPOCH)
                .context("native birth time predates Unix epoch")?
                .as_millis(),
        )?)),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Ok(None),
        Err(error) => Err(error).context("reading native file birth time"),
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::created_time_ms;

    #[test]
    fn unsupported_creation_time_is_unknown_but_other_io_errors_fail() {
        assert_eq!(
            created_time_ms(Err(std::io::ErrorKind::Unsupported.into())).unwrap(),
            None
        );
        assert!(created_time_ms(Err(std::io::ErrorKind::PermissionDenied.into())).is_err());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use rustix::fs::{AtFlags, StatxFlags, statx};

    #[test]
    fn statx_requires_birth_mask_and_preserves_epoch_and_subsecond_precision() {
        let file = tempfile::tempfile().unwrap();
        let mut stat = statx(&file, "", AtFlags::EMPTY_PATH, StatxFlags::BTIME).unwrap();
        assert_ne!(stat.stx_mask & StatxFlags::BTIME.bits(), 0);
        stat.stx_btime.tv_sec = 1_234;
        stat.stx_btime.tv_nsec = 567_890_123;
        assert_eq!(statx_birth_time_ms(Ok(stat)).unwrap(), Some(1_234_567));
        stat.stx_mask &= !StatxFlags::BTIME.bits();
        assert_eq!(statx_birth_time_ms(Ok(stat)).unwrap(), None);
        stat.stx_mask |= StatxFlags::BTIME.bits();
        stat.stx_btime.tv_sec = 0;
        stat.stx_btime.tv_nsec = 0;
        assert_eq!(statx_birth_time_ms(Ok(stat)).unwrap(), Some(0));
        stat.stx_btime.tv_sec = -1;
        assert!(statx_birth_time_ms(Ok(stat)).is_err());
        stat.stx_btime.tv_sec = i64::MAX;
        assert!(statx_birth_time_ms(Ok(stat)).is_err());
        stat.stx_btime.tv_sec = 0;
        stat.stx_btime.tv_nsec = 1_000_000_000;
        assert!(statx_birth_time_ms(Ok(stat)).is_err());
    }

    #[test]
    fn unavailable_statx_is_unknown_but_other_errors_fail() {
        use rustix::io::Errno;
        assert_eq!(statx_birth_time_ms(Err(Errno::NOSYS)).unwrap(), None);
        for error in [Errno::PERM, Errno::ACCESS, Errno::NOENT, Errno::IO] {
            assert!(statx_birth_time_ms(Err(error)).is_err());
        }
    }

    #[test]
    fn opened_inode_keeps_birth_evidence_when_path_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.jsonl");
        let file = File::create(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let birth = native_file_birth_time_ms(&file, &metadata).unwrap();
        assert!(birth.is_some());
        std::fs::rename(&path, dir.path().join("previous.jsonl")).unwrap();
        std::os::unix::fs::symlink("/proc/version", &path).unwrap();
        assert_eq!(
            native_birth_time_ms(&path, &std::fs::metadata(&path).unwrap()).unwrap(),
            None
        );
        assert_eq!(native_file_birth_time_ms(&file, &metadata).unwrap(), birth);
    }
}
