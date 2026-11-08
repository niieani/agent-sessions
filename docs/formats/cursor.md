# Cursor native format

Reference for implementing the Cursor Adapter (`src/adapters/cursor.rs`, PLAN slice 6). Terms per [CONTEXT.md](../../CONTEXT.md). Sources of knowledge:

- Format behavior is pinned by the Rust Adapter and synthetic fixtures.

## 1. Store layout

```
~/Library/Application Support/Cursor/User/globalStorage/
  state.vscdb              # the Store: one SQLite DB, WAL mode
  state.vscdb-wal          # live WAL (actively written while Cursor runs)
  state.vscdb-shm
  state.vscdb.backup       # occasional vendor-made copy (older snapshot; not part of the Store)
```

Schema (verbatim from `sqlite_master`):

```sql
CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)
CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)
CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, workspaceId TEXT,
  createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER,
  recency INTEGER, checkpointAt INTEGER, value TEXT)
```

Everything conversation-related lives in `cursorDiskKV`. Key namespaces observed (2026-08 census):

| key prefix | rows | meaning |
|---|---|---|
| `agentKv:blob:<sha256>` | 130,723 | content-addressed blob store of the `cursor-agent` backend (model-visible messages + binary protobuf tree nodes) — see §10 |
| `bubbleId:<composerId>:<bubbleId>` | 106,901 | one message/event blob (JSON) |
| `checkpointId:<composerId>:<checkpointId>` | 4,504 | file-state checkpoints (undo) |
| `codeBlockDiff:<composerId>:<uuid>` | 2,186 | diff payloads |
| `codeBlockPartialInlineDiffFates:*` | 1,876 | diff bookkeeping |
| `ofsContent:<composerId>:file://<uri>` | 902 | original file snapshots |
| `messageRequestContext:<composerId>:<bubbleId>` | 852 | per-request injected context (open files, lints, rules) |
| `composerData:<composerId>` | 679 | one conversation (Session) header+state blob (JSON) |
| `composerVirtualRowHeights`, `patch-graph`, `cursorBlame`, `inlineDiff`, `expectedContent-*` | ~200 | UI/edit bookkeeping |

`composerHeaders` (560 rows) is a newer denormalized index of composers — **incomplete**: 127 `composerData` rows have no `composerHeaders` row (spanning 2025-11 through 2026-08, not just old ones), and 8 header rows have no `composerData`. Do not treat it as the enumeration source of truth; it is useful as a fast pre-filter and for `isSubagent`/`workspaceId` at a glance.

