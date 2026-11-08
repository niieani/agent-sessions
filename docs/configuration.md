# Archive configuration

[Back to README](../README.md)

Configuration lives in `<archive>/config.toml`. A local Pull needs no file: without configured Hosts, the CLI uses the current machine's hostname and default Store locations.

## Local and SSH Hosts

```toml
[[host]]
name = "local"

[[host]]
name = "remote"
ssh = "archive-example"
remote-bin = ".local/bin/agent-sessions"
unreachable-grace = "30d"

[pull]
unreachable-grace = "7d"
```

`archive-example` is a placeholder SSH alias. Configure authentication with your normal SSH setup. Omitting `ssh` means the Host is local. `name` is a durable Provenance and Bookmark key; keep it stable once collection starts.

The collector deploys a binary to compatible same-platform SSH Hosts when needed. Different-platform Hosts require their own matching release; see [installation](installation.md#upgrades-and-ssh-hosts).

An SSH Host that cannot be reached is skipped within its effective grace period; once overdue, it makes the Pull fail. Authentication and host-key errors are failures, not unreachable-host grace cases. Durations accept an integer followed by `ms`, `s`, `m`, `h` or `d`. `host-pulls.json` preserves fleet health history across index rebuilds.

Per-Host Store overrides are `claude-root`, `codex-root`, `pi-root`, `cursor-root`, `t3-root` and `chatgpt-root`. `cursor-root` names the database file; `t3-root` names the base containing `userdata/statev2.sqlite`. Configured local Hosts use these fields rather than the synthesized-localhost CLI root overrides.

## Workdays and Views

```toml
[views]
timezone = "UTC"
day-start = "04:00"
unit = "session-day"
```

Without overrides, Workdays use the collector's local timezone and begin at midnight; Views use whole Sessions. A user turn owns its Workday, including assistant replies that cross the boundary. `activity --day` uses the same clock.

By default, Markdown is written under the Archive's `views/` directory. Optional `output`, `template`, `index-template` and `layout` settings control presentation. After changing View settings, run `render`; collecting native records again is unnecessary. Keep output directories outside native Store roots and durable Archive input directories.

Kind rules classify Sessions for output, loose placement or omission. An omitted View still has its Capture. Capture exclusions below instead control admission into the index. See the [View Kind contract](../SPEC.md#view-kind-configuration).

## Capture exclusions

For example, exclude recorded working directories inside a disposable sandbox:

```toml
[pull]
exclude-paths = ["**/disposable-sandbox/**"]
```

Merge this into any existing `[pull]` table. Patterns match the Session's recorded working path, or its native project directory when absent, using the same glob rules as Kind paths. The collector forwards exclusions to SSH Hosts. Standalone Export accepts repeated `--exclude-path` arguments.

Excluded copies do not participate in same-Host native-copy election or contribute Bookmarks. Eligible copies sharing a Source/native ID elect the lexicographically first absolute native path before Bookmark filtering. This prevents unchanged copies from alternating; independent conversations need distinct native IDs.

A policy change rebuilds the derived index on the next Pull. Existing Captures, Prior Revisions and Digests stay on disk. Removing the exclusion restores archived evidence; run Render to reconcile View files. A `pull --full-source SOURCE` can recover old-mtime records on stateless SSH Hosts. This policy neither deletes evidence nor regenerates Digests.

## Names, templates and integrations

Name Tables map recorded Projects, Hosts and Sources to display names; they do not change native identities. `name-tables` can load generated mappings from files alongside inline configuration. See [specification](../SPEC.md) for the full rendering contract.

Optional [Session Lead feeds](session-lead-feed.md) attach agent leadership and requester references without changing Message roles. [ChatGPT imports](chatgpt.md) and [Digests](digests.md) have separate setup requirements.
