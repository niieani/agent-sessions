# ChatGPT staging acceptance

Use a disposable staging directory with separate `inputs/`, `archive/`, `output/` and `reports/` directories. Keep private exports and native captures outside version control. Set every output destination explicitly before collecting or rendering.

## Ingest, rebuild and render

```sh
agent-sessions chatgpt ingest-export --archive <staging>/archive --zip <staging>/inputs/archive-export.zip
agent-sessions chatgpt ingest-browser --archive <staging>/archive --input <staging>/inputs/native-captures
agent-sessions chatgpt status --archive <staging>/archive
agent-sessions rebuild --archive <staging>/archive
agent-sessions render --json --archive <staging>/archive
```

Rendering uses the existing engine. Set `[views].output` explicitly in `<staging>/archive/config.toml`. The staging configuration copies the existing Vault Template and uses its Workday definition (`America/Los_Angeles`, `03:00`, `session-day`). Filenames use the shared `native_title` and `short_id` variables because these historical Conversations have native titles but no model-generated Digests. This does not invent a Digest or lock a native title. Digest/title generation and production promotion are separate operations.

The Adapter interprets export graphs and website pages into ordinary canonical Conversations plus source-neutral Branch Segments. The selected path stays indexed for search/Digest; per-Host branch projections follow the same elected Capture. Shared rendering emits each segment-owned Message once, with the normal `<user>` / `<assistant>` wrappers, file-only continuation/back/root links, and Workday attribution inherited from the actual user turn. Branches are not Subagents. Internal graph-only stretches are coalesced/forwarded; native Message IDs stay internal. Captures remain exact, additive evidence.

Regression coverage includes nested and empty/internal forks, cross-Workday continuations, resolving file links, unchanged linear-source JSON contexts, Projection-winner refresh, Rebuild, source number precision, native-byte hashes, media aliases and source-neutral native-title layouts. Detailed run results belong in staging `reports/`, not this current-state contract.

## Collection boundaries and media retries

Modified older Conversations return to recents and are included by listing revision; there is no recurring full-export dependency. Discovery stops at the export/last successful-run boundary with precision overlap. Website listing revisions and detail revisions remain distinct.

Unchanged complete Conversations skip. A changed existing Conversation audits older selected pages for new version markers and refreshes known Version targets recursively. The website exposes markers per page, not a verified complete global fork inventory: a known selected Message alone cannot prove that a new older alternate fork is absent. Page bodies go directly to native files; shared prefixes remain deduplicated in Views.

Transcript progress requires complete discovery/collection, durable inputs, successful ingestion/rendering and a durable media retry queue. Missing image downloads never block an otherwise successful transcript Bookmark. Media-only retries preserve transcript progress and do not repaginate unchanged dialogue. The producer owns scheduling and queues; account/session credentials never enter this Archive. Authentication/source failures stay explicit.

## Native rich content and SVG

Citation markers/components resolve to ordinary Markdown source links from captured metadata. Supported native content structures and member bindings resolve generically; there is no price/product-specific conversion. Incomplete or unsupported structures become links to inert native JSON, with interpretation issues recorded there. `assets/chatgpt/<conversation>/` preserves complete source Messages: code, constants, app data, widgets, fallback Markdown and structured parts. No supplied JavaScript is executed. A source response with missing bindings remains incomplete; a blank fallback is not evidence of a captured value.

Complete SVG illustrations outside code examples become separate sanitized image assets embedded in the Transcript; native JSON retains the complete original. Executable and external SVG content is excluded from the displayed projection. Unsupported/incomplete SVG remains linked as source evidence. Fenced/inline source-code examples and user Message text are not interpreted as components. Interactive playback is not required for preservation.

## Generated-image originals

Export ingestion examines `library_files.json` and preserves original rows as Capture evidence. Only explicitly generated outputs are eligible; user-uploaded bytes stay outside collection. File-ID ZIP members are streamed and checked against declared byte lengths, MIME when present, and PNG/JPEG/WebP signatures. Missing members remain references, never fabricated files.

```sh
agent-sessions chatgpt ingest-export-media --archive <staging>/archive --zip <staging>/inputs/archive-export.zip
```

`ingest-export` includes that step; the separate command can process media without replaying conversation shards. Native companion files also preserve asset IDs, provenance, hashes, lengths and quality. Shared RenderAssets resolve saved media aliases to images and missing assets to native descriptor links, so generated links remain navigable. Confirmed originals outrank observed previews; a preview does not establish original-media completeness.

Status exposes generated/saved/missing IDs, full generated asset descriptors, known Version targets, current streaming IDs, missing advertised Versions and transcript checkpoint eligibility. Same-ID Message content follows observation chronology; equal-time selected pages outrank older Versions. Historical intermediate progress does not block a finished current response. Exact same-revision export sibling proof can cover an advertised Version already preserved locally; missing/different numeric revisions cannot establish that proof.

## Deployment acceptance

The extension/native collector needs a one-time reload after deployment. Its protocol handshake rejects an outdated installed client explicitly instead of letting it hang or certify success. The overnight acceptance target is a logged-in Mac with screen locked and display off, including browser restart/resume; screen unlocking is not a nightly step. Direct file-ID media resolution and cold/locked browser acceptance remain pending live verification. Existing DOM-based media lookup must not be described as a verified direct resolver.

Back up `captures/`, `chatgpt-evidence/`, `chatgpt-media/`, configuration and reusable inputs before promotion. SQLite and Views rebuild locally. Production import uses the same original inputs against explicitly approved destinations; do not move the staging database into production.
