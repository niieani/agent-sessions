# VS Code (Copilot Chat) — native format reference

Reference for implementing the VS Code Adapter (`src/adapters/vscode.rs`). Terms per [CONTEXT.md](../../CONTEXT.md). This is the gnarliest Source (PLAN slice 7): three storage variants coexist, including a delta-encoded JSONL mutation journal.

The native format is interpreted by the Rust Adapter and covered by synthetic fixtures. Recheck vendor schemas when they change.

---

## 1. Store layout

```
~/Library/Application Support/Code/User/workspaceStorage/            # stable channel
~/Library/Application Support/Code - Insiders/User/workspaceStorage/ # Insiders channel (separate Store, same format)
  <workspace-hash>/                    # md5-style hex dir per workspace (also numeric ids, e.g. ext-dev hosts)
    workspace.json                     # {"folder": "file:///…"} or {"workspace": "file:///….code-workspace"}; may be absent
    state.vscdb                        # SQLite: ItemTable(key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)
    state.vscdb.backup                 # periodic copy — IGNORE (stale duplicate)
    chatSessions/                      # may be absent
      <session-uuid>.json              # variant B: full snapshot, rewritten whole on save
      <session-uuid>.jsonl             # variant C: delta journal (kind 0/1/2), appended on save
    chatEditingSessions/<uuid>/        # edit-plan working state — NOT conversations; ignore

~/Library/Application Support/Code - Insiders/User/globalStorage/
  emptyWindowChatSessions/<uuid>.json  # variant B files for chats in windows with no folder open
```

Three session-storage variants, in historical order (all present in a real Store; none can be dropped):

| Variant | Where | Era observed | Written how |
|---|---|---|---|
| **A: DB row** | `state.vscdb` → `ItemTable` key `interactive.sessions` = JSON array of session objects | sessions up to ~early 2025 | whole array rewritten |
| **B: JSON snapshot** | `chatSessions/<sessionId>.json` = one session object | ~Mar 2025 → late 2025 (+ `emptyWindowChatSessions`) | whole file rewritten per save |
| **C: JSONL journal** | `chatSessions/<sessionId>.jsonl` = kind 0/1/2 mutation log (§4) | ~May 2026 (stable) / Feb 2026 (Insiders) → current | **appended** per save |

All three variants serialize the **same session object schema, `version: 3`** (274/274 sampled files are version 3; variant A rows too). VS Code migrated A→B by copying: in the one workspace holding both, all 13 DB sessions also exist as files with identical `lastMessageDate`, and 10 newer sessions are file-only. **The DB row is a stale subset once `chatSessions/` exists — prefer files, fall back to the DB row for ids not present as files**.

An index of the file store lives in `ItemTable` key `chat.ChatSessionStore.index` (`{version, entries: {<sessionId>: {sessionId, title, lastMessageDate, isImported, initialLocation, isEmpty, timing:{created,lastRequestEnded}, lastResponseState}}}`). Useful as a cheap title/isEmpty lookup, but not authoritative — entry count did not match file count in sampling (31 entries vs 40 files); treat files as truth.

### Enumeration strategy (cheapest)

No date partitioning. Sizes on this host: 274 files / 326 MB total, largest single file 13.3 MB (variant B snapshot). Strategy:

1. `readdir` each configured root's `workspaceStorage/` (~10² dirs), plus Insiders' `globalStorage/emptyWindowChatSessions/`.
2. Per workspace dir, `readdir chatSessions/` if present; gate each `*.json`/`*.jsonl` on **file mtime > bookmark**. Snapshot rewrites and journal appends both bump mtime, so mtime is a complete change signal. Native id = filename stem — no content read needed at Enumerate (confirm against `sessionId` at Capture).
3. `state.vscdb`: gate on the DB file's mtime > bookmark (weak — the DB holds unrelated workbench state and churns constantly, but it is a correct upper bound; on old workspaces that are never reopened it never fires, which is exactly right). When it fires, open **read-only** and run one query:
   `SELECT value FROM ItemTable WHERE key='interactive.sessions'` — most workspaces store the empty array `[]` (2 bytes); a cheap `length(value) > 2` predicate skips them. There is no per-session row to filter server-side (one key holds all sessions), so date filtering happens after JSON parse via each element's `lastMessageDate`.
