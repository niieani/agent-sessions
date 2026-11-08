# Adapter format warning fixes

## Brief

Fix all diagnosed local `pull` warnings: Codex rollover filenames/session identity, Codex realtime delegation messages, and Cursor NULL bubble rows. Preserve source fidelity and eliminate associated projection loss. Produce atomic commits.

## Final behavior

- One Codex thread split across rollover files becomes one Capture and one Conversation containing every segment in source order.
- Codex `<realtime_delegation>` user records retain only their `<input>` text.
- Cursor typed NULL bubble rows are treated as tombstones, not malformed JSON.
- Ordinary single-file Codex sessions and non-NULL Cursor bubbles remain unchanged.

## Scope and limits

Read/write: adapter code, adapter tests/fixtures, owning format documentation, this workdesk. Native stores and existing local archives remain read-only. No compatibility shim for the incorrect suffixed-stem identity. No archive migration; a subsequent pull/rebuild uses the corrected adapter behavior.

## Principles and decisions

- Codex `session_meta.id` is logical identity; filename suffix identifies a physical rollover segment.
- Aggregation must not rely on `seen` deduplication, directory iteration order, or ordinal uniqueness.
- Capture remains rebuild-sufficient and records every physical segment's change discriminator.
- Incorrect captures emitted by the old filename parser are recoverably quarantined; their identities are durably superseded in sidecar metadata and removed from derived state.
- Unknown injected tags remain fail-safe dropped; realtime delegation gets explicit normalization.
- Cursor SQL NULL is an explicit typed state and is skipped before JSON parsing.

## Acceptance and verification

- Rollover regression: two files with one logical ID produce one session, ordered messages from both segments, one Capture, stable no-op second pull. Verify integration test.
- Segment growth regression: append to either physical file triggers recapture; shrink retains the richer Capture per archive policy. Verify integration test.
- Realtime regression: `<input>` survives; transcript delta does not. Verify unit/integration test.
- Cursor regression: NULL bubble row produces no warning-worthy parse path and no message; valid bubbles survive. Verify integration/unit test.
- Existing behavior: focused suites, full test suite, fmt, clippy pass.
- Design compliance and cleanup: independent agent reviews against this charter and `work.md` report no unresolved findings.

## Execution shape

Three sequential TDD slices, each independently committable. Codex rollover slice includes capture metadata extensions, upgrade reconciliation, and deeper integration verification. Realtime and Cursor slices are narrow parser fixes. Permanent format docs updated with accepted behavior.
