# Completed implementation slices

Technical scope of completed slices. Current contracts live in SPEC.md and the ADRs; validation lives in the Rust suite and synthetic fixtures. Private operational acceptance records are excluded.

### Official release builds — 2026-10-08

Implemented tag-driven GitHub Releases for ARM macOS and static x86_64 Linux
musl, locked native-runner tests/builds, version/tag agreement, archive checksums,
public-repository provenance, and PR/main CI (tests, clippy, formatting). README
documents authenticated release downloads, checksum verification, the fixed
`~/.local/bin/agent-sessions` fleet path, locked source builds, and mixed-platform
upgrade ordering. [RELEASING.md](../RELEASING.md) defines the manual release steps.

### ☑ 0 — Skeleton

`cargo init`, clap skeleton, error/logging conventions, `cargo fmt`/`clippy`/`test` clean. Half a day of ceremony so no later slice pays it.

### ☑ 1 — Walking skeleton: Claude Code, localhost, end to end

`pull` (localhost only, no ssh) runs enumerate → capture → extract → store → `render` for the Claude Code Store. Conversation = user/assistant text messages only; no subagents yet (subagent files are captured but skipped at extract).claude/projects` (42 MB — fast to iterate on).

### ☑ 2 — Archival core: idempotency, growth, rebuild

Bookmarks; re-running `pull` with nothing new is a fast no-op; a Session that grew gets extended (same identity, richer capture supersedes); `rebuild` regenerates `archive.db` from captures alone, byte-for-byte equivalent in content.

### ☑ 3 — Scale: Codex + Pi, streaming, benchmarks

Codex adapter (event dispatch, guardian-session drop, `<environment_context>`/`<skill>` filtering) and Pi adapter (trivial; proves the trait fits a third source cheaply). Streaming line reader — never a whole file in memory. Date-partition pruning for Codex enumerate.

### ☑ 4 — Fleet: export, Bundle, pull over SSH

`export` streams a Bundle to stdout; `pull` gains ssh transport (system `ssh`), host config, binary self-deploy (scp on missing/version-mismatch), central bookmarks driving remote `--since`. Validate transports using synthetic Stores and a controlled SSH test Host.

### ☑ 5 — Subagents

`subagent_links` in model + DB; parent/child bidirectional resolution; Transcript interleaving by timestamp; `<parent>` re-tagging in child Transcripts. Existing captures re-extract to gain links — first real proof that Extract re-runs pay off.

### ☑ 6 — Cursor adapter

Read-only open (never a write lock), date filter pushed into SQL (`json_extract`), bubble-loading fallbacks and Lexical rich-text extraction. Capture = per-session raw KV rows.

### ☑ 4b — CLI output & logging (independent; land any time after 4)

Pleasant, cron-friendly output: per-Source summary after `pull` (found / new / extended / unchanged / failed, timing), progress indication for long cold ingests (the 2.5 GB Codex first run), visually distinct warnings, configurable verbosity (`-v`/`-q`/`RUST_LOG`), honest exit codes (nothing-new vs ingested vs errors). Folds in the deferred review findings: one bad session must not abort a pull (log, count as failed, continue); orphaned View files swept on render (views/ becomes a true pure function of archive content); stale `.tmp` capture files swept; `<gap>` markers render true durations (no "1 hour" for a 20-minute day-change, no marker from out-of-order timestamps); capture stats re-stat'd around the stream so a sidecar never over-claims its body.

### ☑ 4c — Project and Unfiled View grouping (independent; land any time after 4)

Make Project a derived View classification rather than treating every recorded working directory as one. Add `[views] unfiled-patterns` configuration for user-specific synthetic cwd trees; retain every original path in the Conversation; render into `views/projects/<name>/` or `views/unfiled/<YYYY-MM>/`; disambiguate same-basename Project paths with deterministic path hashes; merge the same path across Hosts.

### ☑ 9 — Codex subagent projections and inherited-context Views

Make ordinary Codex child Sessions extractable without duplicating their parent's materialized history. First introduce an explicit Archive-owned direct-parent Capture resolver at the Adapter/Archive IO seam; an Adapter must receive the resolved same-Host, same-Source parent Capture as input rather than discovering files itself. Preserve the current Capture-only outcome whenever that dependency or the required native invariants are absent.

### ☑ 10 — Cursor subagent projections and native task graph

Project Cursor subagent Composers as ordinary independent child Dialog, then resolve their graph from the native composer/task records. Keep `isSubagent`-only Composers Capture-only; it is a classification hint, not proof of a child transcript or parent identity.

### ☑ 11 — Codex persistent goals and captured objective attachments

Preserve the user intent inside Codex persistent-goal runtime envelopes without retaining their injected continuation, budget, audit, or completion instructions.

### ☑ 12 — View Kinds: configurable classification and the Omitted/Loose rename

Replace `[views] unfiled-patterns` and the render-time `ViewGrouping` classifier with ordered, user-named Kinds per SPEC "View Kind configuration": match conditions (`path`/`message`/`source`) plus exactly one placement Treatment (`omit` / `loose` / `output`). Rename the Unfiled concept to Loose throughout code and Views (`views/unfiled/` → `views/loose/`); no config migration — the removed key fails loudly at load. A Kind claims a top-level Session and its Subagent children together; orphaned children are evaluated directly. Render summary gains per-Kind claim counts; `render -v` lists each Session's id, claiming Kind, and destination.

### ☑ 13 — T3 Orchestrator Augmentation and Claude queued prompts

Capture T3 Code Orchestrator Threads, resolve their exact provider-native identities to existing Sessions, and attach provenance-bearing metadata without creating duplicate Conversations. In the same slice, fix the independently proven Claude `queued_command` Dialog loss. The definitive formats/invariants are [formats/t3.md](./formats/t3.md) and [formats/claude.md](./formats/claude.md#queued-user-prompts); the T3 implementation reference is `/work/example/SoftwareReferences/t3code` (especially `docs/internals/overview.md`, `apps/server/src/config.ts`, `apps/server/src/persistence/Migrations/`, `apps/server/src/persistence/ProviderSessionRuntime.ts`, and provider resume-cursor parsers).

### ☑ 14 — Workdays: activity queries and day-aware Transcripts

Answer "what did I work on that day?" without re-reading Captures, and make Transcripts show the same day boundaries the query uses. Introduces the configurable Workday (CONTEXT.md): `[views] timezone` (IANA, default local) + `day-start` (default midnight); a user Message owns its Workday, following assistant Messages inherit it, a leading assistant Message takes its own.

### ☑ 15 — Capture time, Codex sqlite titles, Ongoing

Persist each Session's winning `capturedAtMs` in the regenerable database; read Codex `threads.name` from `state_5.sqlite` (read-only, WAL-aware) as the native title with the JSONL event as fallback; compute Ongoing per SPEC "Session Days".

### ☑ 16 — Session Days, Templates, Layout, Name Tables

`[views] unit = "session-day"`, minijinja templates with built-in defaults, `[views] output`, `[views.layout]`, index and json layout keys, Name Tables, deterministic collision suffixes, previous/next Session Day references.

### ☑ 17 — Digests

`digest` command: Chat Completions with JSON-schema output, Kind gating, Ongoing and current-Workday skipping, incremental coverage, Topic and Locked Title, Session-level Digest, truncation, `--dry-run`.

### ☑ 18 — Vault first render

Configurable Templates and Layouts render Session Days for external note systems. The Archive exposes source-neutral data; template consumers own their schema.

### ☑ 20 — Backfill widening

Extend Digests from one week to full history in steps, with `--dry-run` token estimates before each step.

### ☑ 21 — Title Write-back

`push-titles` per ADR-0006: Codex and Claude Code routes, dry-run, own-write tracking.

### ☑ 22 — JSON Views

`json` layout writing the render data model per unit.

### ☑ 23 — Retitle and rename list

`retitle` re-locks a Session Day's title to its current Digest title and prints the old→new path list.

### ☑ 24 — Digest robustness and template trimming

Fixes surfaced by the first full backfill. Two request schemas (first-day `topic` required, continuation without `topic`) and tool-side `<Topic> - <facet>` composition remove the three dominant validation failures; Session Title validation drops the word count (non-empty, ≤ 80 chars) and the prompt suggests two to six words; `[digest] retries` (default 2) retries a Rejected Digest in-process with the error as a correction turn and appends every Rejected Digest to `<workday>.rejected.jsonl`; `[digest.prompts]` overrides the built-in prompt texts. Render enables `trim_blocks`/`lstrip_blocks` with the trailing newline kept; built-in template goldens stay byte-identical. `digest --list-omitted` prints Digest directories of Sessions the current Kinds omit so they can be trashed. Verify: fixture where the model answers without a Topic once then correctly (one retry, one sidecar line, a valid Digest); a continuation schema that has no `topic` key; a Vault-template render with no blank lines inside frontmatter.

### ☑ 25 — Store Retention warning and Codex External Name overwrite

Two consequences of the first write-back dry-run. (1) The Claude Adapter reads `cleanupPeriodDays` from the Host's `~/.claude/settings.json` during Enumerate; Export carries per-Source Retention in the fleet manifest; `pull` warns once per Host and Source when Retention is finite (absent setting = 30 days), naming the setting; `[pull] warn-store-retention = false` silences it. (2) `[titles.<source>] overwrite-external` (default false) lets `push-titles` replace External Names on that Source; the Codex route honours it, Claude's route may honour it, Cursor never writes. Rename the CLI dispositions to the glossary's Push / Unchanged / Keep / Missing (`skip-foreign` becomes `keep`). Rejected Digest lines record the time of their own attempt, not the run's start. Verify: fixture Store with an External Name is byte-identical without the flag and updated with it; a fixture settings file with and without the key produces exactly one warning or none; docs/formats/claude.md Retention section; CONTEXT terms Native Name, Pushed Name, External Name, Store Retention.

### ☑ 27 — Kind matching over Dialog Statistics

Implement the accepted [Dialog Statistics contract](../SPEC.md#dialog-statistics) and [ADR](adr/2026-09-04-kind-matching-uses-current-dialog-statistics.md). Extend the existing matcher with counts and Unicode character totals plus numeric comparison operators; share current whole-Conversation facts between Digest and Render. Preserve first-match priority and Subagent inheritance. No built-in minimum length or greeting detector.

### ☑ 26 — Recovered Sessions

ADR-0007. During Extract, a resolved Orchestrator Link whose provider Session has no Capture on any Host produces a Recovered Session: Source and native id from the Link, Dialog from `orchestrator_projection_messages`, Project and Host from the Thread, `recovered_via` provenance naming the Orchestrator Thread. It participates in Kinds, Session Days, Digests, Views (frontmatter `session-recovered-via`), and write-back. A later real Capture supersedes it deterministically on the next Extract/rebuild, and the Thread falls back to Augmentation. Verify: fixture with a T3 Thread and no provider Capture yields one Conversation with the provider identity; adding the provider Capture afterwards replaces the Dialog byte-for-byte with the provider extraction and keeps the Augmentation; real-archive run recovers the twelve Claude Sessions found on 2026-09-04.

### ☑ 28 — Durable Project Assignments

Implement [Project Assignments](../SPEC.md#project-assignments) and their [ADR](adr/2026-09-04-project-assignments-are-durable-files.md): assign/unassign commands, versioned files keyed by Source/native identity, atomic publication, and regenerable database indexing. Validate identities and project references; never edit Capture evidence.

### ☑ 29 — Shared Project resolution and Digest context

Depends on 28. Use one Project resolver for Digest and Render, respecting explicit assignments, workspace mappings, Loose placement, and Omitted precedence. Preserve recorded paths for Provenance and matching. Supply inherited Subagent project context. Send `project: null` for projectless Loose Sessions; improve Topic/facet and Session-title prompts to avoid repeating project names or deriving Topics from temporary directory names.

### ☑ 30 — Transcript hierarchy and complete Subagent metadata

Implement the source-neutral context additions and Vault profile contract in [SPEC](../SPEC.md#transcript-relationships-and-title-context). Expose first-day and parent-day references, one-based Session Day indices, and explicit unknown spawn-day state; use resolved paths. Share common Vault metadata while preserving Untagged agent-authored Subagents. Add `session-subagent`, `session-parent-id`, `session-day-index`, and the agreed scalar `up` relationships. Update the Vault's property/schema documentation and template together when this slice runs.

### ☑ 31 — Project-separated filenames and explicit Digest repair

Depends on 29. Implement the `<workday> <Project> - <Locked Title>` Vault layout and deterministic exact-prefix cleanup described in SPEC. Provide explicit maintenance for replacing the selected Loose Sessions' Topic, Session Title, day Digests and Locked Titles together; ordinary digest/render remain incremental and title-stable. Preserve Title Write-back ownership and pending recovery state. Keep normal Retitle behavior for consumers with established links.

### ☑ 32 — Vault batch regeneration and acceptance

Render repeatability, hierarchy links, complete Subagent metadata and Digest repair are covered by synthetic fixtures and golden Transcripts.

### ☑ 33 — T3 Code title write-back

Extend `push-titles` to T3 Code before the first live push, so every harness shows the same Session Title. Route per [ADR-0006](./adr/0006-title-write-back-is-the-one-sanctioned-store-write.md): the `thread.meta.update` command (`{ threadId, title }`) dispatched to the running T3 server over its HTTP API with a paired bearer token, never a write to `projection_threads`; T3's own `project.meta.update` CLI (`apps/server/src/cli/project.ts` in the reference checkout) is the precedent, including its offline mode. Each T3 Thread linked to a Session gets its own Disposition against that Session's Session Title; T3 auto-generates thread titles and records no origin, so add `[titles.t3] overwrite-external` with the same default and Pushed Name bookkeeping as Codex. Dry-run must not need a running server.

### ☑ 34 — Projects keyed by repository

Sessions from git worktrees currently render as hashed Projects (`example-assistant@9deca999`) because Project identity is the recorded workspace path and same-basename paths collide. Make the recorded repository the primary Project key: normalize `git_repository_url` to `owner/repo` (strip scheme, user, host and host aliases such as `example.github.com`, `.git` suffix, trailing slash; ssh, scp-style and https forms all reduce to the same key), group every Session with that key into one Project, and fall back to the path rule only when no repository is recorded. `[views.project-names]` accepts an `owner/repo` key alongside full paths; a path key still wins for the exact path. Name collisions between distinct repositories with the same `repo` segment keep the hash suffix rule. Rendering stays byte-stable for Sessions whose Project does not change.

### ☑ 35 — Session Intent and next-step attribution in Digests

Continuation Session Day Digests lost what the user set out to do, and next steps did not say who proposed them or who must act, so a reader of one Digest (the Vault's nightly Work Recap) assigned the user work that belonged to someone else. Implement the Intent contract in [SPEC](../SPEC.md) and [ADR](adr/2026-09-27-session-intent-is-stored-on-each-session-day-digest.md): the first Digest establishes Intent from the user's opening turns; continuations receive it in Context and return `null` to keep it; a continuation after Digests that predate Intent receives the Session's opening user turns and must establish it; every Session Day Digest stores the Intent in effect and Render exposes it. The day prompt asks for attribution of next steps that are not the user's own plan. Prompt version 3; existing Digests stay valid and are not regenerated.

### ☑ 36 — Codex Composer Prompts

Current Codex writes `## My request:` instead of `## My request for Codex:` after an attachment preamble; the extractor dropped the whole text part, so requests vanished or read only `[image]`. Parse Composer Prompts per [formats/codex.md](./formats/codex.md#composer-prompts): keep the typed request when every preceding section is Machine Context (attachment/application labels, `source="ambient-ui-state"` elements, in-app browser, Chrome tab and IDE state), keep user-chosen context and unrecognized layouts verbatim, and never let a Message vanish because its request was empty.