4. Skip `state.vscdb.backup`, `chatEditingSessions/`, and extension dirs (`GitHub.copilot-chat/` etc.).

Discover workspace directories through `workspace.json`, `state.vscdb`, `chatSessions/*.json` and `chatSessions/*.jsonl`. Include journal-only directories; an absent snapshot does not prove a Session is absent.

> Open native SQLite Stores read-only, with the shared WAL-aware transaction boundary described in SPEC. Never take write locks on live editor databases.

## 2. Session identity & provenance

- **Native id**: the session object's `sessionId` — a UUIDv4 string. Identical to the filename stem for variants B/C. Stable across the A→B migration and across the session's lifetime. ADR-0004 identity = `vscode` + `sessionId`.
- The same `sessionId` never appeared in two different workspace dirs or channels in sampling (272 unique ids across 272 files), but **does** appear in both the DB row and a file (migration). Dedupe per id: file variant wins; if forced to choose between equal ids elsewhere, compare newer `updatedAt`, then more requests, then more total text.
- **Provenance extras** for the sidecar: native path, storage variant (db-row / snapshot / journal), workspace-hash dir name, app channel (Code vs Code - Insiders), and the `workspace.json` URI.
- Stable and Insiders are **separate Stores** sharing one format. Sessions differ; the same workspace-hash dir name appears under both roots. Record which channel a Capture came from.

## 3. How a live session grows on disk

- **Variant C (current)**: append-only in the common case — each save appends `kind:1`/`kind:2` lines (§4). Trailing newline is not guaranteed; the last line may also be mid-write. A parser must tolerate a truncated final line (keep what replays cleanly). Do **not** bank on append-only: the journal is VS Code's private format and may be compacted (rewritten as a fresh `kind:0`) in future versions — on any mtime/size change, re-capture the whole file rather than byte-appending.
- **Variant B**: the whole `.json` file is rewritten on every save. Same path, same id, entirely new bytes; re-capture whole file.
- **Variant A**: the whole `interactive.sessions` array value is rewritten. Once a workspace migrated to files, the row goes stale forever (observed: row untouched since May 2025 while files kept growing).
- A session created empty is written immediately (many `kind:0`-only journals and `requests: []` snapshots exist — opened chat panels that were never used). These have no Dialog.

## 4. Variant C: the delta journal (kind 0/1/2)

One JSON object per line: `{"kind": <0|1|2>, "k": [<keypath>], "v": <value>}` (`k` absent on kind 0). Replay in file order to reconstruct the session object:

