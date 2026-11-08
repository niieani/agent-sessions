# Pi — native format reference

Reference for implementing the Pi Adapter (`src/adapters/pi.rs`). Terms per [CONTEXT.md](../../CONTEXT.md).

**Knowledge provenance** (this doc was assembled 2026-08-20):

The upstream source is [earendil-works/pi](https://github.com/earendil-works/pi), package `@earendil-works/pi-coding-agent`. Session writing lives in `packages/coding-agent/src/core/session-manager.ts`; `packages/coding-agent/docs/session-format.md` documents the record shapes. References below were checked at commit `5cd93f688aaa`.
Examples are synthetic and follow the upstream session format; they contain no operator data.

---

## 1. Store layout

```
~/.pi/agent/sessions/                      # or $PI_CODING_AGENT_DIR/sessions
  --<encoded-cwd>--/                       # one directory per project cwd
    <encoded-iso-timestamp>_<session-id>.jsonl
```

- **Root**: `$PI_CODING_AGENT_DIR/sessions` when configured, otherwise `~/.pi/agent/sessions`. Upstream also supports a session-directory override; collection roots remain explicit configuration.
- **Project subdirectory**: `--` + resolved cwd with leading `/` stripped and every `/`, `\`, `:` replaced by `-`, + `--` (`session-manager.ts:479`). Example: `/workspace/dev/myapp` → `--workspace-dev-myapp--`. The encoding is **lossy** (a literal `-` in a path is indistinguishable from a separator) — never derive project path from the directory name; use the header's `cwd` field (§8).
- **Filename**: `<ISO timestamp with ':' and '.' replaced by '-'>_<session-id>.jsonl` (`session-manager.ts:952–953`), e.g. `2026-08-20T14-00-00-000Z_0198c9a4-….jsonl`. The timestamp is the session-header timestamp (creation), so filenames sort lexicographically by creation time. The `<session-id>` in the filename equals the header `id`.
- Custom session dirs are possible (`--session-dir`); ignore — we archive the default Store.

### Enumeration strategy (cheapest)

There is no date partitioning (unlike Codex's `YYYY/MM/DD`); partitioning is by project. The store is expected to be small (hundreds of files, KB–MB each). Strategy:

1. `readdir` the sessions root; recurse one level into each `--…--` directory. Follow the two-level shape but tolerate stray `.jsonl` at other depths.
2. Gate on **file mtime > bookmark** before reading any content. Growth is append (§3), so mtime is a reliable change signal.
3. Nothing else is needed — do not parse headers during Enumerate; native id can be taken from the filename suffix at this stage and confirmed against the header at Capture time.

## 2. Session identity & provenance

- **Native id**: the session header's `id` (first line of the file), a **UUIDv7** (`session-manager.ts:208–210`) — time-ordered, globally unique, satisfying ADR-0004. Header `id` and the filename's `_<id>` suffix are written from the same value (`session-manager.ts:953`).
- Programmatic callers may supply a custom id: any string matching `^[A-Za-z0-9]([A-Za-z0-9._-]*[A-Za-z0-9])?$` (`session-manager.ts:212–218`). So treat native id as an opaque string, not a UUID type.
- **Fallback**: if a file has no parseable header (shouldn't happen — upstream refuses to load such files, `session-manager.ts:549–553`), fall back to the filename basename sans `.jsonl`. Keep that fallback but flag the capture.
- **Provenance extras** worth keeping in the sidecar: native path (encodes the project dir), header `version`, header `parentSession` (path of the session this was forked/cloned/branched from — see §7).

## 3. How a live session grows on disk

From the upstream writer (`session-manager.ts:1015–1042`, `_persist`):

- **Deferred creation**: nothing is written to disk until the first **assistant** message lands. Until then all entries buffer in memory; on the first assistant message the whole buffer (header + all prior entries) is written in one shot with `open(…, "wx")` (exclusive create). **Consequence: a Pi session file on disk always contains at least one assistant message** — there are no header-only or user-only files from the normal flow.
- **Append-only afterwards**: each subsequent entry is a single `appendFileSync` of one JSON line (`session-manager.ts:1040`). Bookmark/growth logic can rely on file size + mtime monotonically increasing…
- **…except migration rewrites**: when Pi opens an old session at version < 3, it migrates entries in memory and **rewrites the entire file in place** (`session-manager.ts:917–919`, `_rewriteFile` at `979–989`; migrations at `231–291`: v1→v2 adds `id`/`parentId` to every entry, v2→v3 renames message role `hookMessage` → `custom`). A rewritten file has new bytes throughout with the same path and native id. **Capture must therefore replace, not byte-append** — re-capture the whole file whenever mtime/size changes (a shrink or non-prefix change is normal here, not corruption).
- `/fork`, `/clone`, and branch-extraction (`createBranchedSession`, `session-manager.ts:1413–1480`) create **new files with new session ids** whose content duplicates the parent's entries (re-chained `parentId`s, labels re-appended at the tail) and whose header `parentSession` holds the **absolute path** of the origin file. Expect near-duplicate conversations across files; they are distinct Sessions per ADR-0004.

## 4. File format: JSONL entry types

One JSON object per line. Line 1 is the **session header**; every other line is an entry with `{type, id, parentId, timestamp}` where `id` is an 8-char hex string (`randomUUID().slice(0,8)`, `session-manager.ts:221–228`), `parentId` chains entries into a **tree** (§6), and `timestamp` is an ISO-8601 string. Upstream skips blank/malformed lines when reading (`session-manager.ts:503–511`) — do the same.

| `type` | Purpose | Extract? |
|---|---|---|
| `session` | Header: `version` (absent = v1; current 3), `id`, `timestamp`, `cwd`, optional `parentSession` | provenance only |
| `message` | Wraps an `AgentMessage` in `.message` (roles below) | **yes** — user/assistant |
| `session_info` | `name` — user-set display name (`/name`); latest wins; empty/absent clears | title |
| `model_change` | `provider`, `modelId` | no |
| `thinking_level_change` | `thinkingLevel` | no |
| `compaction` | `summary`, `tokensBefore`, legacy `firstKeptEntryId`, optional `retainedTail` (materialized `AgentMessage[]` checkpoint), `usage`, `details`, `fromHook` | no (Action-tier) |
| `branch_summary` | `fromId`, `summary` — LLM summary of an abandoned branch | no (Action-tier) |
| `custom` | Extension state (`customType`, `data`); never in LLM context | no |
| `custom_message` | Extension-injected context (`customType`, `content`, `display`); IS in LLM context | no — injected context, noise |
| `label` | Bookmark: `targetId`, `label` (undefined clears) | no |

New entry types appear as Pi evolves (Capture keeps them for free; extraction ignores unknown types silently).

### `message` roles (the `AgentMessage` union, per session-format.md)

| role | Shape | Conversation mapping |
|---|---|---|
| `user` | `content`: **string OR** `(text \| image)[]`; `timestamp` (unix **ms**) | **user Message** |
| `assistant` | `content`: `(text \| thinking \| toolCall)[]`; plus `api`, `provider`, `model`, `usage`, `stopReason`, `errorMessage?` | **assistant Message** |
| `toolResult` | `toolCallId`, `toolName`, `content`, `isError`, `details?` | Action — skip |
| `bashExecution` | user-run `!` shell command: `command`, `output`, `exitCode`, `excludeFromContext?` | Action — skip |
| `custom` | extension message (`customType`, `content`, `display`); **was `hookMessage` before session v3** | injected — skip |
| `branchSummary` / `compactionSummary` | in-context projections of the entries above; also appear inside `compaction.retainedTail` | skip |

Content block shapes: `{type:"text", text}`, `{type:"thinking", thinking}`, `{type:"toolCall", id, name, arguments}`, `{type:"image", data(base64), mimeType}`.

## 5. Content extraction rules

Conversation extraction follows upstream message types:

- **user**: if `content` is a string, take it verbatim; if an array, join the `text` of `type=="text"` blocks with `\n\n`; ignore `image` blocks.
- **assistant**: join `text` blocks with `\n\n`; **drop `thinking` blocks** and `toolCall` blocks. An aborted/errored turn can yield zero text blocks → empty message, skip.
- Roles other than user/assistant are skipped entirely for the Conversation. Beware: `stopReason:"aborted"`/`"error"` assistant messages are still persisted — keep whatever text they contain.
- **Title**: latest `session_info.name` if any (upstream shows it in `/resume` instead of the first message). Upstream semantics: latest entry wins **including clears** (`name` empty → unnamed, `session-manager.ts:713–714`). Do not derive a fallback from the first user Message; it would only repeat body content.
- **Dialog flag**: at least one non-empty user or assistant Message; v2 archives them flagged, per SPEC. Note §3 implies Pi files nearly always contain an assistant message, but its text can be empty (pure toolCall turns), so no-Dialog files still occur.

### Noise / injected-context filtering

Pi is much cleaner than Codex — there are **no known injected-context markers inside `user` messages** (no `<environment_context>`/`<skill>` equivalents observed; system prompt is not persisted in the session file). All injection happens via `custom_message` entries / `custom`-role messages, which are excluded wholesale by the role/type rules above. There is no guardian-session equivalent. Re-verify on real fleet samples.

## 6. Tree structure and branching

Since session v2, entries form a **tree** via `id`/`parentId` (in-place branching from `/tree`; `session-format.md` "Tree Structure"). File order is append order (chronological), and the **active conversation is the root→leaf path where leaf = last entry in the file** (`session-manager.ts:958–977`, `_buildIndex` sets `leafId` to the last entry).

Linear iteration interleaves abandoned branches. Extract the active leaf path:

- Default Conversation = the **leaf path** (walk `parentId` chain from the last entry to root, reverse — `session-manager.ts:334–360`), which is what Pi itself would send to the LLM. Orphaned `parentId`s (hand-edited files) → treat entry as a root, per upstream `getTree` (`session-manager.ts:1320–1336`).
- Abandoned branches remain in the Capture and can be surfaced later (they are real dialog the user typed); if that ever matters, a `branch_summary` entry marks each switch-away.
- Do **not** apply compaction logic (`firstKeptEntryId`/`retainedTail`) when extracting — that machinery decides LLM context, not what the human said. Walk the full leaf path. Ignore `retainedTail` contents (duplicates of messages already on the path).

## 7. Subagent linkage

None in core Pi. "subagent" exists only as an **example extension** (`packages/coding-agent/examples/extensions/subagent/`), whose runs surface as ordinary `toolCall`/`toolResult` pairs and possibly `custom` entries — no child session files, parent-child IDs, or cross-file links to resolve. Do not apply the Codex/Cursor `<subagent>` model here.

The only cross-file link is the header's **`parentSession`** (absolute path of the origin file) written by `/fork`, `/clone`, `newSession({parentSession})`, and branch extraction. This is lineage, not a Subagent relationship — do not model it as a subagent link; keep it in provenance (it lets a View say "forked from X"). Note it is a **path** on the origin Host, resolvable to a Session only via that host's captures.

## 8. Timestamps & project path

- **Entry `timestamp`**: ISO-8601 string with milliseconds, UTC.
- **Message `timestamp`** (inside `.message`): unix **milliseconds** number (session-format.md base types). Older sessions may lack it; upstream falls back to the entry timestamp (`session-manager.ts:673–685`). Prefer message ts when present, else entry ts.
- **Session created**: header `timestamp` (also encoded in the filename). **Updated**: max message activity time, falling back to header time, then file mtime.
- **Project path**: header `cwd` verbatim. May be missing/empty on very old  sessions — upstream treats it as `""` (`session-manager.ts:740`). Never reconstruct from the `--…--` directory name (lossy, §1).

## 9. Quirks

1. **Whole-file rewrite on version migration** (§3) — the one exception to append-only; capture logic must tolerate arbitrary byte changes at the same path/id.
2. **File doesn't exist until first assistant reply** (§3) — a user-only session in progress is invisible to Ingest; it appears wholesale once the model responds.
3. **Lossy cwd encoding in directory names**; `-` in real paths is ambiguous. `cwd` field is authoritative.
4. **Fork/clone duplicate content** across files with distinct ids (§3) — dedup is a View concern, not identity.
5. **Session `version` field absent** on ancient files = v1: entries have **no `id`/`parentId`** at all — linear order is the only structure; a compaction may carry `firstKeptEntryIndex` (index, not id). Extraction must handle entries without ids (treat file order as the path).
6. **`hookMessage` role** appears in v2 files (renamed `custom` in v3, `session-manager.ts:259–275`); filter it like `custom`.
7. **Blank/malformed lines are legal** — upstream skips them silently; so must we.
8. **`session_info` with empty name clears the title**; last entry wins.
9. Entry ids are only 8 hex chars — unique within a file, not globally; never use them as archive keys.
10. `assertValidSessionId` permits non-UUID custom ids (§2).
11. Assistant messages carry `usage` (tokens + computed cost) and `provider`/`model` — free metadata if we ever want per-Conversation model/cost stats; recoverable from Capture, don't put in the schema.
12. Header scan limit: upstream refuses headers > 1 MiB (`session-manager.ts:494–498`) — irrelevant to us if we stream, but signals headers can be large (custom metadata fields are legal on the header line).
13. White-label builds (`piConfig.name`) relocate the whole store (e.g. `~/.tau/…`, `TAU_CODING_AGENT_DIR`) — out of scope, but explains sessions that "vanish" if the user renames their install.

## 10. Sanitized examples

Synthetic examples following upstream `session-format.md`; no real session data.

```jsonl
{"type":"session","version":3,"id":"00000003-1111-4111-8111-111111111111","timestamp":"2026-08-20T14:00:00.000Z","cwd":"/workspace/dev/myapp"}
{"type":"message","id":"a1b2c3d4","parentId":null,"timestamp":"2026-08-20T14:00:01.000Z","message":{"role":"user","content":"REDACTED user request","timestamp":1755698401000}}
{"type":"message","id":"b2c3d4e5","parentId":"a1b2c3d4","timestamp":"2026-08-20T14:00:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"REDACTED"},{"type":"text","text":"REDACTED assistant answer"},{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"ls"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":10,"output":20,"cacheRead":0,"cacheWrite":0,"totalTokens":30,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":1755698405000}}
{"type":"message","id":"c3d4e5f6","parentId":"b2c3d4e5","timestamp":"2026-08-20T14:00:06.000Z","message":{"role":"toolResult","toolCallId":"call_1","toolName":"bash","content":[{"type":"text","text":"REDACTED output"}],"isError":false,"timestamp":1755698406000}}
{"type":"session_info","id":"d4e5f6a7","parentId":"c3d4e5f6","timestamp":"2026-08-20T14:00:30.000Z","name":"Redacted session name"}
{"type":"label","id":"e5f6a7b8","parentId":"d4e5f6a7","timestamp":"2026-08-20T14:01:00.000Z","targetId":"a1b2c3d4","label":"checkpoint-1"}
```

Forked-session header:

```json
{"type":"session","version":3,"id":"example-id-…","timestamp":"2026-08-20T15:00:00.000Z","cwd":"/workspace/dev/myapp","parentSession":"/workspace/.pi/agent/sessions/--workspace-dev-myapp--/2026-08-20T14-00-00-000Z_0198c9a4-….jsonl"}
```

Expected Conversation from the first example: one user Message ("REDACTED user request", ts 14:00:01), one assistant Message ("REDACTED assistant answer", ts 14:00:05); title "Redacted session name"; toolCall/toolResult/thinking/label excluded.

## 11. Capture recommendation (per ADR-0001)

- **Capture = the verbatim bytes of the session `.jsonl` file**, whole-file, zstd-compressed: `captures/<host>/pi/<native-id>.jsonl.zst`. Native id = header `id` (fallback: filename basename, flagged).
- **On growth**: re-capture the entire file whenever `(size, mtime)` changed. Do **not** attempt append-delta capture — migration rewrites (§3, Quirk 1) make byte-prefix assumptions unsafe. "Richest capture wins" = larger entry count / newer mtime supersedes.
- **Sidecar** (`<native-id>.meta.json`): native absolute path (preserves the `--…--` directory, i.e. the project association even if `cwd` is missing), file mtime and size at capture, captured-at, adapter version, and header extras (`version`, `parentSession`) for cheap lineage queries without decompressing.
- **Nothing else belongs in the Capture**: the file is self-contained (header + every entry). No auxiliary Pi state (`models.json`, `auth.json`, extension dirs) is part of a Session.
- Read the store strictly read-only, streaming line-by-line (files are small today, but the format allows base64 images inline — a single `message` line can be MBs; upstream itself streams with a 1 MiB buffer, `session-manager.ts:514–546`).
