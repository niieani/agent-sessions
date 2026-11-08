# ChatGPT imports and collector integration

[Back to README](../README.md)

ChatGPT collection is explicit. The CLI imports export ZIPs or reads a native capture directory written by an external browser collector. It does not authenticate to ChatGPT or operate a browser.

## Import an export

Start with a separate Archive and output directory so you can inspect the results before combining data:

```sh
agent-sessions chatgpt ingest-export --archive ./chatgpt-archive --zip /path/to/export.zip --account-id ACCOUNT
agent-sessions chatgpt status --archive ./chatgpt-archive
agent-sessions render --archive ./chatgpt-archive
```

Replace `ACCOUNT` with the stable account identifier associated with the export. Export ingestion streams ZIP shards and retains native evidence. Keep the original export and back up the Archive; SQLite and generated Views are rebuildable.

## Browser capture Stores

Configure `chatgpt-root` on a Host, or use the corresponding Pull override for a synthesized local Host:

```sh
agent-sessions pull --archive ./chatgpt-archive --chatgpt-root /path/to/capture-store
```

A producer writes native response bytes with `url`, `sha256`, `capturedAt` and stable `accountId` metadata. Account display names are not stable identities. Collection must finish before an automated Pull; producers should not perform ingestion or rendering themselves. Website credentials belong to the collector, not the Archive.

`chatgpt status` exposes the versioned `asessions-chatgpt-status-v1` contract: known Conversations, revision boundaries, Messages, completeness and media retry information. Collectors must check the version before interpreting fields. Preserving a response does not establish that all account history was collected.

Repeated ingestion is idempotent. Native revisions and capture timestamps order evidence; partial snapshots retain earlier unique Messages. A missing Project directory response for a referenced Project is an explicit extraction error, so collect that evidence before rebuilding.

## Transcripts, branches and Projects

Hidden reasoning and memory are excluded from Dialog; voice Messages retain transcription text. Canonical text retains citations, with readable references in rendered output. Hosted Transcripts have no Origin Host; the collecting machine remains Capture Provenance.

Forks place shared Dialog in Shared Segment files and retain branch-specific tails with navigation links. Opening-message edits create independent Sessions. Only Path Ends receive Digests; navigation-only changes do not regenerate them. `views.layout.branch` controls branch placement.

Native Projects use their names when unique. Name collisions receive deterministic suffixes, and Name Tables can map native identities:

```toml
[project-names]
"chatgpt:g-p-00000000000000000000000000000001" = "Example Project"
```

The identity must be `g-p-` followed by 32 lowercase hexadecimal digits. Configure output and naming through the same View settings used by other Sources.

## Media and verification

Session Media is content-addressed under `media/objects/`; Views refer to stable `media/sha256/` paths. After rendering, materialize the media links:

```sh
agent-sessions materialize --archive ./chatgpt-archive --method copy
```

Other methods are `clonefile`, `hardlink` and `symlink`; configuration can supply a default. Materialization does not rewrite Transcript text. Back up `media/` with Captures. Bundles include Session Media, and missing originals remain explicit references.

`render --json` emits `asessions-render-v1`, including per-file hashes and `form_only` flags for presentation-only changes. Unknown or externally edited output counts as a content change. The [specification](../SPEC.md) defines the exact Dialog, media and rendering contracts; [import verification](chatgpt-staging.md) describes isolated ingestion checks.