### ☑ 37 — Forced re-digest of one Session Day

`digest --session SOURCE:NATIVE-ID --day D --force` regenerates a Session Day whose Digest already covers its Messages, for extraction changes that do not grow the covered range. Earlier Session Days must have current Digests so the Intent chain is right; `--session` alone restricts ordinary candidates.

### ☑ 38 — Name Tables from generated files

`[views] name-tables` lists Name Table files an external tool regenerates (for example, a note-system integration) and merges them with inline entries; every key has one origin, so duplicates fail config load ([ADR](adr/2026-10-01-name-tables-load-from-generated-files.md)). Project keys may be remote URLs, normalized with Render's repository-key rule. Two Render fixes surfaced by renaming many Projects at once: a kept View whose on-disk spelling differs from the plan by case (a Project renamed only by case) is renamed to the planned spelling, and Projects named by a Name Table no longer force hash suffixes onto raw names they collided with.

### Session Leads — 2026-10-08

Tool-neutral Feed v1 contract, configured HTTP sync with page-level durable publication and per-feed Bookmarks, pending Session Leads and rebuildable index, CLI inspection, and `session.led_by` template/JSON context. Regression coverage: paging/authentication, replacement, pending-before-Capture, rebuild, Session Day re-render, idempotency, malformed pages, and explicit failures. No native Store, production Archive, or Vault writes.

### ChatGPT promotion review — 2026-10-20

Captured native Project directory facts, hard errors for missing referenced Project names, unsuffixed native names and validated native-id Name Table keys. Plain Dialog roles with archive-backed verification; lowercase YAML booleans. Shopping/layout symbols and legacy citations interpreted from native evidence, with explicit unresolved content gaps. Hosted Ongoing state requires recent Messages. Pagination preserves known ancestry; native revision ordering preserves decimal precision. Cross-Host native evidence feeds one complete graph across extraction, status and Render. Orphan export media remains catalog/media-only evidence. Durable Dialog fingerprints detect same-count corrections while unchanged Shared Segment navigation preserves Digests. Native Stores, production Archive, Vault and staging output remain untouched.
