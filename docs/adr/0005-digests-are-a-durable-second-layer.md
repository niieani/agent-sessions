# Digests are a durable second layer; rebuild never regenerates them

**Extended 2026-09-04:** [Project Assignments](2026-09-04-project-assignments-are-durable-files.md) add durable classification files; SQLite remains reconstructible. The original decision below records its earlier scope.

ADR-0001 makes Captures the only irreplaceable data and everything else a rebuildable projection. A Digest — the model-written Topic, title, summary, TL;DR, and Classification of one Session Day, plus the Session Title — is derived from a Conversation, but producing it costs a model call, is non-deterministic, and yields names other systems link to (a Locked Title names a file in a Vault). We therefore store Digests as plain JSON files under `<archive>/digests/<source>/<native-id>/` — one per Session Day (`<workday>.json`) and one per Session (`session.json`) — next to Captures as the second durable layer. `rebuild` re-extracts Conversations and never touches Digests; `archive.db` may cache them. Each Digest records the Message range it covers, the model, and the prompt version, so a later run extends rather than redoes.

## Considered Options

- **Store Digests in `archive.db`** — rejected: makes the database irreplaceable, the exact failure ADR-0001 avoids.
- **Regenerate Digests on rebuild** — rejected: cost, non-determinism, and every Locked Title (and thus every filename and inbound link) would change.
- **Store Digests only in the consumer (the Vault)** — rejected: the producer would depend on its consumer for its own state, and a second consumer could not exist.

## Consequences

- Backup is now two directories: `captures/` and `digests/`.
- The Digest file schema is versioned; a Locked Title, once written, is never changed by the tool (a separate explicit command may re-lock and report the resulting renames).
- A Digest whose covered Message range is smaller than the Session Day's current Messages is stale and is extended on the next run; Ongoing Session Days are not digested until a later Capture closes them.
- Subagent Sessions never receive a Digest; the user's conversation is the value, not agent-to-agent exchange.
