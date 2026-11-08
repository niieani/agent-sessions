# Claude Code native format


The Store is strictly read-only to us. It is plain files — no SQLite, no locks needed, but never write into it.

## Store layout

```
~/.claude/projects/
  <encoded-cwd>/                            one dir per project working directory
    sessions-index.json                     per-project index — PRESENT ONLY SOMETIMES, often stale
    <session-uuid>.jsonl                    one main Session per file (append-only JSONL)
    <session-uuid>/                         per-session side directory (only when needed)
      subagents/
        agent-<agentId>.jsonl               subagent Session (agentId = 17 lowercase hex chars, 'a'-prefixed)
        agent-<agentId>.meta.json           tiny sidecar: agentType/description/toolUseId/spawnDepth
        workflows/wf_<id>/                  workflow-spawned subagents (new ≥ mid-2026)
          agent-<agentId>.jsonl
          agent-<agentId>.meta.json
          journal.jsonl                     {"type":"started","key":"v2:<sha256>","agentId":"..."} rows
      workflows/scripts/*.js, *.txt         workflow orchestration scripts/prompts — not Sessions
    memory/MEMORY.md, *.md                  project memory — not Sessions
.DS_Store                               noise at every level
```

Observed sibling stores under `~/.claude/` that are **not** part of this Source's Store: `history.jsonl` (global prompt history: `display`, `pastedContents`, `timestamp` in **epoch ms**, `project`, `sessionId` — corroboration only), `tasks/<session-uuid>/N.json` (background task state), `sessions/` (daemon key/state files), `file-history/`, `shell-snapshots/`.

### Project directory name encoding — lossy, do not decode

The `<encoded-cwd>` dir name replaces every non-alphanumeric cwd character with `-`. For example, `/workspace/example-ui/.worktrees/demo` becomes `-workspace-example-ui--worktrees-demo`, and `example_console` becomes `example-console`. The encoding loses information.

**v2 rule: the project path comes from the `cwd` field of the first record that has one; decode the dir name only as a last-resort fallback for a session whose records carry no `cwd`, and treat the result as approximate.** `sessions-index.json` entries also carry an exact `projectPath` when present.

### `sessions-index.json`

`{"version":1,"entries":[{sessionId, fullPath, fileMtime (epoch ms), firstPrompt, summary, messageCount, created/modified (ISO), gitBranch, projectPath, isSidechain},...]}`.

Present in only 4 of 18 project dirs sampled; one dir (`...-example-everything`) contains *only* a stale index whose `.jsonl` files have been deleted — direct proof the Store prunes sessions and that the index outlives them. **Do not use it as enumeration authority; do not trust its freshness.** It is useful opportunistically (exact `projectPath`, human `summary`).

## Enumeration (cheapest strategy)

There is no date partitioning. The strategy is:

