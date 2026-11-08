# Transcript Metadata and Session Inspection

**Status:** implemented.

## Purpose

Keep rendered Transcript frontmatter compact and useful when reading a
Conversation. Put operational, provider-specific, and potentially volatile
facts behind an on-demand `info` command instead.

The Capture remains the complete source of truth. Both the persisted
Transcript facts and `info` output are re-extractable from it.

## Transcript frontmatter

In addition to the existing identity, project, title, time, message-count,
and subagent fields, render these fields when the Source provides them:

```yaml
startingModel: gpt-5.6-terra
startingReasoningEffort: high
git:
  branch: main
  commit: abc123
  repositoryUrl: git@github.com:org/repo.git
patchedFiles:
  - src/render.rs
referencedFiles:
  - docs/formats/codex.md
```

All fields are optional. Omit an unavailable scalar or empty list; omit the
whole `git` mapping when it has no available values.

### Starting model and effort

`startingModel` and `startingReasoningEffort` intentionally describe only the
session's initial effective configuration. A model change is possible but
uncommon; effort changes are more common. The Transcript must not present a
last-wins setting as though it applied to the entire Conversation.

For Codex, use the first `turn_context` that provides each value. If it is
absent, use the first `thread_settings_applied.thread_settings` value as a
fallback. Do not render a model/effort timeline in a Transcript.

### Git snapshot

Render available branch, commit, and repository URL as the Session's initial
Git snapshot. For Codex, read this only from the first `session_meta`:
resumed sessions emit further metadata and forked files replay ancestor
metadata, so later records are not authoritative for the new Session.

### File evidence

File lists have deliberately narrow semantics:

- `patchedFiles` contains paths backed by direct edit evidence. Codex derives
  it from the keys of successful `event_msg.patch_apply_end.payload.changes`.
  It does **not** claim to include changes made through shell commands or
  unstructured tool output.
- `referencedFiles` contains only paths carried in explicit native
  file-reference fields. Never mine paths from assistant prose, commands, or
  arbitrary tool arguments.
- A path under `projectPath` is rendered project-relative. A path outside the
  project remains absolute. Deduplicate deterministically while preserving no
  invented classification.

`referencedFiles` may be absent for a Source or Session. An empty list is more
honest than turning contextual file selections into claimed edits.

### Deliberately excluded from Transcripts

Do not render client/version details, token usage, model/effort history,
service tier, approval policy, sandbox settings, workspace roots, instruction
text, tool schemas, rate-limit/account data, or full source-specific metadata.
These facts are operational rather than narrative; some are sensitive or can
be large and volatile.

## Source facts discovered

### Codex

- `turn_context` contains `model` and `effort`; current settings events also
  record model provider, reasoning effort, service tier, and execution
  settings.
- The first `session_meta` has `git` (`branch`, `commit_hash`,
  `repository_url`), `originator`, CLI version, thread origin, and provider.
- `token_count` reports input, output, reasoning, cached-input, and context
  window values. These are usage reports, not a guaranteed cost calculation.
- `patch_apply_end.changes` provides exact paths changed by the patch tool.

### Claude Code

Assistant records contain model, effort, usage, and service-tier data; session
records contain Git branch and entrypoint. Tool and file-history records may
provide future file evidence, but only explicit fields may populate the two
file lists.

### Pi

`model_change` and `thinking_level_change` records identify configuration
changes. Assistant messages carry provider, model, usage, and computed cost.

### Cursor

`originalFileStates` and `newlyCreatedFiles` are edit bookkeeping. File
selections are references, not proof of an edit. Cursor also carries
per-bubble token counts and composer-level context-token data.

### VS Code

Requests carry `modelId`, token counts, and `editedFileEvents`. This Adapter
is not implemented yet; its fields become available when it is.

## `info` command

Add an on-demand inspection command:

```text
agent-sessions info <native-id> --archive DIR [--source SOURCE] [--host HOST] [--json]
```

`native-id` is any source-native session id, not necessarily a UUID. A bare
id searches every Source:

- one canonical Session found: inspect it;
- none: fail as not found;
- more than one: fail as ambiguous, list concise candidates, and require
  `--source`.

Identity within the archive remains `(source, native_id)`. A Session preserved
on multiple Hosts is one canonical Session, not an ambiguity. It inspects the
elected winner by default; `--host` selects a particular preserved Host copy
and fails if that Host has no copy.

The default output is concise and human-readable. `--json` emits structured
data for tooling.

`info` may show, when supported by its Adapter:

- full model and effort timeline; provider and service tier;
- latest and per-turn usage reports, including context window;
- client, version, and thread-origin details;
- coarse execution context (approval and sandbox modes, never instruction
  contents or writable-root lists);
- source-specific file evidence, retaining the distinction between patched,
  modified/created, and referenced files;
- other non-sensitive native facts that are useful to diagnose a Session.

It must label source-specific details and retain their native semantics rather
than flattening them into misleading cross-source claims.

## Architecture

The render seam receives only a typed `TranscriptFacts` projection: starting
model, starting effort, Git snapshot, and the two file-evidence lists. Persist
those facts with the per-Host projection so `render` remains a pure,
deterministic database-to-View operation and `rebuild` can restore it from
Captures.

The Adapter gains an inspection operation that reads a selected Capture on
demand and returns a `SessionInfo` result. Its implementation may share the
Adapter's internal scan with ordinary extraction, but it must not enlarge the
Transcript renderer's interface or force generic provider metadata into every
caller. `info` is the only initial consumer of source-specific detail.

The existing unimplemented `show` command may be renamed to `info`; no
compatibility behavior exists to preserve.

## Implementation coverage

- Codex supplies the complete Transcript fact set currently supported by the
  source: initial model/effort, first Git snapshot, and successful patch-tool
  paths. Its `info` details add initial client/thread facts, configuration
  timeline, coarse execution context, and latest token report.
- Claude supplies first assistant model, explicit effort when present, and
  first available Git fields. Pi supplies first model and thinking-level
  changes when present.
- Cursor supplies direct modified/created file bookkeeping as `patchedFiles`
  and explicit `context.fileSelections` paths as `referencedFiles`; it does
  not relabel selections as edits.
- Other Adapters retain an empty `sourceDetails` object until they implement
  a source-native inspection projection. The common capture identity and
  persisted Transcript facts remain available through `info` for all of them.

## Verification

- Fixture extraction tests cover each supported frontmatter field, omission,
  relative/absolute path normalization, deterministic deduplication, and the
  Codex first-metadata/fork rule.
- Golden Transcript fixtures cover the new YAML output.
- `info` tests cover unique lookup, absent lookup, cross-source ambiguity,
  winner default, explicit Host selection, and stable `--json` structure.
- Source-format tests prove that no free-text filename inference can populate
  either file-evidence list.
