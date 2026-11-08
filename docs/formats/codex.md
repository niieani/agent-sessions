# Codex — native format reference

Reference for implementing the Codex Adapter (`src/adapters/codex.rs`). Terms per [CONTEXT.md](../../CONTEXT.md).

The native format is interpreted by the Rust Adapter and covered by synthetic fixtures. Recheck vendor schemas when they change.

---

## 1. Store layout

```
~/.codex/sessions/                 # the Store
  YYYY/MM/DD/                      # date partition — LOCAL date of session creation
    rollout-<YYYY-MM-DDThh-mm-ss>-<uuidv7>.jsonl
    rollout-<YYYY-MM-DDThh-mm-ss>-<uuidv7>_<segment-uuidv7>.jsonl
~/.codex/archived_sessions/        # flat dir; sessions the user archived in the UI
    rollout-<...>-<uuidv7>.jsonl   # MOVED here (no longer present in the dated tree)
~/.codex/state_5.sqlite            # thread catalog; threads.id → threads.name
```

- **Filename pattern**: ordinary files end in the logical thread UUID. Newer Codex Desktop can roll one logical thread into another physical file named with `<thread-uuid>_<segment-uuid>` *(verified 2026-08-26)*. The embedded timestamp is **local time** (e.g. `rollout-2026-08-19T08-01-49-…` whose first record is `2026-08-19T15:01:49.111Z`, i.e. PDT = UTC−7) *(verified)*. The `YYYY/MM/DD` directory is likewise the **local** creation date.
- **The uuid is UUIDv7** (all observed ids start `019…`/`01a0…`), so it is time-ordered and encodes the creation instant in its first 48 bits.
- **`archived_sessions/` is part of the Store.** Files are *moved* there when the user archives a thread — the same session is absent from the dated tree *(verified: `rollout-2026-02-25…019c9610….jsonl` exists only in `archived_sessions/`)*. Enumerate both roots.
- Adjacent but out of scope: `~/.codex/history.jsonl` (`{session_id, ts (epoch s), text}` — one line per user prompt, cross-session; useful only as a discovery cross-check) *(verified)*.
- **Native title catalog:** `state_5.sqlite.threads.name`, joined by the rollout's logical thread id, is the user-visible `/rename` value. Read it through the shared read-only, WAL-aware SQLite boundary (`mode=ro`; `immutable=1` only as an idle-WAL retry) and persist the per-Session row presence and name in Capture metadata so `rebuild` remains Store-independent. `threads.title` is raw opening text and is never a title source. A JSONL `thread_name_updated` event is the fallback for older rollouts absent from the catalog; an existing catalog row wins even when its name is null or empty.

### Enumeration strategy (cheapest)

1. Walk `~/.codex/sessions/**/rollout-*.jsonl` **and** `~/.codex/archived_sessions/rollout-*.jsonl`. ~1000 files: a full readdir+stat walk is trivially cheap.
2. Gate on **file mtime > bookmark** before opening anything.
3. **Do not prune by date directory alone.** Resuming a session **appends to its original file in its original date directory** — a file under `2026/05/29/` verifiably grew across 2026-05-30 and 2026-05-31 *(verified, §3)*. Date-dir pruning would miss grown sessions. SPEC's "date-partitioned directory pruning" for Codex is only safe as a *stat-avoidance* micro-optimization for directories entirely older than some deep horizon, and even then it trades correctness; prefer the flat mtime gate.
4. **mtime caveat**: large batches of historical files share mtime `2026-07-29 09:57` regardless of content dates *(verified)* — some past migration/copy bulk-touched them. mtime is a valid *change* signal going forward but not a valid *content-time* signal; derive session times from record timestamps, never from mtime.

## 2. Session identity & provenance

