---
status: accepted
---
# Capture admission and native copy election

Eval harnesses copy native Session IDs into many workspace directories. Settling
all these files into one Host Capture makes each Pull alternate diverged bodies,
retaining identical Prior Revisions indefinitely.

Keep ADR-0004's (Source, native id) Session identity. Before Bookmark filtering,
elect the lexicographically smallest absolute native path among eligible copies
on a Host. Size and mtime do not decide election: either can change during work.
A winner's genuine Dialog divergence still retains a Prior Revision; retained
bodies deduplicate by a streaming SHA-256 of decompressed bytes. Host-level
Projection Winner and Origin Attribution remain separate decisions.

Including location in identity would preserve divergent conversations separately,
but would also duplicate synced/moved Sessions and change existing Session,
Subagent and Digest identities. We accept that losing copies are not archived;
a truly independent conversation must have a distinct native ID. Adding an earlier
path, removing the winner, or changing admission policy can elect a new copy.
Stateless SSH Bookmark limitations still apply to old-mtime arrivals: use a full
Source Pull to recover those.

Add `[pull] exclude-paths`, a list of working-path globs with Kind path semantics
(case sensitive, literal separators, `**` recursive, dot segments allowed, home
expansion). Match the recorded Session working path; if absent, match the native
project directory fact (which can be encoded, e.g. Claude). Never infer exclusion
from names containing `eval`. Apply before election, on the Source Host, including
SSH Export via repeated shell-quoted `--exclude-path` arguments. Claude and Pi
probe JSONL paths directly with streaming reads; other adapters may use temporary
Capture evidence in a disposable spool. Excluded units never enter the Archive,
index, failure/unchanged counts or Bookmark contribution.

This deliberately refines the September Kind decision: Kind omission still keeps
Captures regardless of a claim. Explicit operator Capture admission is independent
of Kinds and Dialog quality, and may permanently forgo new native evidence.

A policy change triggers one derived-index Rebuild under Pull's writer lock.
Rebuild skips matching archived Captures before indexing and Bookmark contribution;
no Capture, Prior Revision or Digest is deleted or rewritten. Removing the policy
restores archived evidence on the next Pull/Rebuild. The policy snapshot is derived
SQLite state, not an additional durable input. Retained nonexcluded Captures
continue to restore Bookmarks; full Source Pull remains available. A Rebuild may
be expensive once on a large Archive. Existing generated Views require Render to
reconcile their files. Digests remain on disk and can become eligible again when
the policy is removed; no freshness or format changes occur.

No automatic archive cleanup is introduced. Deduplication prevents new redundant
Prior Revisions; it does not rewrite historical revision directories.
