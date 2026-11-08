---
status: accepted
---
# Session Intent is stored on each Session Day Digest

A multi-day Session sets its purpose in the user's opening turns, but a continuation Session Day's Digest sees only that day's Dialog plus earlier Digests' titles and summaries. Readers that consume only one Digest — the Vault's nightly Work Recap — could not judge "what's next" against what the user set out to do. The Session's Intent is therefore a Digest output: the first Digest establishes it from the user's opening turns, continuation requests receive it in Context and return `intent: null` to keep it or a string when the user restated or changed direction, and the tool writes the resolved Intent into every Session Day Digest.

## Considered Options

- **Store Intent once in `session.json`** — rejected: the Session Digest is rewritten whenever a Session Day is added, so an early day's Transcript would show a later day's direction, and a Transcript renders one Session Day without its siblings.
- **Store Intent only on the day that set or changed it** — rejected: every reader, including Render, would reconstruct the effective value by walking earlier files.
- **Pass the Session's opening user Messages as Context on every continuation day, without an Intent field** — rejected as the only mechanism: it costs input tokens on every continuation, drops direction changes made on intermediate days, and still leaves the digest-only reader without a stated Intent. It is kept as the fallback when earlier Digests record no Intent.
- **Let the model restate Intent every day** — rejected: repeated paraphrase drifts; `null` makes "unchanged" exact and cheap.

## Consequences

- Digests written before Intent existed stay valid (the field is optional) and are not regenerated; a `promptVersion` bump never makes a Digest stale ([ADR-0005](0005-digests-are-a-durable-second-layer.md)). The first new or grown Session Day of such a Session establishes Intent from the Session's opening turns.
- A third request schema (`intent` required, no `topic`) keeps the "continuation must establish Intent" case unbreakable by construction, alongside the Topic schemas.
- A Session Day re-digested because its covered range grew takes Intent only from earlier Session Days, so a redirect on that day is still recorded.
- Explicit Loose repair fingerprints `promptVersion`; the first repair after this change regenerates every eligible Loose Session.

Contract: [SPEC](../../SPEC.md) (Digests). Implementation: [PLAN](../PLAN-completed.md), slice 35.
