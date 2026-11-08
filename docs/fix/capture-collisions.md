# Capture collision and eval admission verification

Example Capture admission configuration:

```toml
[pull]
exclude-paths = ["**/*eval-workspaces/**/vault"]
```

Same-Host native copies elect the lexicographically smallest eligible absolute
native path before Bookmark filtering. Size, modification time and enumeration
order cannot make copies alternate. Identity remains (Source, native id), preserving
existing links and Digest identities. A winner's divergent Dialog still revises;
retaining identical prior body bytes is idempotent across compression settings.
Losing native files are not archived separately. See the admission ADR for why
location was not added to identity and the old-mtime SSH limitation.

A policy change rebuilds the derived index. Matching Captures and their Prior Revisions remain durable on disk; excluded units do not contribute Bookmarks. Removing the policy restores indexed evidence on the next Pull or Rebuild. No Digest format or freshness inputs change.

## Metadata-only admission correction for 0.4.13

Admission probes never Capture or Extract. Claude/Pi read recorded cwd, Codex
reads the first session header cwd (ignoring replayed headers and turn context),
and T3 reads the recorded worktree. ChatGPT and Cursor return no working path.
Rebuild exclusion reads the identical fields from archived Captures. The native
project directory remains the fallback when that field is absent. Fixture tests
compare native and Capture admission paths for every Adapter.

This avoids context-free extraction of incomplete ChatGPT history and removes
admission's temporary recapture of every Session on every Pull. Archive-context
extraction still validates history after Capture settlement. An admission error
fails only that Session identity, counts as a failed unit, withholds the Source
Bookmark, and retries next Pull; healthy Sessions continue. Busy admission is
retry-later. Unknown admission on any Native Copy withholds the entire identity.
These rules apply to both local Pull and SSH Export.

Rebuild retains previous origin decisions in memory while replacing the derived
index, so stable ambiguities log at DEBUG; new ambiguities and conflicting
execution claims still warn. Digest status explicitly unlocks on writer drop:
closing a descriptor alone can leave its lock held by a concurrent fork until
exec/exit. A deterministic duplicate-descriptor regression reproduces the former
Starting-versus-Finished failure without timing or retries.

## Optional cleanup, requiring the operator's decision

Do nothing until a successful backup and repeat-Pull verification. Inventory the
problem `.revisions` directory by streaming SHA-256 of decompressed bodies; retain
at least one body and matching original revision sidecar for each distinct hash.
Review current body and distinct retained bodies before deciding whether divergent
copies are independent useful conversations. If desired, move redundant body/sidecar
pairs into a dated quarantine outside `captures/`, keeping a manifest of original
paths and hashes. Rebuild and compare indexed Session counts and Dialog before
considering any permanent deletion. Old eval Captures can similarly be quarantined
only after exclusion and index verification, with their sidecars and all associated
revision directories. Keep the Digest as durable evidence unless separately decided.
No cleanup command or automatic deletion ships here; existing historical Captures remain durable.
