# Archive maintenance and diagnostics

[Back to README](../README.md)

## Everyday commands

Set `archive` to your Archive directory. Each Archive command accepts `--archive` explicitly:

```sh
agent-sessions pull --archive "$archive" --render
agent-sessions render --archive "$archive"
agent-sessions activity --archive "$archive" --day YYYY-MM-DD
agent-sessions info --archive "$archive" NATIVE-ID
```

Replace date and identity placeholders with actual values. `activity --json` and `info --source SOURCE --json` provide structured inspection. `show T3-THREAD-ID` resolves an exact T3 Thread link to its provider Session. The `search` command is not implemented yet.

Pull without `--render` updates Captures and indexes only. `export` streams a Bundle for remote collection and needs no Archive; it is normally invoked by fleet Pull rather than by hand.

## Backup and rebuild

Back up the whole Archive directory. Durable inputs include Captures and sidecars, media, Digests, Project Assignments, Session Leads and feed Bookmarks, Host attribution preferences, fleet health history and configuration. A database backup alone is insufficient.

```sh
agent-sessions rebuild --archive "$archive"
agent-sessions render --archive "$archive"
```

Rebuild reconstructs SQLite from durable inputs without reading native Stores or invoking models. It does not render Views; run Render separately. Regenerating derived files cannot recover missing durable evidence.

A typical Archive contains:

```text
config.toml          configuration
captures/            compressed native evidence and sidecars
media/               captured Session Media
assignments/         durable Project Assignments
leads/               durable Session Leads
lead-feeds/          feed Bookmarks
host-attributions/   retained choices for ambiguous Host origin
host-pulls.json      fleet health history
digests/             durable model-generated summaries
archive.db           rebuildable SQLite index
views/               generated Transcripts and indexes
```

## Pull outcomes

Per-Host/Source reports distinguish new, extended, revised, unchanged, refused, retry-later and failed units. Successful changed and no-op runs exit zero; collection, parse, read and Archive failures exit nonzero.

Concurrent native writes cause bounded Capture retries. A still-busy Session reports `retry-later`, preserves its previous Capture and Source Bookmark, and remains eligible next time. Busy Sessions and bounded Git lookup deferrals alone are nonfatal. Repository-resolution deferrals have a separate report row. Each retry-later event remains observable; there is no automatic age-based escalation.

Unreachable SSH Hosts use the [configured grace period](configuration.md#local-and-ssh-hosts). Authentication and host-key failures remain errors. Store Retention warnings cover finite windows of 30 days or less; they do not change the native tool's settings.

Use `-v` / `-vv` for more detail or `-q` / `-qq` for less. `RUST_LOG` overrides the default logging filter.

## Host origin and full collection

```sh
agent-sessions origins list --archive "$archive"
agent-sessions pull --archive "$archive" --full-source claude
```

Origin attribution is independent of the elected Capture. Native birth-time evidence can establish origin; exact T3 execution evidence provides a secondary signal. Inconclusive evidence retains a previous choice and reports ambiguity. See the [origin attribution decision](adr/2026-10-09-origin-host-is-independent-of-projection-winner.md).

Use `--full-source SOURCE` when re-enumeration is needed beyond an incremental Bookmark. Rebuild alone cannot fetch native evidence that was never captured. View omission and [Capture admission exclusions](configuration.md#capture-exclusions) are separate policies; neither is a deletion command.

## Optional model work

Review `digest --dry-run` before upgrades or scheduled model requests. Keep the model configuration and tokenizer aligned. For generation, progress and explicit repair commands, see [Digests](digests.md). Keep enough free disk space for retained Captures, revisions, media and generated output.
