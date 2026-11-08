---
status: accepted
---
# Project Assignments are durable files

A historical Session created in a temporary workspace may belong to a real Project that workspace metadata cannot identify. Record an explicit Project Assignment by Source/native identity in an archive-owned file; SQLite indexes it and remains disposable. Native Project identity, path, and repository-key mappings express reusable Project associations, while an assignment expresses one Session's association.

## Considered Options

- Database-only relationships — rejected: rebuilding would lose user-supplied knowledge.
- Editing generated Markdown — rejected: Render overwrites it, and Digest would not see the relationship.
- Rewriting Capture metadata — rejected: a later classification is not a native fact.
- Per-Session exceptions in workspace config — rejected: mixes individual archival annotations with reusable rules.

## Consequences

Assignments form another durable input alongside Captures and Digests. This extends [ADR-0001](0001-capture-files-are-the-only-source-of-truth.md) and [ADR-0005](0005-digests-are-a-durable-second-layer.md): Captures remain authoritative for native evidence; all database state is reconstructible from durable files. Backups include assignments and configuration as well as Captures and Digests.

An assignment wins over Native Project, repository-key and recorded-path grouping and a Loose treatment, but never bypasses Omitted placement. It applies across Session Days and supplies inherited Subagent project context without rewriting the recorded path. Assignment changes affect subsequent Digest context and Render; they do not silently regenerate existing Digests or change Locked Titles.

Contract: [SPEC](../../SPEC.md#project-assignments). Implementation: [PLAN](../PLAN.md), slices 28–29. A Vault workflow can maintain path mappings or invoke the assignment command; scheduling and semantic discovery remain Vault responsibilities.