1. Walk `~/.claude/projects/*/` one level deep for `<uuid>.jsonl` (main sessions), plus `<uuid>/subagents/**/agent-*.jsonl` (subagents, including the `workflows/wf_*/` layer). Nested workflow layers must also be included (match `agent-*.jsonl` anywhere under a `subagents/` segment, at any depth).
2. Gate on file **mtime** vs the Bookmark. Appends bump mtime; this is the whole incremental story. Directory mtimes can prune: a project dir whose recursive newest mtime ≤ Bookmark needs no per-file stats, but note dir mtime only changes on entry create/delete, not on file append — so per-file mtime stat is the reliable gate. The store is small (tens of MB, ~65 jsonl files); stat-everything is cheap.
3. Skip: `sessions-index.json`, `*.meta.json` (read them alongside their `.jsonl`, don't enumerate them), `journal.jsonl`, `memory/`, `workflows/scripts/`, `.DS_Store`.

Largest file observed: 22.7 MB (a main session). Oldest observed: ~9 months. Retention is real — dirs already exist whose sessions were pruned.

## Session identity and provenance

- **Main session**: native id = file basename UUID. Every record's `sessionId` field equals the basename (verified across all 49 files: zero foreign `sessionId`s). Also stamped on records: `version` (Claude Code version), `gitBranch`, `cwd`, `userType` (only `"external"` observed), `entrypoint` (`cli`, `sdk-ts`, `claude-desktop-3p` observed).
- **Subagent session**: use the project-relative native path without `.jsonl`, retaining the parent Session and each `subagents/` segment. For example, `<parent-id>/subagents/agent-example` remains distinct from the same child basename under another parent.
- Records in subagent files carry `agentId` (the 17-hex id, no `agent-` prefix) and `isSidechain: true`; main-session records have `isSidechain: false` (no embedded sidechains observed in current files — sidechains are separate files now).

## Growth model

**Append-only.** Verified live: a growing session's byte-prefix hash is stable across appends (no rewrite, no reorder). Consequences:

- Capture extension = re-compress the whole file (or append raw tail); byte offset / line count is a valid growth bookmark per file, mtime+size is a valid change detector.
- Control records (`last-prompt`, `ai-title`, `mode`, `atis-latch`, `permission-mode`, `queue-operation`) are re-appended after turns, so they appear interleaved throughout the file, not just at the end. **Last occurrence wins** for title-like fields.
- A session file's *first* line is never a message — observed first-record types across all main files: `mode` (20), `queue-operation` (22), `ai-title` (7). Never assume line 1 has `cwd` or `timestamp`; scan for the first record that does.
- Timestamps are *mostly* monotonic per file; occasional out-of-order records exist (6 of 182 in one live file). Sort by timestamp only where the SPEC requires interleaving; otherwise preserve file order.
- `/compact` does **not** start a new file: a `system/compact_boundary` record (with `compactMetadata.trigger`, `preTokens`, `logicalParentUuid`, `preservedSegment`) is followed by an injected `user` record with `isCompactSummary: true` — same file, same session id. A context-exhaustion *continuation into a new session file* also exists; its first user message starts with `"This session is being continued from a previous conversation"` and is likewise `isCompactSummary: true` in current versions.

## Record types (observed census, 13,567 records)

Every record is a JSON object with a `type`. Message-bearing records additionally have `uuid`, `parentUuid` (record DAG, `null` at roots and after compaction), `timestamp` (ISO 8601 UTC with ms, e.g. `2026-08-20T21:45:25.497Z`), and the provenance stamps listed above.

| type | count | role in extraction |
|---|---|---|
| `assistant` | 6,057 | **Message source.** `message` = full API message: `role:"assistant"`, `content` blocks, `model`, `id`, `usage`, `stop_reason`. Extra: `requestId`, `effort` (`high`/`medium`), `slug` (human-readable session nickname, e.g. `parallel-shimmying-torvalds`), `attributionSkill`/`attributionAgent`/`attributionMcpServer`. |
| `user` | 3,868 | **Message source.** `message` = `{role:"user", content: string \| block[]}`. Extra: `promptId`, `toolUseResult` (see subagents), `sourceToolAssistantUUID`, `isMeta`, `isCompactSummary`, `isVisibleInTranscriptOnly`, `origin` (`{kind:"human"}` / `{kind:"task-notification"}`), `promptSource` (`typed`/`sdk`/`system`), `toolDenialKind`, `mcpMeta`. |
| `attachment` | 1,017 | Injected context envelope; `attachment.type` ∈ `task_reminder`, `edited_text_file`, `deferred_tools_delta`, `total_tokens_reminder`, `skill_listing`, `agent_listing_delta`, `queued_command`, `file`, `command_permissions`, `compact_file_reference`, `plan_mode_exit`, `read_truncation_notice`. **Not dialog** (but see `queued_command` quirk). |
| `last-prompt` | 803 | `{lastPrompt, leafUuid, sessionId}` — appended each turn, no timestamp. |
| `ai-title` | 632 | `{aiTitle, sessionId}` — model-generated title, appended (and re-appended) as it evolves. **Best title source; last one wins.** |
| `mode` | 535 | `{mode:"normal"\|…}` state echo. |
| `queue-operation` | 312 | `{operation: enqueue\|dequeue\|remove, timestamp}` — prompt queue bookkeeping; carries no prompt text. |
| `pr-link` | 159 | `{prNumber, prUrl, prRepository, timestamp}`. |
| `system` | 75 | `subtype` ∈ `local_command` (34; slash-command echo in `content`), `turn_duration` (17), `api_error` (14; `error.message`, `retryAttempt`), `away_summary` (4; readable recap in `content`), `compact_boundary` (3), `stop_hook_summary`, `informational`, `model_refusal_no_fallback`. **Not dialog.** |
| `atis-latch` | 34 | opaque state latch. |
| `permission-mode` | 31 | `{permissionMode:"default"\|…}`. |
| `file-history-snapshot` / `file-history-delta` | 26 / 3 | checkpointing backups (`trackedFileBackups`, backup file names). |
| `started` | 12 | **journal.jsonl only** (workflow dirs): `{key:"v2:<sha256>", agentId}`. |
| `custom-title` | 3 | `{customTitle, sessionId}` — user-set title; **overrides `ai-title`.** |

Historical: `type:"summary"` records with `{summary, leafUuid}` — superseded by `ai-title`/`last-prompt`. Absorb unknown types silently; the census above will be stale within months.

## Message extraction

`type:"user"` and `type:"assistant"` records are the ordinary Conversation Message sources. A user-authored `type:"attachment"` / `attachment.type:"queued_command"` with `attachment.commandMode:"prompt"` is also a user Message source; see "Queued user prompts" below. Role for ordinary records = `message.role`, falling back to record `type`. Timestamp = record `timestamp` (ISO → epoch).

### Content shapes

`message.content` is either a **string** (plain user prompt; 71 observed) or an **array of blocks**:

- `{"type":"text","text":"..."}` — the payload. Concatenate all text blocks with `\n\n`.
- `{"type":"thinking","thinking":"...","signature":"..."}` — assistant reasoning (1,490 observed). Exclude it from Conversations (not user-facing dialog; recoverable from Capture).
- `{"type":"tool_use","id","name","input"}` — Action, excluded (but mined for subagent links, below).
- `{"type":"tool_result","tool_use_id","content"}` — Action result inside a `user` record, excluded. A user record whose content is *only* tool_result blocks extracts to empty text and is dropped.
- `{"type":"image","source":{"type":"base64","media_type","data"}}` — user-pasted image; no text. v2: render a placeholder marker or drop; never inline the base64.

There are no rich-text trees (that's Cursor/Lexical); Claude text blocks are flat strings containing markdown.

### Queued user prompts

Claude records a prompt submitted while an assistant turn is still running as an attachment rather than an ordinary `type:"user"` record:

```json
{"type":"attachment","uuid":"queued-record-uuid","timestamp":"2026-08-27T02:41:44.345Z",
 "entrypoint":"sdk-ts","sessionId":"session-uuid",
 "attachment":{"type":"queued_command","commandMode":"prompt",
   "timestamp":"2026-08-27T02:41:44.345Z",
   "prompt":[{"type":"text","text":"there have been no code changes since the review"}]}}
```

This is real user Dialog. Extract `attachment.prompt` with the same string-or-text-block rules as ordinary user content, at the attachment record's native position and timestamp. Take any pending Action seam at this Message boundary. An empty prompt yields no Message.

Do not confuse `commandMode:"prompt"` with `commandMode:"task-notification"`: the latter contains machine-generated `<task-notification>` plumbing and remains noise. Do not text-deduplicate queued prompts against later user records. The 2026-08-28 census found that SDK-launched (`entrypoint:"sdk-ts"`) queued prompts often have **no** later ordinary user record; text equality alone cannot prove that a later identical prompt is a replay rather than the user deliberately repeating it. Suppress a duplicate only if a future native format supplies an exact record-identity/correlation join and that join is documented here first.

Queued prompt attachments can be the sole native copies of user input. Synthetic regression fixtures prove that extracting those attachments restores the missing Dialog without depending on an Orchestrator copy.

### Noise filtering (records that are `user`/`assistant` but not Dialog)

Filter these machine records from Conversation Dialog:

1. `isMeta: true` user records — CLI-injected context (e.g. `<local-command-caveat>Caveat: The messages below were generated by the user while running local commands…`, injected skill/doctor prompts). Exclude.
2. `isCompactSummary: true` (usually with `isVisibleInTranscriptOnly: true`) — machine-written compaction summary posing as a user message (`"This session is being continued from a previous conversation…"`). Exclude from user Dialog (optionally render as a system note).
3. User text that is a slash-command echo: starts with `<command-name>` (paired `<command-message>`/`<command-args>`), or `<local-command-stdout>`/`<local-command-stderr>`. Exclude.
4. User text starting with `<task-notification>` (background-task completion ping; also flagged `origin.kind == "task-notification"`) and `<teammate-message>`-style envelopes. Exclude from Dialog.
5. `[Request interrupted by user]` / `[Request interrupted by user for tool use]` synthetic user texts. Exclude.
6. Assistant records with `message.model == "<synthetic>"` or `isApiErrorMessage: true` — CLI-fabricated errors (`API Error: 401 …`, `Not logged in · Please run /login`, `No response requested.`). Exclude.
7. Assistant `error` / `apiErrorStatus` fields accompany (6); the `<synthetic>` model check covers all observed cases.

`<system-reminder>` fragments appear *inside* otherwise-real user prompts (harness-injected suffixes); v2 policy decision: keep the message, do not attempt intra-message surgery at extract time (Capture keeps everything regardless).

### Titles

Preference order: `custom-title` (user-set) → last `ai-title`. Do not derive a fallback from the first user message: that text is already present in the Conversation body, so repeating it as metadata adds no information. `last-prompt` is the *latest* prompt, not a title.

## Subagent linkage

Resolve exact parent/child references in both directions.

### v2 resolution policy

Extraction persists both sides independently per Host. A main Conversation records a parent Reference from the spawning tool-use/result join but does not guess a child path. A subagent Conversation records its exact native id, its immediate parent (the prefix before the last `/subagents/`), the leaf `agentId`, and native meta fields. Re-ingesting one settled Host Capture always replaces that Host's extracted projection and facts before winner election. The canonical Session's deterministic Projection winner supplies its child Parent fact, and the canonical parent Session's winner supplies candidate References; resolution never merges facts from stale loser Hosts. During migration only, a Session without a populated winner may use all existing anonymous or Host-named facts under the normal deterministic richness and ambiguity rules; winner population switches to strict winner-Host filtering, and named refresh deletes obsolete anonymous facts for that identity. Resolution matches at the first nonempty tier: exact child native id, then `toolUseId`, then parent-scoped `agentId`. Identical facts deduplicate before ambiguity is assessed; an ambiguous tier does not enrich the authoritative child-derived link. Thus workflow paths and future nesting layers never become path-construction rules. Child user-role Messages remain canonical `user` Messages in the database but render as `<parent>`; children render for recovery and are excluded from Project/Loose indexes. Names and types copied into generated link prose have physical newlines escaped so native metadata cannot synthesize Transcript delimiters.

**Parent side** — two coordinated signals:

1. Assistant `tool_use` block with `name` ∈ {`Task`, `Agent`}. `input.description` → subagent name, `input.subagent_type` → role, `input.prompt` → the delegated instructions. Keyed by block `id` (`toolu_…`).
2. A later `user` record with top-level `toolUseResult.agentId` — the authoritative child id. Current async form: `toolUseResult = {agentId, isAsync: true, status: "async_launched", description, prompt, resolvedModel, outputFile, canReadOutputFile}`. The record's first content block is a `tool_result` whose `tool_use_id` joins back to signal (1) for name/role. Child file path = `<parent-session-dir>/subagents/agent-<agentId>.jsonl` — **but workflow children live one layer deeper** (`subagents/workflows/wf_*/agent-<agentId>.jsonl`), so resolve by `agentId` match against enumerated children, not by constructing the path.

**Child side** — self-describing:

- Path gives the parent chain: everything before the last `subagents` segment is the parent's native id.
- `agent-<agentId>.meta.json`: `{"agentType":"Explore","description":"Map old agent-sessions example module","toolUseId":"toolu_01LF…","spawnDepth":1}` (reads `description`→name, `agentType`→role; `toolUseId` and `spawnDepth` are new — `toolUseId` joins directly to the parent's tool_use block; workflow children have only `{"agentType":"workflow-subagent","spawnDepth":1}`).
- Records inside carry `isSidechain: true` and `agentId`.
- The child's first record is typically the delegated prompt as a `user` message — render it as `<parent>` per SPEC, not as user Dialog.
- Depth: count of `/subagents/` segments in the native id, or `spawnDepth` from meta.json.

## Timestamps

- Record `timestamp`: ISO 8601 UTC with milliseconds.
- `sessions-index.json` `fileMtime` and `~/.claude/history.jsonl` `timestamp`: **epoch milliseconds** (numbers).
- Control records (`ai-title`, `mode`, `last-prompt`, `atis-latch`, `permission-mode`, `custom-title`) have **no timestamp**.
- Session created/updated: first/last record timestamp, with file mtime as fallback/upper bound.

## Sanitized raw examples

Main-session user prompt (string content):

```json
{"parentUuid":null,"isSidechain":false,"promptId":"example-id-…","type":"user",
 "message":{"role":"user","content":"hi"},
 "uuid":"example-id-…","timestamp":"2026-08-20T21:45:20.051Z","userType":"external",
 "entrypoint":"cli","cwd":"/workspace/Projects/example","sessionId":"example-id-…",
 "version":"2.1.238","gitBranch":"main","slug":"parallel-shimmying-torvalds"}
```

Assistant record (message trimmed):

```json
{"type":"assistant","requestId":"req_…","effort":"high",
 "message":{"id":"msg_…","type":"message","role":"assistant","model":"claude-opus-5-…",
   "content":[{"type":"thinking","thinking":"…","signature":"…"},
              {"type":"text","text":"Here is the plan…"},
              {"type":"tool_use","id":"toolu_01LF…","name":"Agent",
               "input":{"description":"Map old codebase","subagent_type":"Explore","prompt":"Explore …"}}],
   "stop_reason":null,"usage":{"input_tokens":4,"output_tokens":220,"…":"…"}},
 "uuid":"…","parentUuid":"…","timestamp":"2026-08-20T21:46:01.000Z","sessionId":"…","…":"…"}
```

Subagent spawn result (parent side):

```json
{"type":"user","sourceToolAssistantUUID":"…",
 "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01LF…","content":"Async agent launched…"}]},
 "toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a9f0b4938cd08dede",
   "description":"Map old codebase","prompt":"Explore …","resolvedModel":"claude-opus-5[1m]",
   "outputFile":"/private/tmp/claude-…/tasks/….output","canReadOutputFile":true},
 "timestamp":"…","sessionId":"…"}
```

Compaction pair (same file):

```json
{"type":"system","subtype":"compact_boundary","parentUuid":null,"logicalParentUuid":"example-id-…",
 "content":"Conversation compacted","compactMetadata":{"trigger":"auto","preTokens":168536,
 "preservedSegment":{"headUuid":"…","anchorUuid":"…","tailUuid":"…"}},"timestamp":"…","uuid":"…"}
{"type":"user","isCompactSummary":true,"isVisibleInTranscriptOnly":true,
 "message":{"role":"user","content":"This session is being continued from a previous conversation…"},"…":"…"}
```

Subagent sidecar `agent-a9f0b4938cd08dede.meta.json`:

```json
{"agentType":"Explore","description":"Map old example module","toolUseId":"toolu_01LF…","spawnDepth":1}
```

## Retention

Claude Code deletes session transcripts whose last activity is older than `cleanupPeriodDays` (default 30) from `~/.claude/projects`, leaving empty per-session directories behind. This is the reason the archive exists, and it bit once: the 2026-09-04 title write-back dry-run found eight sessions from 2026-07-28 to 2026-08-04 whose JSONL files were already gone; the archive holds the only copies. Every Host that is pulled must raise the setting before its first pull:

```json
{ "cleanupPeriodDays": 36500 }
```

in the user-level `~/.claude/settings.json` (a project `settings.local.json` would cover one project only). `pull` warns once per Host and Source when a Claude Store's retention is at most 30 days (including zero); longer windows are silent at WARN; `[pull] warn-store-retention = false` silences it. Collection reads `settings.json` beside the configured projects Store root; a missing file or key means 30 days. A malformed file or non-integer/negative value is an explicit Source error. Export carries the observed days, setting name, and Host-local settings path in its manifest, so remote warnings name the setting on the correct Host. This is observational only: collection never edits settings or deletes native Sessions.

## Quirks

1. **Project dir names are irreversibly lossy** (`_`, `.`, space → `-`). Use the native `cwd` field rather than reversing the directory encoding.
2. **Session files never start with a message** — first line is `mode`/`queue-operation`/`ai-title`. Header-sniffing line 1 misidentifies the format.
3. **Control records interleave** throughout the file (re-appended per turn) and lack timestamps; last-wins for `ai-title`.
4. **`sessions-index.json` lies by omission**: present in a minority of projects, can be months stale, and can outlive its sessions (observed a project dir with only the index left).
5. **The store self-prunes** — sessions vanish (default retention finite); orphaned per-session directories (only `.DS_Store` inside) linger.
6. **Workflow subagents** add a `subagents/workflows/wf_<id>/` layer with their own `journal.jsonl` and minimal meta.json — path construction from `agentId` alone finds nothing; resolve children by enumeration.
7. **`toolUseResult` is now async** (`status:"async_launched"`, `outputFile`) — the child may still be empty/running at capture time; a later ingest run must extend it (mtime gate handles this).
8. **`<synthetic>` assistant messages** (login errors, `No response requested.`) are machine-generated; filter on `message.model == "<synthetic>"`.
9. **Compaction fabricates a user message** (`isCompactSummary`) and breaks the `parentUuid` chain (`parentUuid: null`, real link in `logicalParentUuid`).
10. **Empty `thinking` blocks** occur (`{"thinking":"","signature":"…"}`).
11. Records occasionally arrive **out of timestamp order** within a file.
12. Both `sessionId` and legacy `session_id` keys exist (203 records carry snake_case, on user/assistant/attachment) — read camelCase first.
13. Sizes: main sessions reach 22.7 MB here (fleet-wide Claude is mild vs Codex's 76 MB, but stream anyway).
14. `queued_command` is overloaded. `commandMode:"prompt"` contains real user Dialog and may be the **only** native record of that prompt, especially under `entrypoint:"sdk-ts"`; extract it at the attachment's position. `commandMode:"task-notification"` is machine plumbing and remains excluded. Never assume queue drain creates a later ordinary user record, and never use text equality as a deduplication join.
15. Tool names that spawn subagents have already churned (`Task` → `Agent`, plus `Workflow`); match the `toolUseResult.agentId` join, not the tool name, wherever possible.

## Capture recommendation

Per ADR-0001, the Capture is the verbatim native payload, one file per Session:

- **Main session**: the raw bytes of `<encoded-cwd>/<session-uuid>.jsonl`, zstd-compressed. Native id = `<session-uuid>`.
- **Subagent session**: use the project-relative native path without `.jsonl`, retaining the parent Session and each `subagents/` segment. For example, `<parent-id>/subagents/agent-example` remains distinct from the same child basename under another parent.
- **Not captured as Sessions**: `sessions-index.json`, `journal.jsonl`, `workflows/scripts/*`, `memory/*`, `.DS_Store`, `~/.claude/history.jsonl`, `~/.claude/tasks/*`. (`journal.jsonl` and scripts are cheap and orphan-prone; if ever wanted, they'd be Capture sidecar extras, not Sessions.)
- **Growth**: files are append-only, so "grew" = (mtime, size) changed; re-capture the whole file (tens of MB max — trivial) and supersede the previous Capture for the same (Source, native id). No offset bookkeeping needed for correctness; store the source file's mtime+size in the sidecar as the per-session Bookmark input.
- **Provenance sidecar** must record: native absolute path, encoded project dir name, `cwd` (first observed), Host, source mtime and size at capture, captured-at, adapter version, and the mtime + size revision of an associated native `.meta.json`. The metadata revision participates in Enumerate and Bookmarks independently from the JSONL body. A metadata-only change rewrites only this sidecar (including refreshed verbatim `nativeMeta`), never the compressed body; sidecars written before these optional revision fields self-upgrade compatibly.
