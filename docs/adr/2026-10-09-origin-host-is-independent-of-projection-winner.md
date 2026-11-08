# Origin Host is independent of Projection winner

A synchronized native Session can have newer bytes on another Host. Revision
election selects Dialog; origin attribution is a separate decision. Every
consumer keeps the canonical `(Source, native id)` identity.

## Evidence and decision

Pull captures native birth time as epoch milliseconds in each file-backed
Capture sidecar: std `metadata.created()` on macOS; a direct `statx` syscall
requesting `STATX_BTIME` on Linux, including static musl (std only uses Linux
statx on GNU targets). Check the returned `stx_mask` bit, and query the open
file descriptor during Capture so evidence belongs to the captured inode.
A missing returned birth-time bit or an unavailable syscall explicitly records
unknown. Other stat errors fail capture. Aggregate Sessions record individual
segment births and use their earliest birth only when every segment has a known
birth. Native
Stores remain read-only. Birth observations refresh even for unchanged bodies;
full-source replay backfills historical Captures without a new revision.

For multiple Capture Hosts, require known births for every Host. The earliest
birth confirms origin when it precedes the next earliest by at least 1,000 ms.
Report whether that birth matches the first extracted timestamp within 1,000 ms as
corroboration. This survives Pull order: later copies cannot beat an earlier
native birth. Syncthing's temp-file/rename writes re-create the destination
file, explaining the observed gap. Tools that preserve birth times may leave a
tie; genuinely missing/tied evidence remains inconclusive.

Exact T3 Provider Thread bindings from all current Host projections are a
secondary signal when births cannot decide. Reject projections binding several
provider identities. Claude children inherit their exact parent's binding.
`owner_node_id` is an orchestration node, not a machine identifier. Re-derive
T3 evidence on every reconcile; never persist DB-derived claims. Birth evidence
has priority over disagreeing T3 evidence; the report includes both for audit.

Without decisive birth or unambiguous T3 evidence, retain the previous Host and
report ambiguity (or unverified for a sole Capture Host). Durable schema-2
`host-attributions/` preferences contain only identity and the last chosen Host.
They preserve this choice across rebuild and are backed up with Captures,
Digests, Project Assignments and Leads. First ambiguous sighting uses the
Projection winner; confirmed birth attribution needs no preference records.
Rebuild never opens the old database. A corrupt index is safely replaceable.

## Operations and presentation

Reconcile once after a complete Pull. A full-source Pull records births for
all native files still present; then Rebuild re-applies them to existing data.
No sync-daemon API, claim import, daemon or operator audit. The rejected
round-1 accumulated-claims schema is removed rather than migrated; if it was
installed, move its `host-attributions/` out before the full-source backfill.

`origins list` reports all multi-Host Sessions with observed births, start-time
corroboration, fresh T3 facts and the decision. Render uses attributed
`session.host`, independent `session.capture_host`, and confirmed
`session.copy_hosts`. Copies never multiply Messages, Digests, Leads, Session
Days, activity or Daily/Work Recap inputs.