- **Native id** = the first UUID after the filename timestamp = `payload.id` of the **first** `session_meta` record. A UUID after `_` identifies a physical rollover segment, not another Session.
- `session_meta` is the first line of every file (60/60 sampled files) *(verified)*.
- **`session_meta` recurs**: it is re-emitted (same `id`) each time the session is resumed, and **fork files embed their ancestors' `session_meta` records** (§3). Files with 2–35 `session_meta` lines are common *(verified)*.
- **Identity hazard:** replayed ancestor metadata must not overwrite the file identity. In a forked file the replayed *ancestor* metas come **after** the file's own meta, so a forked-but-never-resumed session gets its **parent's** id. v2 rule: **native id from the filename, confirmed against the first `session_meta`'s `payload.id`; ignore subsequent metas for identity.**
- `session_meta.payload` fields (newest CLI 0.148, keys *(verified)*): `id`, `session_id` (newer alias; on subagents it holds the *parent/root* thread id — do not use for identity), `forked_from_id`, `parent_thread_id`, `timestamp` (ISO UTC), `cwd`, `originator` (`"Codex Desktop"`, `"codex_cli_rs"`, …), `cli_version`, `source` (string `"vscode" | "cli" | "exec" | "unknown"` **or** object `{"subagent": …}` — a union, brace for both shapes), `thread_source` (`"user" | "subagent" | "automation" | "agent_created_thread"` *(verified census)*), `model_provider`, `base_instructions` (string in old files, `{text: …}` object in new ones — huge; do not extract), `git` (`{commit_hash, branch, repository_url}`), `context_window`, `history_mode`, `dynamic_tools` (huge tool schemas), `instructions` (old files, often `null`).
- Sidecar provenance worth keeping: native path (encodes local creation date + whether archived), `originator`, `cli_version`, `forked_from_id`, `parent_thread_id`, `git.branch`/`repository_url`.

## 3. How a live session grows on disk

- **Append-only JSONL.** One JSON object per line. Resume appends a fresh `session_meta` (same `id`) and continues appending events; a single file verifiably accumulated `session_meta` records at 00:55, 00:57, 01:05 … through the next day *(verified: `2026/05/29/rollout-…019e7660….jsonl`, 35 metas, 5122 lines)*. No rewrite behavior was observed; bookmark/growth logic may treat captures as extendable, but re-capture defensively on any non-prefix change.
- **Desktop rollover.** A resumed thread may continue in a suffixed physical file. Its first `session_meta.id` remains the logical thread ID; record ordinals can overlap the preceding file, so never deduplicate or order segments by ordinal. Order physical segments by the filename timestamp, then retain every record.
- **Forking can replay history into a new file.** A UI fork and `spawn_agent` with inherited history create a **new UUIDv7 file** whose head begins with its own `session_meta`, then foreign ancestor metas and copied records. Those copied records carry the fork instant's top-level `timestamp`, not the original utterance time. This is one ordinary-subagent shape, not a universal one: fresh `spawn_agent` children can contain only their own meta plus delegated work (§7). `forked_from_id` is useful provenance but not a transcript-boundary rule.
- **Archiving moves the file** to `~/.codex/archived_sessions/` (§1) — from the dated tree's perspective a session can *disappear*; the Archive must never mirror deletions.

## 4. Record taxonomy

Every line: `{"timestamp": "<ISO-8601 UTC, ms, Z>", "type": "<top-level type>", "payload": {…}}` — exactly these three keys on all sampled records old and new *(verified)*.

Top-level `type` values observed *(verified census over ~140 files incl. full streams of 80)*:

| top-level `type` | payload discriminant | role in extraction |
|---|---|---|
| `session_meta` | — | identity, provenance, subagent linkage (§7), guardian drop (§6) |
| `response_item` | `payload.type` (below) | **the message stream** — primary extraction source |
| `event_msg` | `payload.type` (below) | UI event mirror — duplicates of response_items; fallback + metadata only |
| `turn_context` | — | per-turn `cwd`, model, approval/sandbox policy → project path updates |
| `compacted` | — | `payload.message` = the compaction summary text injected after context overflow |
| `world_state` | — | `{full: bool, state: {agents_md, skills, plugins_instructions, …}}` — injected env snapshot, noise |
| `inter_agent_communication_metadata` | — | `{trigger_turn: bool}` — multi-agent plumbing, noise |
| `message` (legacy top-level) | — | `{type:"message", role, content}` lines; **zero occurrences in the current store** *(verified)* — keep the handler as cheap legacy tolerance |

`response_item.payload.type` values *(verified census)*:

| `payload.type` | maps to | notes |
|---|---|---|
| `message` | **Conversation Message** (user/assistant) | §5 |
| `reasoning` | no (Action-adjacent) | `summary[].{type:"summary_text",text}` + often `encrypted_content` |
| `function_call` / `function_call_output` | Action, except §5.1 | `name`, `arguments` (JSON **string**), `call_id`; output is a plain string, sometimes JSON. Subagent linkage mined here (§7) |
| `custom_tool_call` / `custom_tool_call_output` | no (Action) | newer unified-exec tools; output may be a **block array** of `{type:"input_text",text}` |
| `web_search_call`, `tool_search_call`, `tool_search_output`, `ghost_snapshot` | no | newer Action types |
| `agent_message` | no — **not assistant speech** | inter-agent mail: `{author, recipient, content:[{input_text}|{encrypted_content}]}` *(verified)*. Despite the name, never render as dialog; content is often Fernet-encrypted |

`event_msg.payload.type` values *(verified census)*: `token_count` (dominant by volume), `agent_reasoning`, `agent_message`, `user_message`, `task_started`, `task_complete`, `patch_apply_end`, `exec_command_end`, `mcp_tool_call_end`, `web_search_end`, `turn_aborted`, `context_compacted`, `thread_settings_applied`, `thread_goal_updated`, `thread_rolled_back`, `thread_name_updated`, `sub_agent_activity`.

