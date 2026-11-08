# A Session its Store has deleted may be recovered from an Orchestrator Thread

Stores delete Sessions on their own schedule (Claude Code after thirty days by default). The first full backfill found twelve Claude Sessions that T3 Code still projects in full — 595 Messages — but whose JSONL files were gone before the archive's first pull. ADR-0001 says Captures are the only source of truth and the T3 format decision says an Orchestrator Thread never creates a Conversation; taken literally, those Sessions are lost although a faithful record exists on disk. We therefore allow one recovery path: when an Orchestrator Link resolves to a provider Session that no Host has ever Captured, Extract builds a **Recovered Session** from the Orchestrator Thread's projected Dialog. It keeps the provider as its Source and native id (ADR-0004 identity is unchanged), records provenance naming the Orchestrator Thread it came through, and renders, digests, and pushes titles like any Session. A real Capture always wins: the moment a Host yields the Session, the recovery is superseded, the provider Capture becomes the Conversation, and the Orchestrator Thread returns to contributing Augmentation only.

## Considered Options

- **Never recover** — rejected: the record exists and the archive's whole purpose is not to lose it; the fidelity audit showed T3's projection matched or exceeded the Claude extraction.
- **A separate `t3` Source for recovered Sessions** — rejected: the identity is the provider Session; a second identity would split the same conversation across two Sources and break Orchestrator Links, Kinds, and Vault filenames.
- **Recover into the provider Capture format** (fabricate a JSONL) — rejected: a Capture must be a verbatim Store artifact (ADR-0001); a synthetic one would be indistinguishable from a real one later.

## Consequences

- Recovery is a projection over the T3 Capture, so `rebuild` reproduces it and nothing new becomes irreplaceable.
- A Recovered Session is marked in its facts and Transcript frontmatter; consumers can tell it from a captured one.
- Message-level formatting may differ from what the provider would have written; the projection is T3's rendering of the Dialog, which the fidelity audit found complete.
- The Store Retention warning remains the first line of defence; recovery only covers Sessions that ran through an Orchestrator.

Recovery chooses one complete eligible Thread by logical revision, discriminator, Adapter version, then lexical Source/Thread/Host tie-breakers; it never merges unrelated Threads. Empty Dialog or unknown roles cannot synthesize a Session. The provider native path remains absent until a real Capture supplies it, so Title Write-back cannot target a missing Store or mistake the Orchestrator database for a provider file.

Title Write-back to a linked T3 Orchestrator Thread uses its own Thread id and the Recovered Session’s Session Title through ADR-0006; it does not require a provider native path or recreate the deleted provider Store.
