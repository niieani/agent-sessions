# Agent Sessions

Archive AI agent conversations from your computers and turn them into readable Markdown. Keep the original evidence even after an agent tool rotates its logs, changes formats or is uninstalled.

Agent Sessions is a local Rust CLI. It collects from local files and remote machines over SSH, stores compressed Captures, and writes Markdown you can browse with ordinary file tools. Collection and rendering need no model or hosted service; model-generated summaries are optional.

## What it does

- Collects Claude Code, Codex, Cursor and Pi conversations.
- Links T3 Code orchestration metadata to the underlying agent Sessions.
- Imports ChatGPT exports and captured browser evidence.
- Preserves original records while producing Markdown grouped by Project or workday.
- Rebuilds its SQLite index and generated files from durable Archive inputs.

Collection reads native stores without modifying them. The optional `push-titles` command explicitly writes titles back to supported tools.

## Install

Install [Rust with rustup](https://rustup.rs), then build from source:

```sh
git clone https://github.com/niieani/agent-sessions.git
cd agent-sessions
cargo install --locked --path .
agent-sessions --version
```

The repository pins its Rust toolchain. Ensure Cargo's binary directory is on your `PATH`.

Release targets are Apple Silicon macOS and x86_64 Linux. See [installation](docs/installation.md) for release binaries, checksum verification and upgrades.

## Quickstart

Choose an Archive location outside your agents' own data directories:

```sh
archive="$HOME/agent-sessions-archive"
agent-sessions pull --archive "$archive" --render
```

The command creates the Archive, collects available local Sources, and writes Markdown under `$archive/views/`. Missing default Stores are treated as empty. No configuration or model credentials are needed for local collection.

Run the same command again to collect changes. Captures retain native evidence; Transcripts show conversation text with links and references, rather than full tool-call logs.

Add `config.toml` inside the Archive when you need SSH Hosts, custom Store locations or different output settings. For example:

```toml
[views]
timezone = "UTC"
day-start = "04:00"
unit = "session-day"
```

This groups Transcripts by workday, starting at 04:00 UTC. After changing View settings, regenerate them without collecting again:

```sh
agent-sessions render --archive "$archive"
```

## Common commands

| Task | Command |
|---|---|
| Collect and render | `agent-sessions pull --archive "$archive" --render` |
| Regenerate Markdown | `agent-sessions render --archive "$archive"` |
| Rebuild the SQLite index | `agent-sessions rebuild --archive "$archive"` |
| List activity on a date | `agent-sessions activity --archive "$archive" --day YYYY-MM-DD` |
| Inspect a Session | `agent-sessions info --archive "$archive" NATIVE-ID` |
| Plan optional summaries | `agent-sessions digest --archive "$archive" --dry-run` |

Use `agent-sessions --help` or `agent-sessions COMMAND --help` for command options. Summaries require a configured model endpoint and matching local tokenizer; see [Digests](docs/digests.md).

## Data and backups

An Archive contains compressed native records, configuration, optional summaries and other durable metadata, plus a rebuildable SQLite index and generated Views. **Back up the whole Archive directory.** Rebuilding the index cannot recover deleted native evidence.

Archives can contain private prompts, paths, tool output and attachments. Keep your Archive and exports out of public repositories. Collection does not prevent the original tools from applying their own retention policies.

## Current limits

- VS Code / Copilot Chat collection and the `search` command are not implemented yet.
- Native formats change; unsupported or malformed input can require an Adapter update.
- ChatGPT browser collection requires an external producer; this CLI does not sign in to ChatGPT or operate a browser.
- Summaries are model-generated and can be wrong. Original Captures remain available for verification.

## Documentation

- [Installation and upgrades](docs/installation.md)
- [Configuration, SSH collection and output settings](docs/configuration.md)
- [Model-generated Digests](docs/digests.md)
- [ChatGPT imports and collector integration](docs/chatgpt.md)
- [Archive maintenance and diagnostics](docs/operations.md)
- [Session Lead feeds](docs/session-lead-feed.md)

For implementation details, see the [specification](SPEC.md), [glossary](CONTEXT.md), [native formats](docs/formats/) and [architecture decisions](docs/adr/).

## Contributing

Bug reports should include the tool version, failing command and a small synthetic reproduction. Keep private session logs and credentials out of issues and PRs.

Before submitting code:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

See [AGENTS.md](AGENTS.md) for project contracts, [the plan](docs/PLAN.md) for unfinished work, and [RELEASING.md](RELEASING.md) for release maintenance.

## License

[MIT](LICENSE). Dependency license texts and notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
