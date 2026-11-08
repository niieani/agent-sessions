---
status: accepted
---
# Supersede by Dialog coverage, not byte size

Append-oriented file Sources (Claude, Codex, Pi) used a one-way byte rule: a native payload smaller than its Capture never replaced it, because vendors prune and rewind session files and Captures are the only source of truth ([ADR-0001](0001-capture-files-are-the-only-source-of-truth.md)). In September 2026 Codex rewrote existing rollout files into a leaner format — per-event records such as `user_message`, `agent_message`, and `exec_command_end` became `item_completed`, `session_meta` and `turn_context` slimmed down — while keeping the same `response_item` Messages and the same first and last timestamps. Every rewritten file was smaller, so every one was refused. On the Bundle path a refusal counts as a failed Session so a stateless remote resends it, which held the laptop's Codex Bookmark in place: roughly a hundred failures a night and exports that grew from seven Sessions to more than seven hundred. Byte size had stopped saying which body holds more of the conversation.

We therefore settle a changed append-oriented body by the Dialog its Adapter parses out of both bodies, and never by bytes:

- **Covers** — the existing Capture's Messages form an ordered prefix of the incoming body's, and the incoming span ends at the same moment or later: supersede, whatever the size.
- **Truncated** — the incoming Messages are a strict prefix of the Capture's, or the Messages match but the incoming span ends earlier: refuse and keep the Capture. The refusal counts as a failed Session on both ingest paths, so the Bookmark holds and the pull reports it.
- **Diverged** — anything else: the Messages no longer line up. The old body is kept as a **Prior Revision** and the incoming body becomes the Capture. The run reports it without failing.

A Message's identity is its role, timestamp, and text. Text is included so that a rewrite which keeps the timestamps but removes or redacts content is treated as Diverged, which preserves the old body, rather than as Covers, which would replace it. The span end is the later of the last Message timestamp and the Conversation's last recorded activity, so trailing tool activity that was cut away also counts as truncation.

The Dialog view is an Adapter method (`body_dialog`) that reads one body without Archive context: no parent resolution and no child gating. Codex's view therefore includes guardian Sessions and Messages replayed from a parent, so both bodies of one Session are compared on the same terms. A body that fails to decompress or yields no Dialog view is an error for that Session: the staged payload is discarded, the Capture stays, and the Session counts as failed. There is no fallback to size.

A Prior Revision lives next to its Capture, at `<native-id>.revisions/<captured-at-ms>.<ext>.zst`, with its original Provenance sidecar stored verbatim as `<captured-at-ms>.revision.json`. The body is hard-linked before the new body is renamed over the Capture, so the current Capture is never missing during the commit. The `.revision.json` suffix keeps revisions out of every sidecar walk: Enumerate, rebuild, and Capture lookup see only current Captures.

Logical SQLite Sources (Cursor and T3) keep their revision-first order: a lower logical revision is refused and a newer one supersedes even when it is smaller. Their Capture is a projection of database rows at a logical revision the Store itself orders, and a newer revision may legitimately hold less after a native revert or deletion. Dialog comparison would refuse those legitimate states. For T3 it would also be the wrong unit, because an Orchestrator Thread extracts to Orchestrator facts rather than a Conversation.

## Considered Options

- **Keep the byte rule and allowlist the Codex migration** — rejected: it would fix this migration only, and the next format change by any vendor would fail the same way.
- **Compare role and timestamp only** — rejected: a rewrite that redacts text while keeping timestamps would be treated as Covers and replace the richer body.
- **Refuse on divergence** — rejected: a real divergence, such as Codex dropping Messages from the middle of a compacted thread, would fail every run forever with no way to accept the vendor's current state.
- **Accept divergence without keeping the old body** — rejected: it loses exactly the evidence ADR-0001 exists to protect.
- **Store revisions in a separate top-level tree** — rejected: a revision belongs with its Capture, and the `.revision.json` suffix already keeps it out of every Capture walk.

## Consequences

- A changed append-oriented Session costs two streamed Extract passes during Capture settlement: one of the Capture and one of the staged body.
- Local pull now counts a refusal as a failed Session, as Bundle ingest already did. A truly truncated native file therefore keeps its Source's Bookmark from advancing and is reported on every run until the file grows back past the Capture.
- Pull output reports `revised` and `refused` counts for each Source. `revised` is included in `changed`; `refused` is included in `failed`.
- Retained Prior Revisions are durable preserved evidence. `rebuild` does not read them and they are never Extracted.
- Cross-Host Projection election still prefers the larger body for append-oriented Sources. It chooses between Hosts and not between revisions of one Capture, so it is unchanged.

Contract: [SPEC](../../SPEC.md) (Ingest, Bundle, Resilience).
