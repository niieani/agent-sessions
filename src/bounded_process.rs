//! Deadline-controlled process groups. File-backed output avoids pipe drains
//! waiting forever for a helper that inherited stdout/stderr.
use anyhow::{Context, Result, bail};
use process_wrap::std::{ChildWrapper, CommandWrap, ProcessGroup};
use std::{
    io::{Read, Seek, SeekFrom},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct ChildGuard {
    child: Box<dyn ChildWrapper>,
    reaped: bool,
}
impl ChildGuard {
    fn stop(&mut self) -> Result<()> {
        let killed = self.child.start_kill();
        // An already exited group has nothing left to signal. Always reap the
        // direct child; process-wrap also reaps group children owned by us.
        if let Err(error) = killed
            && error.raw_os_error() != Some(3)
        {
            return Err(error.into());
        }
        self.child.wait()?;
        self.reaped = true;
        Ok(())
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.start_kill();
            let _ = self.child.wait();
        }
    }
}

pub fn output(mut command: Command, limit: Duration) -> Result<Output> {
    if limit.is_zero() {
        bail!("repository lookup budget exhausted; retry later");
    }
    let deadline = Instant::now() + limit;
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut command = CommandWrap::from(command);
    command.wrap(ProcessGroup::leader());
    let mut child = ChildGuard {
        child: command
            .spawn()
            .context("spawning bounded repository process")?,
        reaped: false,
    };
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            break status;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            child.stop()?;
            bail!(
                "repository process timed out after {limit:?}; killed and reaped its process group; retry later"
            );
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    };
    child.stop()?;
    const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;
    let read = |mut file: std::fs::File| -> Result<Vec<u8>> {
        if file.metadata()?.len() > OUTPUT_LIMIT {
            bail!("repository process output exceeds 8 MiB; retry later");
        }
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(OUTPUT_LIMIT).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    // No filesystem operation here dereferences the recorded working path.
    Ok(Output {
        status,
        stdout: read(stdout)?,
        stderr: read(stderr)?,
    })
}
