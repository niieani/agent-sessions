# Implementation Plan

Active backlog only. Move a whole slice to [PLAN-completed.md](PLAN-completed.md) when it lands, retaining its number, acceptance evidence, and heading. Never renumber or reuse slice IDs; partially completed slices remain here. Adjust unfinished slices freely (they are intentions, not commitments). Terms per [CONTEXT.md](../CONTEXT.md); requirements per [SPEC.md](../SPEC.md).

## Slicing principle

Every slice ends with something runnable against real data against representative input. Risk is retired in order: **capture format first** (the one thing that's expensive to change once data is archived — Conversations and Views are regenerable via `rebuild`; Digests and Project Assignments are additional durable inputs), then archival semantics, then scale, then fleet, then the long-tail adapters. Adapter behavior is pinned by native-format documentation and synthetic fixtures.

## Shape of the code

Single crate (`agent-sessions`), lib + thin bin. Split into a workspace only if it earns it later.

```
src/
  model.rs        Conversation, Message, SubagentLink, Orchestrator facts, Provenance — narrow canonical types
  adapters/       one module per Source; the only code that sees native formats
    mod.rs        the Adapter trait: enumerate(store, since) -> SessionRefs; capture(ref) -> Capture; extract(capture) -> Conversation
    claude.rs  codex.rs  pi.rs  cursor.rs  vscode.rs  t3.rs
  archive/        capture file store + SQLite (schema, upserts, bookmarks, rebuild)
  bundle.rs       Bundle format: tar stream of captures + manifest
  pull.rs         orchestration: hosts (localhost + ssh), self-deploy, export invocation
  render.rs       Conversation -> Transcript markdown
  cli.rs          clap surface
```

Key dependencies: `clap`, `serde`/`serde_json`, `rusqlite` (bundled, read-only mode for native stores), `zstd`, `tar`, `anyhow`/`thiserror`, `tracing`. Later: `rayon` (parallel extract), FTS5 (ships with rusqlite), `tiktoken-rs` if token counts survive the open question.

CLI surface (grows by slice): `pull` (the one user-facing verb; localhost is host zero from day one — ssh is just a transport added later), `export` (remote side, Bundle to stdout), `rebuild`, `render`, `search`, `show`.

## Formats to pin early (the non-regenerable surface)

- **Capture file**: `captures/<host>/<source>/<native-unit-id>.<ext>.zst` — verbatim native payload (original JSONL bytes for file Sources; typed NDJSON of the Session or Orchestrator Thread's raw rows/files for SQLite Sources). Path encodes Host/Source/id; a `<native-unit-id>.meta.json` sidecar carries the rest of Provenance (native path/revision, captured-at, Adapter version). Sidecar exists because native Conversation reconstruction must work from the captures directory alone — Provenance cannot live only in the regenerable DB.
- **Bundle**: tar stream of capture files + sidecars + one `manifest.json` (host, export bookmark, session list). Designed to pipe: `ssh host agent-sessions export --since X` → stdout → central ingest, no temp files.
- **Project Assignment files** (slice 28): `assignments/<source>/<identity-component>.json`; independent durable classification input, indexed by rebuild. Back up these files, Captures, Digests, and configuration.
- **DB schema** (regenerable, so free to evolve): `sessions` (identity per ADR-0004, title, project, times, dialog flag), `session_hosts` (which Hosts contributed), `messages`, `subagent_links`, Orchestrator Thread projections/Link facts/resolved Augmentations, `bookmarks(host, source)`, FTS table, `meta(schema_version)`.

## Slices

Only unfinished slices remain below. Completed implementation and acceptance evidence live in [PLAN-completed.md](PLAN-completed.md).

### ☐ 7 — VS Code adapter
The gnarliest port: three storage variants, including the delta-encoded JSONL mutation journal (kind 0/1/2 replay). Capture the *raw journal*, replay at extract time — so journal-replay bugs are fixable retroactively.
**Done when**: the Adapter discovers each supported `workspaceStorage` variant and renders its synthetic fixtures correctly.

### ☐ 8 — Views & search
FTS5 over Conversations (`search`), manifest View, opt-in Action extraction (`render --include-actions` re-reads the Capture — tiers 4–6 on demand), resolve the token-count open question. Eager-vs-lazy rendering decided here by usage feel.
**Done when**: "what did I ask about X last month, on any machine?" is one command.

### ☑ 15 — Agent-created Codex task inputs

Project the initial task delivered to a Codex Desktop agent-created Session without confusing it with either an end-user Message or a Subagent Parent Instruction. The narrow source-native rule is documented in [formats/codex.md](./formats/codex.md#51-agent-created-task-input-function_call_output--agent-message): accept only an opening `codex_app.create_thread` result with a well-formed, nonempty `codex_delegation` envelope in a `thread_source:"agent_created_thread"` Session, preceded solely by filtered injected-message context. It becomes an Agent Message rendered with `<agent>`; all other function results remain Actions.

**Landed** 2026-09-14: Codex Adapter revision 5 extracts the validated delegation input at its native timestamp, adding the `Agent` Dialog role without changing user-Message matching or subagent link semantics. `<agent>` joins the reserved renderer delimiters, and Agent Messages start a Workday so their following assistant replies remain together across midnight. Regression coverage proves the expected agent-created task renders, an identical ordinary-user-thread tool result, a later tool result, and an empty-source-id envelope stay omitted Actions, and an Agent turn has Workday attribution. Rebuilding a Catalog re-extracts existing Codex Captures; no native Store is read or modified.

## Testing strategy

- **Fixtures**: small synthetic native records under `tests/fixtures/<source>/`; fixture → expected Transcript golden tests.
- **Fidelity review**: compare synthetic native evidence and rendered Transcripts for complete Dialog, ordering, Action exclusion and resolved Subagent links. Keep any private acceptance evidence outside the repository.
- **Property tests** where formats are adversarial: journal replay (slice 7), growing-session extension (slice 2: extend-then-rebuild ≡ fresh-ingest).
- **Benchmarks** land with slice 3 and get re-run per adapter slice; regressions are findings, not noise.

### ☐ 32 — ChatGPT evidence and incremental browser collection

Explicit staging Archive first. Stream native export shards into additive Captures; reconcile browser pages and Versions under the same identities. Expose known ancestry/revisions for bounded discovery. Render all captured branch continuations without repeating prefixes. Preserve optional generated media separately and user attachment references in Captures. Prove actual export ingestion, two unchanged runs, Rebuild equivalence, complete Markdown link targets, interruption recovery, and fidelity before production use. Browser collection and daily scheduling are external producers; no live website credentials enter the Archive.

Archive implementation now uses ordinary Pull/Bundle collection from a browser-native Store, machine Capture Provenance with account sidecar identity, native revision ordering and compacted evidence, independent opening Sessions, stable-tail Fork Views/Shared Segments, Path End Digests and source-neutral content-addressed Session Media with separate materialization. Render reports verified form-only changes. Staging verification uses a scratch Archive/output and a scratch copy of the Vault Template; production remains unchanged. Extension Store metadata/status integration, cold/locked-browser acceptance, scheduling/state cutover, production Project Name Tables and local Digest backfill remain operational acceptance work.

### Remaining ChatGPT operational acceptance

- Extension/runner review findings remain in its repo: resumed-job attribution,
  shared run-state writers, pickup/authentication deadlines, restart recovery,
  queue/bookmark cutover and workflow updates. No extension or Vault edits in
  this change.
- Production Project Name Table generation and local Digest backfill remain
  external operational work; staging verification uses scratch Archives/Views.
