//! Deterministically hold the fork-to-exec window open. CLOEXEC only closes
//! inherited descriptors at exec, not at fork (or posix_spawn's internal clone).
use agent_sessions::archive::Archive;
use std::{
    io::{Read, Write},
    os::unix::{io::AsRawFd, net::UnixStream, process::CommandExt},
    process::Command,
};

#[test]
fn dropping_guard_releases_lock_even_during_another_threads_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    let guard = archive.acquire_writer_lock().unwrap();
    assert!(
        archive.acquire_writer_lock().is_err(),
        "live guard excludes writers"
    );
    let (mut started, start_child) = UnixStream::pair().unwrap();
    let (mut release, release_child) = UnixStream::pair().unwrap();
    let thread = std::thread::spawn(move || {
        let mut command = Command::new("/usr/bin/true");
        // SAFETY: the child only performs async-signal-safe read/write on
        // pre-created descriptors; no locks, allocation, or Rust formatting.
        unsafe {
            command.pre_exec(move || {
                unsafe extern "C" {
                    fn write(fd: std::ffi::c_int, buf: *const u8, len: usize) -> isize;
                    fn read(fd: std::ffi::c_int, buf: *mut u8, len: usize) -> isize;
                }
                let mut byte = 1u8;
                if write(start_child.as_raw_fd(), &byte, 1) != 1
                    || read(release_child.as_raw_fd(), &mut byte, 1) != 1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().unwrap().wait().unwrap()
    });
    started.read_exact(&mut [0]).unwrap();
    drop(guard);
    let next = archive.acquire_writer_lock();
    // Always let the child exec before asserting, including on the base.
    release.write_all(&[1]).unwrap();
    assert!(thread.join().unwrap().success());
    assert!(
        next.is_ok(),
        "CLOEXEC child retained the dropped guard's lock: {:#}",
        next.err().unwrap()
    );
}