`event_msg` records are UI-facing **duplicates** of the response_item stream *(verified: a session's `user_message`/`agent_message` texts match its `response_item` message texts)*. Extract messages from `response_item` only, else you double every message. Exceptions worth reading from `event_msg`:

- `agent_message` (`{message, phase, memory_citation}`) — **fallback only**: promote these to assistant Messages *only when the session has zero assistant `response_item` messages*. Keep that rule for old/degenerate files.
- `thread_name_updated` (`{thread_id, thread_name}`) — legacy title fallback for rollouts without a row in the sqlite catalog *(verified sample: `"thread_name": "Update 1.5.1 release notes"`)*. Latest non-empty value wins only within that fallback source. A catalog row is authoritative even when its `name` is null or empty, which clears the Conversation title.
- `thread_rolled_back` (`{num_turns}`) — the user undid the last N turns; subsequent records supersede rolled-back ones. Rare (14 in the 80-file census). A naive linear extraction will include rolled-back turns; rollback projection is a separate extraction concern.
- `task_complete.last_agent_message` duplicates the final assistant text — ignore.
- `token_count` — per-turn usage stats; ignore for Conversations (relevant only if the token-count open question lands).

## 5. Message extraction (`response_item` → Message)

Payload shape: `{type:"message", role, content, id?, phase?, internal_chat_message_metadata_passthrough?}`.

- **Roles**: map `user` and `assistant` to Dialog. `developer` and legacy `system` records carry injected instructions and remain outside Conversations; Captures preserve them.
- **Content**: either a raw string (legacy) or an array of blocks. Text lives in blocks of type `input_text` (user/developer), `output_text` (assistant), or `text` (legacy); take `.text`, drop empty, join parts with `\n\n`. Observed block types beyond text: `input_image` (`{type, image_url: "data:image/png;base64,…", detail}` — base64 payloads inline; skip for Conversation, note as attachment) and `encrypted_content` *(both verified)*.
- **`phase`** on newer assistant messages: `"commentary"` (progress updates streamed mid-turn) vs `"final_answer"` *(verified; ~40% commentary)*. Old files lack the key. Keep both as assistant Messages (both are speech, not Actions); preserving the distinction in the model is optional but the Capture keeps it regardless.
- **Empty-after-filtering messages are dropped**, and a session with no user Message or no Dialog is still Captured but flagged no-Dialog.

### 5.1 Agent-created task input (`function_call_output` → Agent Message)

Codex Desktop records the opening instruction for an agent-created task as a
`response_item` `function_call_output`, rather than as a user message. The
output is a `codex_delegation` XML envelope:

```xml
<codex_delegation>
  <source_thread_id>…</source_thread_id>
  <input>Implement the requested task.</input>
</codex_delegation>
```

Promote only this exact shape to an Agent Message, preserving the native record
timestamp, when all of the following hold: the first `session_meta` has
`thread_source:"agent_created_thread"`; the result is the first projected
Message and all earlier `response_item`s are omitted `message` context;
`namespace == "codex_app"`; `name == "create_thread"`; and the XML is
well-formed with nonempty `source_thread_id` and `input`. Render the input as
`<agent>`, not `<user>` or `<parent>`. The source task may be outside the
Archive, so `source_thread_id` is validation evidence, not a Subagent link.

Every other function result remains an Action, including malformed envelopes,
ordinary user-created Sessions, and later create-thread results.

## 6. Noise / injected-context filtering

**Drop whole sessions:**

- **Guardian sessions** (automated approval-judging threads): `session_meta.payload.thread_source == "subagent"` **and** `payload.source.subagent.other == "guardian"`. Still true in current files *(verified sample under `2026/07/29/`, `source: {"subagent":{"other":"guardian"}}`)*. Capture them but exclude them from Conversations/views — their "user" messages are machine-generated transcript-review prompts.

**Drop individual user-role messages whose text starts (after `trimStart`) with an injected tag**. Observed reality is broader *(verified prefix tally over 40 files: 1826 plain vs the following)*:

| leading tag | meaning | verdict |
|---|---|---|
| `<environment_context>` | cwd/sandbox/approval snapshot | drop  |
| `<skill>` | injected skill instructions | drop  |
| `<user_instructions>` | injected project/user instruction block | drop |
| `<subagent_notification>` | child-agent status push | strip + mine for links (§7), never render as dialog |
| `<turn_aborted>` | "user interrupted the turn" marker | **keep** — it is real dialog context |
| `<recommended_plugins>` | injected plugin ads | drop  |
| `<codex_internal_context source="goal">` | persistent-goal runtime envelope | normalize its user objective per **Goal preservation** below; discard the rest |
| other `<codex_internal_context>` | injected app context | drop |
| `<codex_delegation>` | delegated-task envelope from another thread | drop from user dialog; the `<input>` inside is the parent's instruction (§7) |
| `<realtime_delegation>` | voice/realtime turn containing `<input>` plus generated `<transcript_delta>` | retain decoded `<input>` only; discard transcript replay |
| `<send_user_message_question_reply>` | JSON array of user question replies | retain every question and answer per **Question replies** below |
| `<external_codex_apps_open_page>` | app navigation, e.g. `{"page_id":null}` | drop |
| `<hook_prompt hook_run_id="…">` | generated hook instructions | drop |
| `<heartbeat>` | system-sent continuation | drop only at the start of a text part |
| `<user_shell_command>` | user ran a shell command via the composer; contains command + result | judgment call: not typed prose, mostly tool output — drop from Conversation (recoverable from Capture) |
| `# AGENTS.md instructions for …` | untagged injected agent instructions | drop the entire user-role block; older View-shaped captures may wrap it in `<user>` |
| Composer Prompt: context sections, then `## My request for Codex:` / `## My request:`, then the typed request | app/IDE composer expansion | see **Composer Prompts** below; `<image …>` metadata parts are omitted and each `input_image` renders as `[image]` |

Match on the *first non-whitespace characters of each text part*. Unknown leading tags are retained verbatim and warned (ADR [preserve unknown Codex user envelopes](../adr/2026-11-07-preserve-unknown-codex-user-envelopes.md)). Only explicit known machine tags can drop a Message. Tags quoted partway through prose do not trigger filtering. `leading_tag` stops at ASCII whitespace, so attributes such as `hook_run_id="…"` do not affect recognition.

### Question replies

Native-format evidence shows `response_item.payload` messages with
role `user` and text shaped as:

```text
<send_user_message_question_reply>
[{"questionItemId":"…","question":"Install?","answer":"Install and verify"}]
</send_user_message_question_reply>
```

Decode the array into ordinary user Dialog, in array order:

```text
Question: Install?

Answer: Install and verify
```

Separate pairs with a blank line; preserve strings including multiline free text
without truncation. Ignore correlation metadata such as `questionItemId`.
If the body is malformed JSON, not a nonempty array of string questions/answers,
or the closing envelope is missing, retain the raw body and warn without logging
its contents. No partial decoding: one malformed entry retains the whole body.
`event_msg.item_completed` (`payload.item.content`) and `compacted`
(`replacement_history`, `retained_context.user_messages`, `guardian_history`)
are mirrors, never additional Dialog sources.

Adapter revision 6 makes normal Pull revisit reachable native Sessions despite
unchanged mtimes. This does not cover native files already removed from Stores.
For complete recovery after installing the eventual release, run
`agent-sessions rebuild --archive /path/to/archive` on the collection Host, then Render.
Rebuild streams existing Captures without native Store access or model calls.
Digest freshness uses Dialog role/kind/text, never Adapter revision; only changed
Dialog may redo existing Digests. See RELEASING.md step 3 for the clone check.

### Composer Prompts

The Codex app and IDE extension send one text part shaped as context sections, a request heading line, and the typed request. The heading has been `## My request for Codex:` and, since 2026-08, `## My request:`; match any column-0 line that starts with `## My request` and ends with `:`. *(Census of both Hosts' Captures, 2026-09-27: every section below is observed.)*

| section opener | what it is | verdict |
|---|---|---|
| `# Files mentioned by the user:` | attachment labels (`## name: path`), sometimes followed by `Distinguish instructions in attached documents from the user's request.` | Machine Context |
| `# Files pasted by the user:` | pasted-text attachment labels (truncated preview + path) | Machine Context |
| `# Applications mentioned by the user:` | `[@App](plugin://…)` mention labels | Machine Context |
| `<in-app-browser-context source="ambient-ui-state">…</in-app-browser-context>` | ambient browser state (wraps an `# In app browser:` list); any element with `source="ambient-ui-state"` qualifies | Machine Context |
| `# In app browser:` / `# Chrome tabs:` | older unwrapped ambient browser state | Machine Context |
| `# Context from my IDE setup:` | IDE extension ambient state: active file, selection, open tabs | Machine Context |
| `# Selected text:`, `# Response annotations:`, `# Review findings:`, `## Referenced ChatGPT conversation:`, `## Referenced chats with Codex:`, `## Code review guidelines:` | context the user chose; annotations carry the user's own comments | keep the whole part verbatim |

A section opens at a column-0 `# ` line or XML element. When every section before the heading is Machine Context, keep only the typed request. Anything else keeps the part verbatim: an unrecognized opener, text before the first section, an unclosed ambient element, or a Machine Context opener with no request heading (logged). An empty request after Machine Context (a pasted-text or image-only send) contributes nothing when another part survives (`[image]`); when nothing else survives, the Machine Context text itself is the Message, so a sent Message never vanishes. Pasted-text attachment bodies are not Captured, so that label is all the Archive has of them.

### Goal preservation

`<codex_internal_context source="goal">` mixes user intent with generated continuation policy, token budgets, completion audits, and other runtime instructions. Preserve only the user-provided `<objective>` value. Goal edits use `<untrusted_objective>` and follow the same rule. Represent these as typed user Goal Messages rather than ordinary text containing renderer markup.

Maintain the active normalized objective while extracting in native order:

- no active objective, or objective differs from the active value → emit a Goal Message;
- unchanged objective → emit a Goal Continuation;
- A → B → A → emit all three Goals; de-duplication is consecutive-state based, not global.

Render a Goal as:

```text
<user>
<goal>
Objective text
</goal>
</user>
```

Render a continuation as `<user>` containing `<goal-continuation />`. Add both goal tags to Transcript delimiter escaping.

An objective may be the exact instruction `Read the Codex goal objective file at ABSOLUTE_PATH before continuing.` Capture may resolve that path only when it is a regular, bounded UTF-8 `attachments/<id>/goal-objective.md` beneath the same Codex Store root. Snapshot the contents and resolution state into the Capture sidecar on the Source Host so Bundle ingest and `rebuild` remain Store-independent. Never read the live path during Extract or Render. Resolved snapshots are never downgraded if the native file later disappears.

If the attachment is missing or cannot be safely read, retain the original inline objective as an unresolved Goal and warn. Render it as:

```text
<user>
<goal>
<!-- note: objective file no longer available -->
Read the Codex goal objective file at /…/goal-objective.md before continuing.
</goal>
</user>
```

Missing references remain eligible for a later local Capture enrichment pass. Paths outside the allowed attachment tree are rejected and never opened.

## 7. Subagent linkage and child Dialog extraction

Codex subagents are full sessions in the same Store (their own rollout files). The raw child-side parent fact is sufficient to identify the relationship; the hard problem is excluding any materialized parent context without hiding child-owned Dialog.

**Evidence update — 2026-08-25:** a read-only scan of 946 local rollouts found 272 ordinary `source.subagent.thread_spawn` children. Of those, all 107 files without `forked_from_id` had exactly one own `session_meta`; 164 of 165 with `forked_from_id` had multiple metas, but one forked file had only one. A separate inherited child interleaves 42 copied and 96 child-owned response-item IDs, proving that neither `forked_from_id`, a second meta, a timestamp, nor a single prefix cutoff is a safe extraction rule. Phase 9 implements the fail-closed rules below: proven fresh and inherited children project, while unsafe candidates remain Capture-only.

**Child side — first `session_meta.payload`** *(verified current sample)*:

```json
{"id": "example-id-…", "session_id": "example-id-…", "forked_from_id": "example-id-…",
 "parent_thread_id": "example-id-…", "thread_source": "subagent",
 "source": {"subagent": {"thread_spawn": {
     "parent_thread_id": "example-id-…", "depth": 1,
     "agent_path": "/root/example-task",
     "agent_nickname": "Example Agent", "agent_role": null}}},
 "agent_nickname": "Example Agent", "agent_path": "/root/example-task"}
```

- Native identity remains the filename UUID / first `payload.id`. Read the first meta only for this fact; later foreign metas are replay evidence, never an identity override.
- Parent id: `source.subagent.thread_spawn.parent_thread_id` (also observed camelCase `parentThreadId`); fall back to top-level `parent_thread_id`. Do **not** use `session_id`: on children it can name the root/parent. Do **not** use `agent_path`: it is a tree path, not a Session id.
- Display fields: `agent_nickname` / `agent_role` (top-level or inside `thread_spawn`) and `depth`. Persist these as the child `SubagentParent` fact even when the parent has no usable spawn event.
- Guardians remain excluded: `thread_source:"subagent"` with `source.subagent.other:"guardian"` is an automated approval review, not a child Dialog.

### Child shapes and fail-closed extraction

1. **Direct / non-replayed child.** No `session_meta` whose `payload.id` differs from the child's native ID. Example: `2026-08-07/rollout-…00000002-1111-4111-8111-111111111111.jsonl` has one own meta and a direct delegated user request. Extract its retained `response_item` stream normally. This remains correct if provenance happens to carry `forked_from_id`.
2. **Inherited child.** A foreign meta and copied response items are present. Resolve the direct parent Capture on the same Host and Source. Build the set of all non-empty direct-parent `response_item.payload.id` values; discard every child `response_item` carrying an ID in that set, then extract only the retained records. This works for leading replay and interleaved replay alike. Count Action seams only from retained records; skipped copied calls and results must not leak a seam into the child View.
3. **Unproven child.** Missing direct parent Capture, a missing/empty response-item ID on any potential Dialog record in either Capture, malformed parent identity, or an old id-less stream: keep Capture-only. Do not infer a boundary from file position, `forked_from_id`, timestamps, UUID order, or the second `session_meta`.

The direct parent's Capture is required because it includes any ancestor material already inherited by that parent. ID membership therefore removes parent and grandparent replay without assuming a contiguous copied prefix.

### Rendered inherited context

The Transcript contains only child-owned Dialog. Its frontmatter retains the relationship and exposes materialized-context removal as metadata, never as an inline pseudo-message:

```yaml
subagent: true
parentId: <parent-session-id>
context:
  mode: inherited
  sourceId: <parent-session-id>
  copiedItemCount: 28
```

`parentId` means “spawned by”; `context.mode: inherited` means replay was positively identified and omitted. Direct children omit `context` (or may state `mode: fresh`). The raw excluded ID set is an extraction aid; retain it or a digest only if future exact-context reconstruction requires it. A parent transcript may continue after spawn, so a link alone is not an exact historical snapshot.

**Parent side** (references to children; used for interleaving by timestamp):

- `event_msg` `sub_agent_activity`: `{event_id, occurred_at_ms (epoch ms), agent_thread_id, agent_path, kind: "started"|…}` *(verified)* — the cleanest parent-side child-ID and placement source; prefer it when present.
- `function_call` `spawn_agent` / `wait_agent` / `list_agents` records carry child references in `function_call_output.output`, a JSON string with `agent_id`, `agent_path`, or `agents[]`. Arguments may include opaque encrypted content; do not treat it as Dialog.
- `<subagent_notification>{JSON}</subagent_notification>` blocks inside user messages: JSON has `agent_id` / `agent_path` and a `status` object. Strip notification blocks and collect exact child references; the surrounding text (if any) survives as a user Message.
- Dedupe refs by child id, merging fields.
- `agent_path` (`/root/foo`) is a *tree path*, not a session id — resolve to session IDs via `sub_agent_activity.agent_thread_id` or child `session_meta` when possible; keep the two identity spaces separate: link by UUID, keep `agent_path`/nickname as display metadata.
- `response_item` `agent_message` (`author`/`recipient` = agent paths) and `inter_agent_communication_metadata` are inter-agent chatter — usable as evidence of multi-agent activity, not as dialog.

Child-side parent facts are enough to resolve and render a child where its direct parent is captured. Parent-side references enrich description and placement; absent/untrusted parent timing must place the `<subagent>` link at the renderer's deterministic fallback boundary rather than inventing chronology.

## 8. Timestamps

- Every line has a top-level `timestamp`, an ISO-8601 UTC string with milliseconds and `Z`.
- `session_meta.payload.timestamp` = session creation (ISO UTC).
- created_at = first record's timestamp (≈ `session_meta.payload.timestamp` ≈ UUIDv7 time); updated_at = max record timestamp, *not* file mtime (§1 bulk-touch quirk).
- **Fork lie**: replayed ancestor records carry the fork instant, not original utterance time (§3).
- Filename/dirs: local time (§1). `sub_agent_activity.occurred_at_ms` and `task_started.started_at`: epoch ms / s respectively.

## 9. Project path derivation

- Primary: `session_meta.payload.cwd`; later `turn_context.payload.cwd` values update it (last wins). Turn contexts can also carry approval, sandbox, model, reasoning, workspace and collaboration metadata; those fields remain native evidence.
- Project name = last path segment.
- Quirk: desktop-app subagents may have `workspace_roots` including `~/.codex/visualizations/...`; automation runs (`thread_source:"automation"`) have whatever cwd the automation targets. `cwd` is still the best available signal.

## 10. Quirks (the bite list)

1. **Forked files replay ancestor history** — duplicated content across sessions, replayed `session_meta`s that break naive id extraction, replayed timestamps that lie. §2–3.
2. **`archived_sessions/` exists and files move there** — sessions "vanish" from the dated tree.
3. **Resume appends into the original date directory** — date-dir pruning misses growth; use mtime over the full tree. §1.
4. **Historical mtimes were bulk-touched** (many files share `2026-07-29 09:57`) — mtime ≠ content time.
5. **`event_msg` duplicates `response_item`** — extract from one stream or double every message. §4.
6. **`response_item.payload.type == "agent_message"` is not assistant speech** (inter-agent mail, often encrypted) despite sharing a name with `event_msg.agent_message` which *is* assistant speech. §4.
7. **`session_meta.source` is a string-or-object union**; `session_id` on subagents is the *parent's* id. §2.
8. **Guardian sessions look like real dialogs** (user/assistant messages) but are automated approval reviews; drop by `thread_source`+`source.subagent.other`. §6.
9. **Injected context arrives as `role:"user"`** and the tag set grows over time. §6.
10. **`base_instructions`/`dynamic_tools` bloat `session_meta`** (tens of KB per line) — stream, don't assume small first lines.
11. **Encrypted content** (`encrypted_content` blocks, encrypted `spawn_agent` messages) is normal, not corruption.
12. **`threads.name` provides the native title** — `thread_name_updated` is the fallback; without either, omit title metadata rather than repeating the first user Message already present in the body.
13. **`compacted` / `context_compacted` records** mark context-overflow summarization; the file still contains full history (append-only), so extraction ignores them.
14. Legacy top-level `{type:"message"}` lines existed once  but no longer occur in this store — tolerate, don't rely.
15. Tag-prefixed user messages require explicit classification; unknown envelopes survive with a warning. §6.

## 11. Sanitized raw examples

`session_meta` (current, truncated):

```json
{"timestamp":"2026-08-20T00:44:02.291Z","type":"session_meta","payload":{
  "session_id":"01a01c9f-f60d-…","id":"01a01c9f-f60d-…",
  "timestamp":"2026-08-20T00:44:00.458Z","cwd":"/workspace/Projects/example",
  "originator":"Codex Desktop","cli_version":"0.148.0-alpha.15","source":"vscode",
  "thread_source":"user","model_provider":"openai",
  "base_instructions":{"text":"You are Codex… (huge)"},
  "git":{"commit_hash":"…","branch":"main","repository_url":"git@github.com:org/repo.git"},
  "context_window":258400,"history_mode":"…","dynamic_tools":[…huge…]}}
```

User / assistant messages:

```json
{"timestamp":"…","type":"response_item","payload":{"type":"message","role":"user",
  "content":[{"type":"input_text","text":"Fix the failing test in foo.rs"}]}}
{"timestamp":"…","type":"response_item","payload":{"type":"message","id":"msg_…","role":"assistant",
  "phase":"final_answer","content":[{"type":"output_text","text":"Done — the test…"}],
  "internal_chat_message_metadata_passthrough":{"turn_id":"…"}}}
```

Injected context (drop) and subagent notification (strip + link):

```json
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text",
  "text":"<environment_context>\n  <cwd>/workspace/…</cwd>\n  <approval_policy>on-request</approval_policy>…"}]}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text",
  "text":"<subagent_notification>\n{\"agent_id\":\"example-id-…\",\"status\":{\"completed\":\"docs: …\"}}\n</subagent_notification>"}]}}
```

Parent-side child evidence; native title; rollback:

```json
{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_…",
  "occurred_at_ms":1785880312102,"agent_thread_id":"019fcec2-f787-…",
  "agent_path":"/root/transcribe_x","kind":"started"}}
{"type":"event_msg","payload":{"type":"thread_name_updated","thread_id":"example-id-…","thread_name":"Update 1.5.1 release notes"}}
{"type":"event_msg","payload":{"type":"thread_rolled_back","num_turns":1}}
```

## 12. Capture recommendation (per ADR-0001)

**The Capture contains every physical `rollout-*.jsonl` segment for the logical thread, in filename timestamp order, zstd-compressed** — `captures/<host>/codex/<uuid>.jsonl.zst`.

- **Unit**: one logical thread = one Session = one Capture. Most have one byte-for-byte physical file. Rollover threads concatenate their physical files without reserialization; the Capture sidecar records each segment's path, mtime, and size.
- **Scope**: both `~/.codex/sessions/**` and `~/.codex/archived_sessions/*`. Archiving moves the file between them, not a new Session — same uuid, same Capture, sidecar records the current native path. A move with unchanged bytes (filename, mtime, size) rewrites only the sidecar's location, never the body, and counts as unchanged.
- **Every line is kept**, including `token_count` spam, `world_state`, `dynamic_tools`, encrypted blocks, and replayed fork history — that is the point of schema-free Capture; JSONL compresses ~10:1 under zstd. No filtering, reordering, or re-serialization ever happens at Capture time; all rules in §§5–7 are Extract-time only and therefore retroactively fixable.
- **Growth**: files are append-only in practice (§3). A grown file (same uuid, larger size, newer mtime) supersedes the previous Capture of the same Session. Do not assume prefix-stability: recompress the whole current file rather than appending to the old Capture, and treat a *shrunk* file as an anomaly to log (recapture anyway; the older, longer Capture may be retained per archive policy — never overwritten with less data without note).
- **Sidecar** (`<uuid>.meta.json`): native path at capture time (dated-tree vs archived), file size, mtime, captured-at, adapter version, plus cheap header-derived fields (`originator`, `cli_version`, `thread_source`, `forked_from_id`, `parent_thread_id`, `cwd`) so `rebuild` and fleet manifests never need to decompress to triage.
- **Streaming**: capture = open, stream through zstd, close. Never buffer the file (76 MB max observed); never parse-validate more than line 1 (identity confirmation) on the capture path.
