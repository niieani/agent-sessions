---
status: accepted
---
# Kind matching uses current Dialog Statistics

Low-value test Sessions and unanswered requests should be omitted according to user-configured thresholds, not a built-in judgment of significance. Extend Kinds with whole-Conversation Dialog Statistics, evaluated identically by Digest and Render against the current Conversation; a claim is a derived result, never a permanent ingestion decision.

## Considered Options

- A separate eligibility gate or greeting detector — rejected: duplicates Kind policy and cannot express the user's own definition of useful Dialog.
- Permanent exclusion during ingestion — rejected: an unanswered or tiny Session can grow into useful work; excluding its Capture would prevent recovery.
- Session Day thresholds — rejected for these predicates: a short follow-up belongs to its substantial Session and should not disappear independently.

## Consequences

Captures and Conversations are preserved regardless of a claim. Later ingestion or re-extraction changes the facts; the next Digest/Render evaluation may admit a previously Omitted Session without changing configuration. Removing a View does not remove its durable Digest. Thresholds are configurable heuristics, not semantic claims that no work occurred. Existing first-match priority and parent-to-Subagent Kind inheritance remain in force.

Contract and examples: [SPEC](../../SPEC.md#dialog-statistics). Implementation: [PLAN](../PLAN.md), slice 27.
