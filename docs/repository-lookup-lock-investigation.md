# Writer-lock regression evidence

Base: 35302301c96e0d63db757377829c479a3b9ea3fe. Version remains 0.4.12.
Host: Linux x86_64, pinned Rust 1.98.1. No macOS access used.

## Measured base reproduction

From a clean checkout of the base, run 60 times (normal parallel Rust test harness):

```sh
for i in $(seq 1 60); do cargo test --locked --test codex_integration > /tmp/codex-$i.log 2>&1 || printf 'failed %s\n' "$i"; done
```

This machine: 6/60 failures. Logs: /tmp/asessions-lock-base/failure-*.log.
The test name varies (e.g. action_counts_are_an_explicit_render_option).
The base loop output is /tmp/asessions-base-loop.log.

## Deterministic ownership reproduction

Copy tests/writer_lock_inheritance.rs onto the base and run:
`cargo test --locked --test writer_lock_inheritance -- --nocapture`.
A second thread starts a child, explicitly pausing before exec via socket
handshakes using only async-signal-safe syscalls. The child inherited the
archive's close-on-exec lock descriptor. Drop the sole writer guard while the
child is still paused, then attempt another writer. Base: fails every time;
explicit unlock on Drop: passes. The child is released and reaped before the
assertion, including on failure. Logs: /tmp/lock-inheritance-{red,green}.log.
The pre_exec hook deliberately widens the inheritance window for diagnosis;
production Git/process-wrap does NOT install that hook.

## Source audit

process-wrap 10.0.1 std/process_group.rs:84-87 calls process_group(0), not
pre_exec. Pinned Rust std sys/process/unix/unix.rs admits Linux GNU/musl and
Apple to posix_spawn, with POSIX_SPAWN_SETPGROUP. UID/GID/custom pre_exec/PATH
constraints can instead select fork/exec. Process groups themselves do not
force fork. The new Git command is eligible for posix_spawn on both platforms.
Linux libc's spawn internally clones before exec; unrelated concurrent threads
can drop guards during that window. CLOEXEC closes descriptors at exec, not
at creation of the child. std fs uses O_CLOEXEC; std Unix File locks use flock,
whose ownership belongs to the shared open file description.

Temp output files and their clones also use std CLOEXEC files; child stdin is
null, and stdout/stderr are the intended dup2 targets. They do not themselves
own archive locks. The polling controller reaps the leader on success/timeout
and kills remaining group members. The 10 ms polling sleep prolongs observation
of an exit, but does not keep a CLOEXEC lock descriptor open after exec. No
lookup child was found surviving its controller return. A child on ANOTHER test
thread can nevertheless still be in spawn when the owner drops its guard.

The material exposure increase from 2263f8f is that previously metadata returned
Missing without spawning for absent fixture cwd paths. 3530230 removed that
unbounded parent metadata access, correctly, and now spawns Git for those paths.
This raised the rate of an inherited-description ownership bug. Explicit
LOCK_UN ends the sole guard's ownership immediately, even with transient child
copies. This is a production fix, not serialization or retry logic. Real
concurrent library calls in one process can hit it. Current CLI Pull performs
lookups synchronously and joins/reaps them before dropping its guard: that path
alone does not leave a Git child holding its lock after return. Separate CLI
processes cannot inherit each other's descriptors. A following command could
only see this stale lock if the owning process had another concurrent spawn
still before exec. The parallel harness provides exactly that overlap; it is
not evidence of a normal single-threaded Pull leaking a running Git child.
Normally exec closes inherited copies quickly; blocked child setup can extend
that interval. The explicit unlock protects the general guard contract without
retries or test-only serialization.

process-wrap retained: its process-group configuration is not the forced-fork
cause, and its cleanup remains covered by real hanging-child tests.

## Permission evidence

On this unprivileged host, git init then chmod 000 .git followed by
`git -C DIR rev-parse --git-dir` exits 128 with generic "not a git repository";
chmod 700 restores successful discovery. Git setup.c validate_headref returns
-1 for failed lstat/open and is_git_directory turns that into false, including
EACCES/EPERM and ENOENT. Source: https://github.com/git/git/blob/master/setup.c.
The real permission regression fails on base with Ok(Path), then passes with
the independent bounded negative-discovery probe. Fake Git tests cover opaque
not-a-repository with a surviving .git and explicit Operation not permitted.
Actual macOS TCC EPERM was not reproduced here. Denial deliberately concealed
as ENOENT remains indistinguishable; documented rather than guessed.

## Fixed verification

With the explicit unlock fix: **200 parallel codex_integration runs, 0 failures**,
using the same cargo command and default parallel test harness, no retries and
no RUST_TEST_THREADS override. Output: /tmp/asessions-fixed-loop.log.

The permission and descriptor-inheritance regressions both failed first on the
base code and passed after their respective fixes. Full fmt/clippy/test checks passed (including the new regressions and
Digest upgrade fixtures), with no rerun of failures; no version bump, merge or tag is part of this
fix. The required macOS/ARM and Linux/musl CI jobs exercise the same tests.
