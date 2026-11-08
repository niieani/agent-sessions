# Title Write-back is the one sanctioned write to a native Store

**T3 transport superseded by [orchestration v2 integration](2026-10-08-t3-orchestration-v2-authority.md).** Other Source routes and ownership rules remain in force.

Native Stores are read-only — the hard rule that keeps live agent databases safe. Digests produce good Session Titles that the harnesses themselves lack (most Codex threads are unnamed), and those names are what makes a thread findable when resuming inside Codex, Claude Code, or T3 Code. We therefore allow exactly one write path: the explicit `push-titles` command pushes a Session Title into a Store's own name field, per Source through a documented safe route, never from inside `pull`, `digest`, or `render`, and preserving External Names unless explicitly opted in. Routes: Codex `UPDATE threads SET name` in `state_5.sqlite`; Claude Code an appended `custom-title` record plus its `custom-title.json` sidecar; T3 Code only through its `thread.meta.update` server command (HTTP API on the running server, never its projections or offline engine); Cursor never, because its `composerData` blob is rewritten whole. The tool records every value it pushed in the Session's Digest (the Pushed Name) so its own earlier writes can be refreshed while External Names stay untouched.

A Store records no origin for a name: a person's rename and the harness's own auto-naming are indistinguishable. Codex now auto-names every new thread from its first turn, so on Codex an External Name is the normal case and a strict never-overwrite rule would make write-back inert there. The rule is therefore per Source and opt-in: `[titles.<source>] overwrite-external = true` lets write-back replace External Names on that Source, accepting that a hand-typed rename on that Source may be replaced too. The default for every Source stays never-overwrite.

## Consequences

- ADR-0001's read-only stance becomes "read-only except Title Write-back"; the invariant that `pull` never writes a Store is unchanged and tested.
- Every route has a dry-run and a test against a fixture copy of the Store; the first version runs only against Stores on the local Host.
- A pushed title returns on the next `pull` as the native title, so Transcripts and the harness agree.
- the operator enables `overwrite-external` for Codex knowingly; a Source whose names are mostly hand-chosen (Claude Code's `custom-title`, Cursor) keeps the default.

## T3 route (slice 33)

Each resolved local Orchestrator Thread receives its linked Session's Session Title, including a Recovered Session. Read `projection_threads` read-only with WAL visibility; dispatch `thread.meta.update` with a fresh `commandId`, `threadId`, and `title` to `/api/orchestration/dispatch`. T3 clears pending title regeneration itself. Transport follows the reference project CLI's server origin and bearer protocol. Its credential issuance and offline mode write persistence, so neither is permitted here: discover `userdata/server-runtime.json`, use an existing paired bearer credential from `T3_BEARER_TOKEN`, and fail explicitly per Thread on missing auth, unreachable server, or rejection. Never print credentials.

`[titles.t3] overwrite-external` defaults false, like Codex. `session.json.pushed["t3:<thread-id>"]` records each Thread separately. Before mutation, `push-title.pending` identifies that Thread; recovery recognizes an already-dispatched title and records ownership without redispatch. Recover pending work before any other Thread sharing that Digest; read the latest Digest when recording each Pushed Name. Dry-run performs no auth discovery or HTTP request. Live real-data acceptance remains attended and deferred by the operator.
