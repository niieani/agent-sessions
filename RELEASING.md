# Releasing agent-sessions

Releases are explicit version tags, not automated version bumps. `Cargo.toml` is
the version authority; `agent-sessions --version` reports that
version followed by the build's commit hash (`agent-sessions 0.2.0 (08ebf0c0ef47)`),
and the SSH fleet handshake compares the whole line. GitHub records the source commit
on the release tag and, for public builds, in the archive's provenance attestation.

Every release that changes behaviour must bump the package version: the current
fleet redeploys only when the version changes. Never ship changed behaviour under
an unchanged version.

1. On a release branch, bump `[package].version` in `Cargo.toml`, then run
   `cargo check` to refresh this package's version in `Cargo.lock`. Keep dependency
   upgrades separate. Commit both files and merge through a green PR.
2. On the approved commit on `main`, verify `cargo fmt --check`,
   `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`.
3. Check that the release does not regenerate existing Digests:

   - `cargo test --locked --test digest_upgrade` must pass. It loads every
     `tests/fixtures/digest-upgrade/<release>/digests` tree with an unchanged
     Dialog and requires zero candidates and no model calls through the library,
     and `0 new · 0 redo` with no warning through `digest --dry-run`. If this release changes the stored Digest
     format, add `tests/fixtures/digest-upgrade/v<version>/` written by this build.
   - Run the release build's dry-run against a copy of a real-shaped archive,
     never the live archive or the installed binary. The dry-run takes no lock,
     but a copy keeps a release candidate's reads off the live Archive.
     If a check also needs Pull or Render, first replace the clone's Host list
     with a single localhost entry (keep the configured Host name, no
     `ssh`) and redirect every View output to a temporary directory. Never Pull
     the fleet with a pre-release candidate: its handshake can self-deploy over
     remote binaries even when the Archive is a clone. Custom Templates must
     emit `session.project.unresolved` when they display unresolved status.


     ```bash
     cp -cR /path/to/archive /tmp/digest-release-check   # APFS clone
     target/release/agent-sessions digest --archive /tmp/digest-release-check --dry-run | tail -1
     target/release/agent-sessions digest --archive /tmp/digest-release-check --dry-run --json \
       | jq '{new, redo, redo_reasons, fingerprints, estimate, warning}'
     ```

     The last line reads `digest dry-run: N new · M redo (<reasons>) ·
     K fingerprints · <duration or no rate history>`. Run the previous
     release's binary on the same copy first: the planned Session Days
     (`new_days + redo_days`) must not exceed the previous release's (0.4.5
     and earlier print them as `N candidates`), and `redo_days` must not grow.
     `title refresh` follows changed or new Session Days and is expected. A
     `WARN:` line on stderr means redone Session Days exceed the new ones or
     `--redo-warn-fraction` of existing Day Digests; treat it as a regression
     unless the release intends it. When the planned work regenerates existing
     Digests, the release notes must state that, why, `redo` with its reasons,
     and the estimated duration.
4. After the operator approves the release, tag and push that commit using its package version:

   ```bash
   version=$(cargo metadata --locked --no-deps --format-version 1 | jq -er '.packages[] | select(.name == "agent-sessions") | .version')
   git tag -a "v$version" -m "agent-sessions $version"
   git push origin "v$version"
   ```

The `Release` workflow rejects a tag that differs from `v<Cargo.toml version>`.
Native ARM macOS and x86_64 Ubuntu runners format-check, lint, test, and build
with the lockfile before publication.
Linux builds target static musl and must pass an ELF check for no interpreter or
shared-library dependencies. `rust-toolchain.toml` pins the exact Rust version,
components, and targets for both workflows and source builds. All action
references use full commit SHAs with version comments; checkout does not persist
credentials. Update these pins deliberately and validate both platforms.

Both build jobs must succeed before publishing one GitHub Release with:

- `agent-sessions-<version>-aarch64-apple-darwin.tar.gz`
- `agent-sessions-<version>-x86_64-unknown-linux-musl.tar.gz`
- `SHA256SUMS` (SHA-256 of both archives, filenames relative to the download directory)

Each archive contains the `agent-sessions` binary at its root. Public
builds use `actions/attest` in a separate job that only downloads and attests
archives; tests and build scripts run without OIDC or attestation write access.
Publication waits for this job to succeed. Private builds skip attestation issuance
because GitHub requires Enterprise Cloud for private-repository attestations.
macOS binaries use Rust's ad-hoc signing; Developer ID signing/notarization is
outside this release setup.

Before asking the fleet to upgrade, download both assets, verify `SHA256SUMS`,
and smoke-test `--version` on each platform. Follow the README install commands
and change the nightly executable path to `$HOME/.local/bin/agent-sessions`.
For mixed-platform SSH Pull, upgrade remotes to the same release before the
central Host. Self-deploy copies the central binary only to same-platform
remotes; a remote on another platform with a different version fails its Pull
and keeps its own binary.

## Fleet installation

Install the same release on every Host before the next fleet Pull. Use the README installation commands and your deployment system; mixed-platform Hosts must receive their matching binary. Verify `--version` on each Host before upgrading the central collector.

If a build fails before publication, fix the cause and use a new version/tag, or
rerun the failed jobs for a transient failure. If the publish job fails, inspect
whether a release was already created before retrying: the workflow intentionally
fails on an existing release rather than overwriting published assets. Never move
a published tag or silently replace its assets.
