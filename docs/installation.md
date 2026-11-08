# Installation and upgrades

[Back to README](../README.md)

## Build from source

Install Rust through rustup. `rust-toolchain.toml` pins the compiler, components and targets; keep `Cargo.lock` in use.

```sh
git clone https://github.com/niieani/agent-sessions.git
cd agent-sessions
cargo install --locked --path .
agent-sessions --version
```

Cargo installs into its configured binary directory, normally `$HOME/.cargo/bin`. Ensure it is on `PATH`. To build without installing:

```sh
cargo build --locked --release
./target/release/agent-sessions --version
```

The executable is `agent-sessions`. Install from this repository rather than assuming a crates.io package exists.

## Release binaries

When available, binaries are listed on [GitHub Releases](https://github.com/niieani/agent-sessions/releases). Supported release targets:

| Platform | Target |
|---|---|
| Apple Silicon macOS | `aarch64-apple-darwin` |
| x86_64 Linux | `x86_64-unknown-linux-musl` |

Linux releases link SQLite and zstd into a static musl executable and use rustls for TLS. macOS releases use system macOS libraries without requiring Homebrew runtime libraries. Bundled TLS roots update with the binary.

Use GitHub CLI to list releases and select a published tag:

```sh
gh release list --repo niieani/agent-sessions
```

The following Bash example installs an explicitly selected release. Replace `vX.Y.Z` with a tag from that list; it is a placeholder, not a published version.

```bash
set -euo pipefail
release_tag=vX.Y.Z
version="${release_tag#v}"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  *) echo 'Unsupported release platform' >&2; exit 1 ;;
esac
asset="agent-sessions-$version-$target.tar.gz"
download_dir=$(mktemp -d)
gh release download "$release_tag" --repo niieani/agent-sessions \
  --pattern "$asset" --pattern SHA256SUMS --dir "$download_dir"
(
  cd "$download_dir"
  awk -v asset="$asset" '$2 == asset { print; count++ } END { if (count != 1) exit 1 }' \
    SHA256SUMS > selected.sha256
  case "$target" in
    aarch64-apple-darwin) shasum -a 256 -c selected.sha256 ;;
    x86_64-unknown-linux-musl) sha256sum -c selected.sha256 ;;
  esac
  tar -xzf "$asset"
  ./agent-sessions --version | grep -q "^agent-sessions $version ("
  mkdir -p "$HOME/.local/bin"
  install -m 755 agent-sessions "$HOME/.local/bin/agent-sessions"
)
"$HOME/.local/bin/agent-sessions" --version
```

Where a public release has an attestation, verify the archive before extraction:

```sh
gh attestation verify "$download_dir/$asset" --repo niieani/agent-sessions
```

## Upgrades and SSH Hosts

Back up the Archive before upgrading. Use one consistent executable path in scheduled jobs so shell and scheduler `PATH` differences do not select different versions.

For SSH collection, install the matching release for each remote platform before upgrading the central collector. Set each Host's `remote-bin` to its installed executable, relative to that remote's home directory when appropriate. The handshake compares version and build identity.

The collector can deploy its own binary to a same-platform remote. It cannot deploy a macOS binary to Linux or vice versa: a different-platform remote with a mismatched version fails its Pull and reports which asset to install.

After an upgrade, check planned summary work with `digest --dry-run` before starting model requests. Unknown configuration keys fail instead of being silently ignored. See [Digests](digests.md) and [release maintenance](../RELEASING.md).