- **kind 0 — init snapshot**: `v` is the complete session object; becomes the accumulator. Always line 1; a later kind 0 (never observed, but possible on compaction) resets the accumulator.
- **kind 1 — set**: assign `v` at keypath `k`. Keypath segments are strings (object keys) or integers (array indices), e.g. `{"kind":1,"k":["requests",0,"result"],"v":{…}}` or `{"kind":1,"k":["customTitle"],"v":"General greeting inquiry"}`.
- **kind 2 — append**: `v` is an **array of items to push** onto the array at `k` (elementwise extend, not push-as-one), e.g. `{"kind":2,"k":["requests"],"v":[{…request…}]}` or `{"kind":2,"k":["requests",0,"response"],"v":[{…chunk…}, …]}`. If the target is missing, treat `v` as the new array; if the target exists but is not an array, wrap it.
- Replay must **create missing intermediate containers** along the keypath (object for string segment, array for integer segment — decided by the *next* segment's type), and skip lines whose keypath types mismatch the tree rather than aborting the file.
- Skip blank and unparseable lines.

Real journal, in order (9 lines, one request): `kind 0` (empty session) → `kind 2 ["requests"]` (push request with `message`, empty `response`) → `kind 1 ["inputState","mode"]` → `kind 1 ["customTitle"]` → `kind 1 ["requests",0,"result"]` → `kind 1 ["requests",0,"responseMarkdownInfo"]` → `kind 1 ["requests",0,"followups"]` → `kind 1 ["requests",0,"modelState"]` → `kind 2 ["requests",0,"response"]` (push all response chunks at once, thinking + markdown).

Observed oddity: journals also set scalars late (`kind 1 ["responderUsername"] "GitHub Copilot"` after a kind 0 with `responderUsername: ""`).

## 5. The session object (all variants, `version: 3`)

Top-level fields (count / 274 sampled sessions):

| field | seen | meaning |
|---|---|---|
| `version` | 274 | always `3` |
| `sessionId` | 274 | native id (§2) |
| `creationDate` | 274 | **ms** epoch |
| `lastMessageDate` | 261 | **ms** epoch; absent on some fresh journal sessions until first save |
| `requests` | 274 | the conversation — array of request/response turns |
| `requesterUsername` | 253 | e.g. GitHub login; `responderUsername` (274) = `"GitHub Copilot"` (empty string early in journals) |
| `customTitle` | 28 | user-visible/model-generated native title; without one, leave the Conversation untitled rather than repeating body text |
| `initialLocation` | 274 | `"panel"` / `"editor"` … |
| `isImported` | 253 | migration artifact |
| `requesterAvatarIconUri`, `responderAvatarIconUri` | 253/261 | noise |
| `isNew`, `hasPendingEdits`, `pendingRequests`, `inputState` | 7/21/12/21 | journal-era UI state; noise |

### `requests[]` — one entry per user turn

Key fields (counts / 436 sampled requests): `message` (436), `response` (436), `variableData` (436), `agent` (436), `contentReferences` (436), `codeCitations` (436), `requestId`/`responseId`/`timestamp` (431), `result` (397), `followups` (396), `isCanceled` (429), `isHidden` (47, DB-era), `workingSet` (40), `editedFileEvents` (46), `modelId` (30, e.g. `"copilot/auto"`), `modelState`/`timeSpentWaiting`/`responseMarkdownInfo` (7, journal-era), `confirmation` (16), `vote`/`voteDownReason` (6), `slashCommand` (2).

- `timestamp`: **ms** epoch, one per request — this is the only per-turn timestamp; user and assistant Messages of a turn share it.
- `result.metadata` holds `renderedUserMessage` — the *actual prompt sent*, including injected `<context>`/`<reminderInstructions>` blocks, plus token counts (`promptTokens`, `outputTokens`) and `toolCallRounds`. Never extract from it (§7).
- `modelState`: `{value, completedAt}` (ms) — journal-era completion marker.

### User message: `requests[].message`

`{ text: string, parts: [...] }`. **`message.text` is the complete, clean user-typed text** — inline references like `#file:types.ts` appear in it verbatim. Extraction rule: take `message.text`; if absent, join `parts[].text` with `"\n\n"`. Part kinds observed (463 text / 16 dynamic / 29 tool / 6 agent / 2 subcommand): all carry `text` + `range`/`editorRange`; `dynamic` adds a file URI in `data`, `tool` adds `toolId`/`toolName`, `agent` marks an `@agent` mention, `subcommand` a `/command`. Their `text` is already contained in `message.text` — parts are only a fallback.

### Assistant response: `requests[].response[]` — ordered chunk array

Chunk kinds across 436 requests (count): *untagged* (5117), `toolInvocationSerialized` (1944), `inlineReference` (1596), `codeblockUri` (994), `textEditGroup` (970), `prepareToolInvocation` (829), `undoStop` (654), `confirmation` (19), `progressTaskSerialized` (13), `progressMessage` (6), `mcpServersStarting` (4), `thinking` (2+, dominant in 2026 sessions), `warning` (1). Extraction (§6) and filtering (§7) rules below.

## 6. Content extraction → Conversation Messages

Per request, in `requests` order (each request yields ≤1 user + ≤1 assistant Message, both timestamped from `request.timestamp`, ms→s):

1. **User Message** = `message.text` (fallback: joined `parts[].text`). Skip if empty after trim.
2. **Assistant Message** = concatenation of response chunks **joined with `""`** (chunks are contiguous stream fragments — markdown continuity depends on not inserting separators), then trimmed. Skip if empty. Per chunk:
   - **untagged** (no `kind`, has string `value` — the markdown prose): include `value`, **unless it is a pure code-fence fragment** — one or more runs of ``` ` ```×≥3 with only whitespace around them (regex `^(\s*`{3,}\s*)+$`). 1563 such orphan-fence chunks exist in sampled data: VS Code splits markdown around code blocks it renders natively (`codeblockUri`/`textEditGroup` carry the actual code), leaving dangling fences that corrupt transcripts if kept.
   - `markdownContent`: include `content.value`. Not present in local sampling (untagged chunks fill this role) retain support for this native shape.
   - `inlineReference`: include a short inline name so prose reads naturally — `basename(inlineReference.fsPath ?? inlineReference.path)`. **v2 improvement**: a second shape exists in real data, `{name, location, containerName}` (symbol references) — fall back to `name`.
   - `thinking` (2026-era): `{id, value, generatedTitle?, metadata?}` — extended reasoning text. **Excluded** (tier-4+ noise). Note: `generatedTitle` on a thinking chunk is where the auto-title comes from; `customTitle` at session level is the usable copy.
   - Everything else remains Action/UI evidence: tool invocations, edits, progress, confirmations, warnings and similar records. Preserve them in Captures and exclude them from Conversation Dialog.
3. **Dialog flag**: session has Dialog iff any user/assistant Message is non-empty. Sessions with `requests: []` (very common — idle chat panels) have none.
4. **Title**: `customTitle` if present, else first non-empty user message line truncated to 120 chars.
5. `requests[].isCanceled` is common (user hit stop); the partial response chunks that exist are still real assistant output — extract them.
6. `isHidden: true` is not a Dialog boundary by itself; empty request text is filtered by rule 1.

## 7. Noise / injected-context filtering

VS Code needs **no injected-context stripping** (unlike Codex): `message.text` is the pure user input. The injected context (`<context>` with current date, `<reminderInstructions>`, attached-file contents) lives only in `result.metadata.renderedUserMessage` and in `variableData` — both unextracted. The noise problem here is entirely on the response side (§6 chunk filtering) plus:

- **Whole-session noise**: `requests: []` sessions (no Dialog — archived, flagged); `chatEditingSessions/` dirs (not conversations); `state.vscdb.backup`.
- **vsliveshare workspaces**: 36 workspace dirs point at `vsliveshare-workspace-tmp-*` temp `.code-workspace` files — legitimate sessions, useless project paths (§9).
- No guardian/system-session concept exists in this Source.

## 8. Subagent linkage

**None.** Copilot Chat has no child-session mechanism in this format; nothing in 274 sampled sessions links one `sessionId` to another. (`toolCallRounds` in `result.metadata` are intra-request agent loops, not Sessions.) The Adapter returns no SubagentLinks.

## 9. Timestamps & project path

- **All timestamps are milliseconds since epoch** (integers): `creationDate`, `lastMessageDate`, `requests[].timestamp`, `timing.*`, `modelState.completedAt`. Convert ms→s.
- Session times: created = `creationDate`; updated = `lastMessageDate` ?? file mtime ?? created. Variant A rows have no backing file, so no mtime fallback.
- **Project path**, in priority order:
  1. `workspace.json` → `folder` (single-folder) or `workspace` (multi-root, `.code-workspace` URI) — parse as `file://` URI, percent-decoded. Project name: basename; strip `.code-workspace`.
  2. With no usable `workspace.json`, inspect request `variableData.variables[].value` for URI objects (`fsPath`, `path`) or nested `value.uri`. Resolve workspace evidence only on the Source Host; extraction must not probe another Host's filesystem.
  3. `emptyWindowChatSessions` files have no workspace at all → project null.

## 10. Quirks (the list)

1. **Three coexisting storage variants** (§1); files supersede the DB row for the same `sessionId`; DB rows are the *only* copy for workspaces last used pre-migration (~early 2025).
2. Open live `state.vscdb` read-only.
3. One DB key = all sessions of the workspace — there is no per-session SQL predicate; only `length(value)>2` and DB mtime prune work server-side.
4. **Orphan code-fence chunks** (1563 observed) must be dropped or transcripts get corrupted fences (§6).
5. Markdown prose chunks are **untagged** (`kind` absent); every *tagged* noise kind hides its text under `content.value`, so "top-level string `value`" is the safe include test.
6. `inlineReference` has a second, symbol-shaped variant `{name, location, containerName}` — use `name`.
7. `variableData.variables[].value` may nest the URI under `value.uri` — include this shape in project inference.
8. Journal files may lack a trailing newline; final line may be mid-write — replay what parses, drop the tail.
9. Include `emptyWindowChatSessions/` in globalStorage/Insiders enumeration.
10. Insiders and stable are independent Stores with identically-named workspace-hash dirs — keep channel in Provenance.
11. Workspace-hash dirs can be purely numeric (e.g. `1782540907281`) and lack `workspace.json`.
12. `chat.ChatSessionStore.index` disagrees with the directory listing (31 vs 40); never enumerate from it.
13. Timestamps ms, not s; `request.timestamp` is the only per-turn time — user and assistant share it.
14. Sessions with `requests: []` are ubiquitous (opened panels); expect a large no-Dialog fraction.
15. Largest observed session file: 13.3 MB single-line JSON — parse streaming or accept bounded slurp per file, never per store.

## 11. Sanitized raw examples

Variant C journal (`chatSessions/<id>.jsonl`), abridged:

```jsonl
{"kind":0,"v":{"version":3,"creationDate":1773184933340,"initialLocation":"panel","responderUsername":"GitHub Copilot","sessionId":"example-id-…","hasPendingEdits":false,"requests":[],"pendingRequests":[],"inputState":{"attachments":[],"mode":{"id":"agent","kind":"agent"}}}}
{"kind":2,"k":["requests"],"v":[{"requestId":"request_2749b22a-…","timestamp":1773203416479,"agent":{"extensionId":{"value":"GitHub.copilot-chat"}},"modelId":"copilot/auto","message":{"text":"hello","parts":[{"range":{"start":0,"endExclusive":5},"editorRange":{"startLineNumber":1,"startColumn":1,"endLineNumber":1,"endColumn":6},"text":"hello","kind":"text"}]},"variableData":{"variables":[]},"response":[]}]}
{"kind":1,"k":["customTitle"],"v":"General greeting inquiry"}
{"kind":1,"k":["requests",0,"result"],"v":{"timings":{"firstProgress":2472,"totalElapsed":2961},"metadata":{"renderedUserMessage":[{"type":1,"text":"<context>\nThe current date is …\n</context>\n<reminderInstructions>\n…"}],"promptTokens":12345,"outputTokens":67}}}
{"kind":1,"k":["requests",0,"modelState"],"v":{"value":1,"completedAt":1773203420035}}
{"kind":2,"k":["requests",0,"response"],"v":[
  {"kind":"mcpServersStarting","didStartServerIds":[]},
  {"kind":"thinking","id":"thinking_0","value":"The user has just said \"hello\". …","generatedTitle":"Considered user greeting"},
  {"kind":"thinking","id":"","value":"","metadata":{"vscodeReasoningDone":true,"stopReason":"text"}},
  {"baseUri":{},"supportHtml":false,"supportThemeIcons":false,"value":"Hello! I'm GitHub Copilot. …"}
]}
```

Variant B snapshot / variant A array element (same object shape), abridged response showing the fence quirk:

```json
{
  "version": 3, "sessionId": "example-id-…", "creationDate": 1747340620000, "lastMessageDate": 1747342269711,
  "requesterUsername": "user", "responderUsername": "GitHub Copilot", "initialLocation": "panel", "isImported": false,
  "requests": [{
    "requestId": "request_3fd1ce13-…", "responseId": "response_f1edbb0e-…", "timestamp": 1747340620792,
    "message": {"text": "move the expand-arrow to the info-row …", "parts": [{"kind": "text", "text": "move the expand-arrow …"}]},
    "variableData": {"variables": [{"kind": "file", "id": "vscode.implicit.viewport", "name": "file:Widget.tsx",
      "value": {"$mid": 1, "fsPath": "/workspace/Projects/app/src/Widget.tsx", "path": "…", "scheme": "file", "external": "file:///…"}}]},
    "response": [
      {"supportHtml": false, "supportThemeIcons": false, "value": "I'll move the arrow. First let me look at "},
      {"kind": "inlineReference", "inlineReference": {"$mid": 1, "fsPath": "/workspace/Projects/app/src/Widget.tsx", "path": "…", "scheme": "file"}},
      {"supportHtml": false, "supportThemeIcons": false, "value": ":\n\n````\n\n````\n"},
      {"kind": "toolInvocationSerialized", "toolId": "copilot_editFile", "toolCallId": "…", "isComplete": true, "isConfirmed": true,
       "invocationMessage": {"value": "Editing [](file:///workspace/Projects/app/src/Widget.tsx)", "supportHtml": false, "uris": {"file:///…": {"$mid": 1}}}},
      {"kind": "codeblockUri", "isEdit": true, "uri": {"$mid": 1, "fsPath": "/workspace/Projects/app/src/Widget.tsx"}},
      {"kind": "textEditGroup", "uri": {"$mid": 1, "fsPath": "/workspace/Projects/app/src/Widget.tsx"}, "edits": [[{"range": {}, "text": "…"}]], "done": true},
      {"supportHtml": false, "supportThemeIcons": false, "value": "Done — the arrow is now right-aligned in its own column."}
    ],
    "result": {"timings": {"firstProgress": 1200, "totalElapsed": 9000}, "metadata": {}},
    "followups": [], "isCanceled": false, "contentReferences": [], "codeCitations": [],
    "agent": {"extensionId": {"value": "GitHub.copilot-chat"}, "id": "github.copilot.editsAgent"}
  }]
}
```

`workspace.json`:

```json
{"folder": "file:///workspace/Documents/Projects/my-app"}
{"workspace": "file:///workspace/Documents/Projects/my-app/my-app.code-workspace"}
```

## 12. Capture recommendation (per ADR-0001)

One Capture per Session (`captures/<host>/vscode/<sessionId>.<ext>.zst` + `<sessionId>.meta.json`), by variant:

- **Variant C (journal)**: the **verbatim bytes of the `.jsonl` file**, untouched. Replay (§4) happens at Extract time, never at Capture time — so journal-replay bugs are fixable retroactively (PLAN slice 7). Ext: `.jsonl.zst`.
- **Variant B (snapshot)** and **`emptyWindowChatSessions`**: the **verbatim bytes of the `.json` file**. Ext: `.json.zst`.
- **Variant A (DB row)**: no per-session native byte range exists (one SQL value holds all sessions), so per SPEC ("for SQLite-backed Sources, the Capture is the per-session blobs, not a database copy"): parse the `interactive.sessions` array once, and capture **each array element re-serialized as a single compact JSON document** — the session's complete native payload, schema-free. Ext: `.json.zst`. Record `capture_kind: "db-row-extract"` in the sidecar so the non-verbatim re-serialization step is honest and auditable. Only do this for `sessionId`s that do **not** exist as files (files supersede, §1).
- On growth or rewrite, capture the whole native unit. Snapshot/journal compaction can shrink bytes without losing Dialog; use the documented Dialog-coverage settlement policy rather than byte size alone. A stale DB row must not replace a more complete file-backed Session.
- **Sidecar** (`.meta.json`): native path, workspace-hash dir, channel (`code` / `code-insiders`), storage variant, workspace.json URI (project provenance — `workspace.json` itself is not captured), file mtime/size at capture, captured-at, adapter version.

This keeps the archive's promise: everything §5–§7 extracts — and every field we currently ignore (`result.metadata`, `variableData`, tool invocations, edited-file events, votes) — survives verbatim (variants B/C) or as an honest per-session re-serialization (variant A) for future re-extraction.
