# Unreachable Hosts are skipped within a grace; self-deploy never crosses platforms

**Superseded in part:** [Durable Host Pull Health](2026-11-07-durable-host-pull-health.md) replaces the disposable health decision and adds per-Host grace.

Status: accepted

The fleet includes laptops that sleep and VMs that are stopped for weeks. Counting every connection failure as a Pull failure made the nightly fail whenever a laptop was closed, which trains the operator to ignore failures. Pull therefore distinguishes an Unreachable Host — ssh exits 255 on the first probe with OpenSSH's `ssh: connect to host …` or `ssh: Could not resolve hostname …` diagnostic — from a failed one. An Unreachable Host is skipped with a warning and keeps its Bookmarks, so the next successful Pull catches up. Classification is an allowlist of those two OpenSSH diagnostics, not every exit 255: authentication and host-key rejections, and any message we do not recognize, stay failures, because they are misconfiguration on a running host rather than absence.

Skipping must not let a Host disappear silently. The Unreachable Grace (`[pull] unreachable-grace`, default `7d`) turns an Unreachable Host into a failure once its last successful Pull — one in which every Source succeeded — is older than the grace. The last-success time is stored in `archive.db` (`host_pulls`), not in a durable Archive file: it is operational monitoring state, not archive content, and nothing else is derived from it. It is excluded from the rebuild content dump and lost on `rebuild`. A Host without a recorded success is measured from when the Archive first tracked it (the first Pull that tried it). So losing the table restarts every Host's grace window. That delays detection of a missing Host by at most one grace window and never produces a false failure. We rejected a durable state file because it would add a non-reconstructible input to the backup set for a value whose loss is harmless.

Self-deploy compares the remote's `uname -sm` platform, probed in the same round trip as the version, with the platform of the central binary's build target triple, and copies the binary only when they match. On 2026-11-03 the previous behaviour overwrote a Linux remote's working binary with the macOS binary and then failed on re-handshake, leaving the remote broken. A cross-platform remote is now never written to. That Host fails and the error names the release asset (`<target>` triple) to install at `remote-bin`. Choosing and downloading platform assets centrally was rejected for now: fleet installation is owned by the infrastructure repository's install scripts, and the remote must not depend on the central Host for its binary.

## Consequences

- An offline Host costs nothing until its grace expires; operators see it as `unreachable` in every `pull` report.
- A connection lost after first contact (a laptop sleeping mid-export) is still a failure for that Pull.
- Mixed-platform fleets must install each release on every remote before the central Host; the central binary never repairs a foreign-platform remote.
