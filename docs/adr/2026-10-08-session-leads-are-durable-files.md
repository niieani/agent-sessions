---
status: accepted
---
# Session Leads are durable files

External producers know whether an agent led a Session. Preserve that knowledge independently of native Captures, Conversation roles, Subagent relationships, and Host provenance. Match only Source/native Session identity; a declaration can arrive before its Capture.

Follow [Project Assignments](2026-09-04-project-assignments-are-durable-files.md): one atomically published JSON file per Session, with a rebuildable SQLite index. Retain the complete latest lead in Feed v1 shape, including producer provenance. Do not alter native evidence or make SQLite the sole record. Per-feed Bookmarks are archive files too and bind the configured feed name to its URL.

Validate each complete page before publishing leads, upsert only that page into the index, then publish its Bookmark. Failure leaves the uncommitted page eligible for replay. Files may have been partially replaced; replay is idempotent and converges to the page's last declaration per identity. Leads have no deletion operation. Feed order, not recordedAt, determines replacement. Independently configured feeds cannot compare opaque cursors; the latest imported declaration wins. Producers should avoid competing ownership.

Render and inspection read the committed SQLite index, avoiding repeated JSON decoding; Sync touches only each page rather than repeatedly loading all files and rewriting unrelated rows. Rebuild reconstructs the index from authoritative files. An interrupted page is repaired by rerunning Sync; out-of-band file edits require Rebuild. File discovery skips dotfiles and warns about unexpected entries; canonical malformed records still fail rebuild. Every Session Day exposes the same `session.led_by`; existing content comparison handles changed notes. User-owned templates decide how to present authorship.

Contract and configuration: [Session Lead Feed v1](../session-lead-feed.md). Backups include `leads/` and `lead-feeds/` alongside existing durable inputs.