`ItemTable` holds only UI state for Cursor chats (`composer.composerHeaders*` gates, `workbench.panel.composerChatViewPane.<id>.hidden`, …) — no conversation content. (VS Code proper stores chats in `ItemTable` of *workspace* DBs; that is the separate VS Code Adapter's problem.)

Some historical `bubbleId:*` rows have SQL `NULL` values. These are stale/deleted tombstones, not empty JSON documents; skip them before JSON parsing. Continue diagnosing empty `TEXT`/`BLOB` values as malformed because their declared storage type claims content.

## 2. Read-only access — hard rules

- The DB is in WAL mode (`PRAGMA journal_mode` → `wal`) and is actively written while Cursor runs. Open with `mode=ro` (rusqlite `OpenFlags::SQLITE_OPEN_READ_ONLY`). Do not use `immutable=1` while the WAL has content — that skips WAL and would hide live writes. An idle catalog (WAL absent or empty) may retry `immutable=1` so read-only mode can proceed without creating `-shm`.
- A read-only open still needs the `-wal`/`-shm` files readable (they are). CLI probing: `sqlite3 -readonly`.
- Open this database read-only; never take write locks on the live Store.
- Never dump whole tables. Max single values seen: bubble 3.7 MB, composerData 1.6 MB — per-row streaming is fine; unbounded `SELECT value` over all 130k `agentKv` rows is not.

## 3. Session identity & provenance

- **Native id** = the complete `composerId` key. Treat it as opaque: vendor-defined non-UUID IDs are valid. Archive filenames use bounded encoded stems while Provenance preserves the raw identity. Never interpret a native ID as an Archive path.
- Native location = the `state.vscdb` path.
- `composerData:<id>` is the root record; all other per-session rows embed the composerId in their key.

## 4. Enumeration (cheap → expensive)

1. **Local metadata projection**: use an indexed key-range query over composers (679 rows, not 130k) that returns only id, `COALESCE(lastUpdatedAt,createdAt)`, and `length(value)`. Compare those tiny logical metadata values with Capture sidecars; transfer full composer JSON into Rust only for mismatches, unrevisioned roots, or missing bodies/sidecars. This is both cheap and safe under Host/vendor clock skew—filesystem mtime and logical timestamps are deliberately not compared. **`LIKE 'composerData:%'` does not use the index range**; use `GLOB` or `key BETWEEN 'composerData:' AND 'composerData;'`, which SQLite plans as `SEARCH... (key>? AND key<?)`.
2. **Warm no-op**: the metadata projection plus sidecar comparison returns no SessionRefs when everything matches. A missing Capture body is excluded from the gate's sidecar census, so its root is point-loaded and repaired even below the Bookmark.
3. **Stateless-export Since filter in SQL**: numeric `lastUpdatedAt`, else numeric `createdAt`, must exceed `:since_ms`. A mistyped timestamp (including strings), a valid root with neither field, and a malformed root remain candidates rather than being cast to zero and vanishing forever. Because a stateless remote has no central header signature to compare, every composer with a `composerHeaders` row is conservatively re-covered; central idempotency absorbs unchanged Captures. The filtered root query took 0.42 s over all 679 composers on the sampled 3.5 GB DB. Both timestamp fields are epoch **milliseconds**.
4. Only for composers that pass the filter, load bubbles (§6).

Do not enumerate from `composerHeaders` (incomplete, §1) — but `SELECT lastUpdatedAt FROM composerHeaders` is a legitimate extra cheap pre-check if wanted.

Require `value IS NOT NULL`; zero NULL rows exist today but keep the guard.

## 5. How a live Session grows on disk

- **New messages** → new `bubbleId:<composer>:<bubbleId>` rows are **inserted** (append-like; `key UNIQUE ON CONFLICT REPLACE`). Bubble rows for a session accumulate monotonically.
- **The `composerData:` row is rewritten in place** on every update (whole JSON blob replaced; `lastUpdatedAt` and `fullConversationHeadersOnly` advance). Observed live: two reads minutes apart returned different `conversationCheckpointLastUpdatedAt` in the same row.
- **Wholesale conversation rewrites happen.** Observed: a session (backend `cursor-agent`, `isNAL: true`) had 25,774 bubble rows but only 3,754 entries in `fullConversationHeadersOnly`; the header list had been regenerated with **fresh bubbleIds**, duplicating earlier content under new ids, and stamping the rewritten bubbles with the rewrite time (all early bubbles `createdAt` ≈ the same instant, 2 days after the composer's `createdAt`). The superseded originals remain as orphan rows with their true timestamps. Consequences:
  - Membership and order of the Conversation come **only** from `fullConversationHeadersOnly`; never reconstruct from a `bubbleId:%` key scan when headers exist.
  - Early-message timestamps in a rewritten session are unreliable (see §8).
  - A grown/rewritten session is the "richer capture supersedes" case of ADR-0004: same identity, re-capture everything.

## 6. Bubble loading (conversation body)

`composerData` fields that carry the conversation, in v2 precedence order (the current Store's authoritative membership wins over a stale legacy copy):

1. Non-empty `fullConversationHeadersOnly`: array of `{bubbleId, type, serverBubbleId?, createdAt?, grouping?, contentHeightHint?}` — authoritative membership and ordering. 527/679 composers have one. A simultaneous legacy inline array is ignored.
2. Non-empty `conversation`: legacy inline array of full bubble objects. Zero of 679 composers on the current Store use it, but old Stores do.
3. Neither present / empty: scan `bubbleId:<composerId>:*`, then stable-sort by bubble timestamp (timestamped first, deterministic bubble-id tie break, missing timestamps last). This is a last-resort legacy recovery path, never UUID-key conversation order.

A missing bubble row for a listed header is possible — skip, but count for diagnostics.

`conversationMap` exists on current composerData (`_v` 17) but was empty (`{}`) on all 679 — watch it; it may become the inline store again.

## 7. Records → Messages

### Bubble object (`_v: 3` current)

Sanitized user bubble (structure-complete, values trimmed; ~60 always-present bookkeeping arrays elided):

```json
{
  "_v": 3,
  "type": 1,
  "bubbleId": "example-id-…",
  "serverBubbleId": "example-id-…",
  "createdAt": "2026-08-20T05:32:23.420Z",
  "text": "do you have access to https://…/EXAMPLE-123?",
  "richText": "{\"type\":\"doc\",\"content\":[{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"do you have access…\"}]}]}",
  "tokenCount": {"inputTokens": 0, "outputTokens": 0},
  "unifiedMode": 2,
  "conversationTurnIndex": 0,
  "isAgentic": false,
  "context": { "fileSelections": [], "folderSelections": [], "…": "…" },
  "toolResults": [], "lints": [], "images": [], "…60 more mostly-empty arrays": []
}
```

Assistant bubbles additionally carry (each on its own bubble — Cursor emits one bubble per step):

```json
{ "type": 2, "text": "Let me check the Atlassian integration…", "turnDurationMs": 2476 }
{ "type": 2, "text": "", "thinking": {"text": "The user is asking…", "signature": ""}, "capabilityType": 30 }
{ "type": 2, "text": "", "capabilityType": 15,
  "toolFormerData": {"toolCallId": "GetMcpTools_0_…", "status": "completed", "name": "get_mcp_tools",
                      "rawArgs": "{…}", "params": "{…}", "result": "{…}", "tool": 63} }
```

### Mapping rules

- `type: 1` → user Message; `type: 2` → assistant Message; any other type → skip. A 15k-row sample contained only types 1 and 2 (554 / 14,446 — assistant bubbles dominate ~26:1 because every thinking/tool step is its own bubble).
- **Text extraction chain**: `text` (non-empty) → `richText` → `markdown` (legacy field) → drop. Coverage today: 896/899 sampled user bubbles have non-empty `text`; the 3 `richText`-only ones were empty Lexical paragraph shells (correctly dropped).
- **`richText` has TWO formats** — both appear on the current store:
  - **Lexical** (older bubbles + composer-level `richText`): `{"root":{"children":[…,{"type":"text","text":"…"}]}}`
  - **TipTap/ProseMirror** (bubbles rewritten by the `cursor-agent` backend): `{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"…"}]}]}`
  - The walker must collect every node with `type == "text"` while descending `root`, `children` **and** `content`. Join with a space, trim.
  - `richText` that doesn't start with `{` is treated as plain text.
- Bubbles with empty extracted text (thinking-only, `toolFormerData`-only, `isRenderable: false`) produce no Message — they are Actions/noise (SPEC tiers 4–6), recoverable from the Capture. Do **not** extract `thinking.text` as assistant content.
- **Dialog** flag: at least one non-empty user or assistant Message. 152+ composers are empty drafts; archive them, flag no-Dialog.
- **Title**: `composerData.name` when non-empty. Auto-generated names exist (`"EXAMPLE-123 ticket access"`); empty drafts remain untitled. Do not repeat the first user Message as a fallback because it is already present in the body.

## 8. Timestamps

| field | unit | notes |
|---|---|---|
| `composerData.createdAt`, `.lastUpdatedAt` | epoch **ms** (number) | authoritative session times |
| bubble `createdAt` | ISO-8601 string with ms, `Z` | current format (`_v` 3) |
| bubble `createdAt` (older) | epoch ms number | also support `updatedAt` fallback: ISO → ms → `updatedAt` ISO → ms; keep the chain |
| header entry `createdAt` (in `fullConversationHeadersOnly`) | ISO-8601 | **unreliable** — clobbered to rewrite time on conversation rewrites (§5); prefer the bubble row's own value, same caveat |
| `composerHeaders.createdAt/lastUpdatedAt/recency/checkpointAt` | epoch ms | mirror of composerData |

**Quirk**: after a backend rewrite, both the header entry **and** the bubble row of pre-rewrite messages carry the rewrite time, not the original send time. Original times survive only in orphaned superseded bubbles. Extraction should trust `composerData.createdAt` for session start even when the first bubble claims later.

## 9. Project path derivation

Project-path preference order:

1. **`composerData.workspaceIdentifier.configPath.path`** (present on current-era composers; also in `composerHeaders.value` JSON): the workspace folder or `.code-workspace` file. Strip `.code-workspace` to name the project. Cursor's auto-generated multi-root workspaces live under `…/Cursor/glassMultiRootWorkspaces/<name>.code-workspace` — the path names the combination, not a repo; prefer `trackedGitRepos[].repoPath` (list of real repo paths + branches with `lastInteractionAt` ms) when present.
2. For older composers: `context.folderSelections[0]` (string, or object with `path`/`uri.path`/`uri.fsPath`) → each bubble's `context.folderSelections`/`context.fileSelections` → session `context.fileSelections`; file paths are walked up to a project root by marker files (`.git`, `package.json`, `pyproject.toml`, `go.mod`, …). Note: root inference consults the **local** filesystem — only valid on the Host where the Store lives, so it must happen at capture/enumerate time on the remote, or be recorded as raw paths in the Capture (they are, verbatim) and inferred best-effort at extract.
3. `composerHeaders.workspaceId` is an opaque VS Code workspace hash; the literal string `"empty-window"` means no workspace — treat as no project.

Edited-file provenance: keys of `originalFileStates` (file:// URIs), `newlyCreatedFiles[]`, and `context.fileSelections[].uri.fsPath` — v2 may or may not keep editedFiles as a feature, but the fields are in the Capture regardless.

## 10. Noise / injected context

Good news vs Codex: **bubble `text` is the verbatim user input** — Cursor stores injected context *outside* the conversation body, so no `<environment_context>`-style stripping is needed at extract time:

- `messageRequestContext:<composerId>:<bubbleId>` rows hold what was injected for a request (open files, linter errors, rules, `currentFileLocationData`, …).
- The `cursor-agent` backend's model-visible messages live in `agentKv:blob:<sha256>` — content-addressed, **not enumerable per session** (keys are bare SHA-256; the linkage is inside `composerData.conversationState`, a base64 protobuf of blob hashes). ~35% are plaintext JSON messages in AI-SDK shape (`{"role":"user","content":[{"type":"text","text":"<open_and_recently_viewed_files>…"}]}`, roles `system|user|assistant|tool`, parts `text|reasoning|tool-result`); ~65% are binary protobuf tree nodes (start `0x0A 0x20` + 32-byte child hash). There is where `<open_and_recently_viewed_files>`-style injected context appears — i.e. only in data we deliberately don't extract.
- `@file`/`@folder` mentions the user typed remain part of `text` (they are user input; keep them).

## 11. Subagents

Modern composers have first-class linkage inside `composerData` (mirrored in `composerHeaders.value` / `isSubagent`). The fields occur in observed child roots from `_v` 13 through 18; do not gate this feature on a format-version threshold.

**Evidence update — 2026-08-25:** 125 local Captures are classified subagent; 123 have nonempty `subagentInfo` and every one names a captured `parentComposerId`. 119 pairs are reciprocal through the parent's `subagentComposerIds`; 113 additionally have an exact parent task-result `agentId` join. The sampled child `00000001-1111-4111-8111-111111111111` has 77 authoritative bubbles starting with a child user prompt that byte-matches the parent task's `params.prompt`; this proves an independent child Dialog, not inherited parent history.

**Implementation status (2026-08-25):** phase 10 is implemented. The Adapter projects a child only when `subagentInfo.parentComposerId` is non-empty and its authoritative body produces Dialog; `isSubagent`-only and no-Dialog candidates remain Capture-only. It records exact parent task references plus `subagentComposerIds[]` fallback references, de-duplicates rewritten tasks by `(agentId, toolCallId)`, and records out-of-header task evidence without a render-placement timestamp. The resolver requires a parent-side reference before it renders a Cursor parent's `<subagent>` block; a valid child-side-only fact stays as a recoverable child Transcript.

- **Parent side**: `subagentComposerIds: ["<childComposerId>", …]` (observed len 6). The spawn point is the parent bubble whose `toolFormerData.name == "task_v2"` (`toolCallId` e.g. `Task_1_801d71…`); its `params.prompt` is the parent's instruction to the child, `params.description` the short label, bubble `createdAt` the interleave timestamp. Also `subComposerIds` (best-of-N), `isBestOfNParent`.
- **Child side**: `subagentInfo`:

```json
{"subagentType": 3, "subagentTypeName": "generalPurpose",
 "parentComposerId": "example-id-…", "rootParentConversationId": "example-id-…",
 "toolCallId": "Task_1_801d717d-9f2e16", "toolCallIdHistory": ["Task_1_801d717d-9f2e16"],
 "parentRequestId": "example-id-…", "rootParentRequestId": "example-id-…",
 "conversationLengthAtSpawn": 0, "additionalData": {}}
```

- `subagentTypeName` values observed: `generalPurpose` (67), `explore` (44), plus `browser-use` and `side-chat`; all are ordinary child composers for extraction purposes.
- The strongest join is the parent's `task_v2` bubble: parse `toolFormerData.result` as JSON and match `result.agentId` exactly to the child composer ID. `params` supplies the description, type, and delegated prompt; `toolCallId` matches `subagentInfo.toolCallId`.
- `subagentComposerIds[]` is the exact parent-side fallback when no task result survives. `subComposerIds`/best-of-N are related evidence, not a replacement identity rule.
- Extract a child normally only when `subagentInfo.parentComposerId` is nonempty and the authoritative body yields Dialog. Its first user Message is a Parent Instruction and renders `<parent>`; use `parentComposerId`, `toolCallId`, `subagentTypeName`, the child name, and the delegated prompt to populate child-side facts.
- `isSubagent` alone (including a header-only reclassification) is not enough to fabricate a parent link or child Dialog: preserve Capture-only until `subagentInfo` proves the relationship.
- Parent rewrites can retain duplicate/orphan `task_v2` bubbles. Dedupe references by `(agentId, toolCallId)`. Orphan bubbles are valid link evidence and metadata, but their `createdAt` is not a safe render placement; set `spawned_at_ms` only when the matching task bubble is in the parent's current authoritative header list. Otherwise leave placement absent and let the renderer use its deterministic end boundary.
- A child-side parent fact with a captured parent can render the child for recovery. Parent `<subagent>` interleaving should initially require reciprocal parent evidence, with child-only recovery deliberately visible as such rather than silently claiming a parent action.
- Header-only reclassification participates in Capture freshness. When the elected Capture changes from a standalone composer to a subagent, local and Bundle ingest retain that Capture but retract the old standalone Conversation and its dependent rows. The retraction belongs after Capture/Host projection election, so a non-winning Host copy must not erase the elected projection.
- Cursor child Composers do not materialize duplicated parent Dialog; they need no inherited-context marker.

## 12. Other per-session namespaces (Action-tier data)

Not extracted, but part of understanding the Store: `checkpointId:<composer>:<uuid>` = `{files:[{uri:…}]}` undo checkpoints; `codeBlockDiff:<composer>:<uuid>` = diff payloads; `ofsContent:<composer>:<file-uri>` = original file content snapshots; `composerData.originalFileStates` / `newlyCreatedFiles` = edited-file bookkeeping; `tokenCount` per bubble and `contextTokensUsed`/`promptTokenBreakdown` per composer.

## 13. Quirks list

1. Open the Store read-only. WAL + live writer ⇒ `mode=ro`. `immutable=1` only as an idle-WAL retry after SQLITE_CANTOPEN.
2. `LIKE 'prefix%'` on `cursorDiskKV.key` is a full index scan (case-insensitive LIKE); use `GLOB`/`BETWEEN` for a ranged SEARCH.
3. `composerHeaders` is incomplete both ways (127 composers missing from it; 8 rows with no composerData). Enumerate from `composerData:` keys.
4. Bubble rows outnumber `fullConversationHeadersOnly` up to ~7× on rewritten sessions; orphans are superseded duplicates with *different bubbleIds but same content*. Headers define membership + order.
5. Conversation rewrites clobber `createdAt` of pre-rewrite bubbles to the rewrite instant (session appears to "start" days late). Trust `composerData.createdAt`.
6. `richText` is Lexical (`{"root":…}`) **or** TipTap (`{"type":"doc",…}`); Both shapes require traversal. Walk `root`+`children`+`content`, collect `type=="text"`.
7. Inline `conversation` arrays: extinct on current store (0/679) but older native Stores have them — support all three loading paths of §6.
8. Bubble `createdAt` changed type over time: epoch-ms number (old) → ISO-8601 string (current). Composer-level times are always epoch ms.
9. `composerData._v` observed 10, 11, 13, 14, 16, 17; bubble `_v` 3. Fields appear/disappear across versions — parse defensively, never require optional fields.
10. Assistant thinking (`thinking.text`), `toolFormerData`, and `capabilityType`-only bubbles have empty `text` — they drop out of the Conversation naturally; do not "rescue" them.
11. `workspaceId == "empty-window"` ⇒ no project. Multi-root `glassMultiRootWorkspaces/*.code-workspace` paths are synthetic; prefer `trackedGitRepos[].repoPath`.
12. `composerData` blobs reach 1.6 MB and bubbles 3.7 MB (row-level streaming fine); a single session's Capture (composerData + 25k bubbles) can exceed 100 MB uncompressed — stream rows straight to the zstd writer, never accumulate.
13. Encryption keys sit in composerData (`blobEncryptionKey`, `speculativeSummarizationEncryptionKey`) — they key some `agentKv` binary blobs. They are captured verbatim as part of composerData (acceptable: the Archive is as private as the Store), but never needed for extraction.
14. `state.vscdb.backup` may contain sessions vacuumed from the live DB. Out of scope for ingest; note it exists for manual rescue.

## 14. Capture recommendation

Per ADR-0001 ("for SQLite-backed Sources, the Capture is the per-session blobs, not a database copy") and PLAN's pinned capture format (NDJSON of the session's raw KV rows):

**One Capture per composer** = `captures/<host>/cursor/<capture-stem>.ndjson.zst`, one JSON object per line, each line a verbatim KV row. `<capture-stem>` is the UUID unchanged, `id-<hex>` for short non-UUID ids, or `id-sha256-<digest>` for long ids:

```json
{"table":"cursorDiskKV","key":"composerData:<id>","value":"<raw JSON string, byte-exact after JSON decoding>","valueType":"blob"}
{"table":"composerHeaders","key":"<composerId>","value":"{\"composerId\":{\"value\":…,\"valueType\":\"text\"},\"futureVendorColumn\":{\"value\":…,\"valueType\":\"blob\"}}","valueType":"row"}
{"table":"cursorDiskKV","key":"bubbleId:<id>:<bubbleId>","value":"…","valueType":"text"}
…
```

- **Include** (this is the verbatim Session payload):
  1. the `composerData:<id>` row — always first line;
  2. the `composerHeaders` row if that table and row exist (it carries `isSubagent`/`workspaceId`/`recency` not always derivable elsewhere). Discover columns dynamically and serialize every column with its own SQLite storage class, so schema additions are Captured without an adapter update;
  3. **all** `bubbleId:<id>:*` rows in key order — including orphans not referenced by `fullConversationHeadersOnly` (they hold the only true timestamps after rewrites, and headers may re-reference content later). Values are stored byte-exact as read (`value` is TEXT/BLOB; all observed values are UTF-8 JSON — if a non-UTF-8 value ever appears, base64 it and mark `"encoding":"base64"`).
- **Exclude** (Action-tier, huge, and reconstructible policy per SPEC tiers 4–6 — they are *not* part of the Conversation and would multiply Capture size ~3–10×): `checkpointId:*`, `codeBlockDiff:*`, `codeBlockPartialInlineDiffFates:*`, `ofsContent:*`, `messageRequestContext:*`, `inlineDiff:*`, `cursorBlame:*`, `patch-graph:*`, `composerVirtualRowHeights:*`, and the whole `agentKv:blob:*` space (not per-session enumerable; §10). This is a deliberate, documented deviation from "everything about the session": the bubble rows already contain each tool call's name/args/result via `toolFormerData`, so tiers 4–6 remain recoverable from the Capture.
- `valueType` preserves the SQLite storage class (`text`, `blob`, `integer`, `real`, or `null`; outer `row` denotes the serialized `composerHeaders` column set). A UTF-8 TEXT/BLOB value is a JSON string whose decoded bytes are exact. A non-UTF-8 value is base64 with `"encoding":"base64"`. Failed Capture writes remove their temporary file.
- **Growth/supersede**: identity is (`cursor`, composerId); the sidecar's `sourceMtimeMs` is the maximum available logical revision from composerData and composerHeaders (`lastUpdatedAt`, then `createdAt`). `sourceSize` is a deterministic 64-bit discriminator over root byte length plus every header column name, SQLite storage class, and exact value bytes—not the database file size. Thus `isSubagent: 0→1` and unknown future header-only changes refresh the Capture even without a timestamp bump. A higher revision re-captures the full row set even if a rewritten root is smaller; a lower revision never rolls a newer Capture back. The Capture body is committed atomically before its sidecar.
- **Bookmark** for (Host, cursor) = max available logical composer/header revision (ms) ingested, with valid unrevisioned roots always re-covered (§4).
- Extraction (§6–§9) must run entirely from these captured rows — it needs nothing but `composerData` + bubbles, which is exactly what is captured.
