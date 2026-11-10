# Agent Sessions

A durable, tool-independent archive of the conversations a user has with AI agents, collected from every machine they work on. Native agent tools keep their own ephemeral session stores; this project captures them verbatim and extracts the conversation — the part that matters — into a stable canonical form.

## Language

### Sources & ingestion

**Source**:
One supported native producer whose Store contributes Sessions or Augmentations to the Archive. A Source may be an agent vendor (Claude Code, Codex, Cursor, VS Code, Pi), an Orchestrator (T3 Code), or a Hosted Source (ChatGPT).
_Avoid_: agent, vendor, tool, client

**Hosted Source**:
A Source whose Sessions run on a remote service rather than on a Host. A Host still contributes its Captures, but never originates its Sessions.
_Avoid_: cloud source, web source

**Store**:
The on-disk location where a Source's own application keeps its native Session or Orchestrator Thread data. Ephemeral and owned by that application — read-only except for explicit Title Write-back.
_Avoid_: source (for the location), root, database

**Store Retention**:
How long a Source's own application keeps Session data in its Store before deleting it. Claude Code's default is thirty days. Once a Store has deleted a Session, the Archive's Capture is the only copy.
_Avoid_: cleanup (the application's word), expiry, pruning

**Adapter**:
The component that understands one Source's native format. Exactly one per Source; the only code that ever sees native formats.
_Avoid_: parser, scanner, connector

**Ingest**:
The idempotent, incremental act of pulling new or updated native archival units from a Store into the Archive. Re-ingesting an unchanged Store is a no-op; re-ingesting a changed Session or Orchestrator Thread updates its same-identity record.
_Avoid_: scan, discover, import, sync (for this act)

**Enumerate**:
The cheap first phase of ingestion: an Adapter lists candidate Sessions or Orchestrator Threads using only metadata (paths, timestamps, index queries), without reading their content.
_Avoid_: list, glob

**Capture** (verb and noun):
Preserving one native archival unit's payload verbatim, and the preserved payload itself. The unit is normally a Session; an Orchestrator Source uses one Orchestrator Thread. A Capture is schema-free — new native event types cost nothing and require no code changes to keep.
_Avoid_: raw, original, sidecar, backup (as names for the artifact)

**Capture Exclusion**:
An explicit `[pull] exclude-paths` working-path glob that prevents a native archival unit from entering the Archive and derived index. Independent of Kind omission. Existing matching Captures and Digests remain durable but leave the index on the next Pull/Rebuild; Bookmarks receive no contribution from them.

**Native Copy**:
A same-Host native file sharing Source and native Session id with another file. Pull/Export elect the lexicographically smallest eligible absolute path before Bookmark filtering; Native Copy location is not Session identity.

**Prior Revision**:
A Capture body that a diverged incoming payload replaced, kept verbatim beside the current Capture with its original Provenance. It is preserved evidence only: never Enumerated, Extracted, or rebuilt.
_Avoid_: backup, old capture, history

**Extract**:
Producing a Conversation or Orchestrator facts from a Capture. Re-runnable at any time — a smarter Adapter can re-extract old Captures without re-reading the Store.
_Avoid_: parse, hydrate, normalize

**Export**:
The remote-side act of producing a Bundle of everything newer than a given Bookmark.
_Avoid_: dump, send

**Bundle**:
The self-contained unit a Host exports for pulling: Captures plus a manifest.
_Avoid_: batch, archive (reserved for the Archive), payload

**Pull**:
The central act of collecting Bundles from the fleet's Hosts and ingesting them into the Archive.
_Avoid_: sync, fetch, push

**Bookmark**:
The per-Host, per-Source record of how far ingestion has progressed, used to skip already-ingested data on the next run.
_Avoid_: watermark, cursor (collides with the Cursor Source), checkpoint

**Host**:
A machine contributing a Capture of a Session or Orchestrator Thread. Capture presence does not prove origin. Part of Provenance; the fleet is the set of Hosts feeding one Archive.
_Avoid_: machine, node, computer

**Unreachable Host**:
A Host that ssh could not connect to on a Pull's first contact (connection timeout, refusal, no route, host down, or name resolution), so no remote command ran. Skipped without failing the Pull and without touching its Bookmarks; a Host rejected after connecting (authentication, host key) or lost mid-Pull has failed instead.
_Avoid_: offline host, down host, failed host (for this case)

**Unreachable Grace**:
How long after a Host's last successful Pull it may stay an Unreachable Host before Pull fails on it. A Host with no recorded success is measured from when the Archive first tracked it. Optional `[[host]] unreachable-grace` overrides `[pull] unreachable-grace` (default `7d`); both use integer durations with `ms`, `s`, `m`, `h`, or `d`. Effective grace appears in the Unreachable Host report. Host Pull Health survives Rebuild.
_Avoid_: timeout, staleness window

**Host Pull Health**:
The durable first-tracked and latest successful Pull times per Host, stored in `host-pulls.json` and indexed in SQLite. A successful Pull completes all Sources and Repository Resolution without failure. Neither time can be reconstructed from Captures.

### Canonical model

**Archive**:
The durable, tool-independent system of record, fed by every Host. Holds Captures as verbatim truth, Conversations as the canonical product, and resolved Augmentations that add provenance-bearing Orchestrator facts to Conversations.
_Avoid_: output, batch, export

**Session**:
One recorded interaction with an agent, as a unit of identity: its Provenance, its Capture, and its extracted Conversation.
_Avoid_: composer, chat

**Conversation**:
The canonical extracted exchange for a Session: the user's Messages, Agent Messages, the assistant's responses, and Subagent links — in that order of importance. Deliberately narrow and stable; Actions are not part of it.
_Avoid_: dialog (as the artifact), transcript (for the record itself), simplified session

**Message**:
One turn element within a Conversation: a role, text content, and a timestamp.

**Agent Message**:
A task instruction authored by another agent and delivered as the initial content of an agent-created Session. It is distinct from a user Message, assistant response, and Parent Instruction; it renders as `<agent>` and starts a Workday, but does not participate in user-Message View matching.
_Avoid_: user message, parent instruction, tool result

**Action**:
Something the agent did rather than said — a tool call, its parameters, its status, its result payload. Excluded from Conversations by default; recoverable from the Capture when needed.
_Avoid_: tool message, event, activity

**Provenance**:
The native identity a Session or Orchestrator Thread carries forever: its Source, Host, native id, and native location. What makes a record traceable back to where it came from.
_Avoid_: sourceId/sourcePath (as bare names)

**Projection winner**:
The one Host Capture elected to supply a canonical Session's Conversation or Orchestrator Thread's facts when the same Source/native id is present on several Hosts. Each settled Capture replaces that Host's regenerable projection; election is deterministic from Source-specific Capture revisions and exact ties use Host. Every other Host remains recorded as Provenance. A Hosted Source may first aggregate native evidence across contributing Captures, so election does not discard parts of its Conversation.
_Avoid_: latest Host, last writer, merged Conversation

**Origin Host**:
The Host evidenced to have originated a Session, independently of which Host's Capture supplies its current Conversation. Multiple contributing Hosts with no decisive evidence do not establish origin. A Hosted Source's Sessions have none; a contributing Host is never presented as an origin it was not shown to be.

**Host Attribution**:
A derived decision crediting a Session to a Host. Capture sidecars supply native birth times; current T3 projections supply secondary execution evidence. Confirmed attribution has one evidenced Origin Host; ambiguous attribution retains the previous Host through a durable Host preference containing no evidence claims. A sole unevidenced Capture Host is unverified. Rebuild reconstructs attribution without reading the old database.

**Host Preference**:
A durable Source/native identity and last chosen Host, with no evidence claims. It preserves an ambiguous Host Attribution across Rebuild. Decisive birth evidence reproduces origin without a Host Preference.

**Copy Host**:
A contributing Host other than a confirmed Origin Host. Its Capture preserves evidence but does not create another Conversation, Digest, Session Day, or Session Lead identity.

**Subagent**:
A child Session spawned from within a parent Session. The relationship can be known independently from either perspective and is resolved bidirectionally: the parent knows which Subagent it spawned, and the Subagent knows its parent. User-role Messages in a Subagent Conversation are Parent Instructions, not end-user Dialog.
_Avoid_: task, child (alone)

**Parent Instruction**:
A Message sent from a parent Session into its Subagent. Rendered distinctly from an end-user Message even when the Source records both with the same role.

**Composer Prompt**:
A user Message as a vendor client sends it: generated context sections, then a request heading (Codex: `## My request for Codex:` or `## My request:`), then what the user typed. Extract keeps the typed request; context sections the user chose (selected text, response annotations, referenced chats) stay with it verbatim.
_Avoid_: preamble (for the whole Message), wrapped prompt

**Machine Context**:
Context sections a client generates rather than the user writing them: attachment and application labels, and ambient UI state such as the open browser tab or IDE files. Never Dialog — except that a Message whose only content is Machine Context keeps it rather than vanishing.
_Avoid_: noise, metadata (alone), injected context (reserved for tagged agent configuration)

**Dialog**:
The property of containing at least one non-empty user or assistant Message. Sessions without Dialog are still archived, but flagged so views can exclude them.

### Orchestration

**Orchestrator**:
A client that owns an interaction container while delegating agent execution to one or more underlying Source Sessions. T3 Code is the first supported Orchestrator.
_Avoid_: agent, provider, wrapper, frontend

**Orchestrator Thread**:
An Orchestrator-owned interaction container with its own identity, messages, and metadata. It is not a Session and cannot replace or redefine the identity of an underlying Session.
_Avoid_: session, conversation, chat

**Provider Thread**:
An Orchestrator's record of one provider-owned interaction, distinct from both its Orchestrator Thread and a provider process shared across interactions. Its proven native identity identifies the underlying Session.
_Avoid_: provider session (for the interaction), runtime (for the identity)

**Orchestrator Link**:
A provenance-bearing relation between an Orchestrator Thread and an underlying Session, resolved only from exact native provider identity evidence. An unresolved or ambiguous relation remains preserved as a fact and contributes no Augmentation.
_Avoid_: alias, guessed match, inferred session

**Recovered Session**:
A Session whose Store no longer holds it, rebuilt from the Dialog an Orchestrator Thread still projects for it. It keeps the provider as its Source and carries provenance naming the Orchestrator it came through. A real Capture always wins: if the Store ever yields the Session again, the recovery is superseded and the Orchestrator Thread goes back to contributing Augmentation only.
_Avoid_: fallback session, T3 session, synthetic session

**Augmentation**:
Facts from a resolved Orchestrator Thread attached to an underlying Session without changing that Session's identity, Projection winner, or Source-owned Conversation. Augmentation is additive and provenance-bearing; it never silently substitutes Orchestrator Dialog for Source Dialog.
_Avoid_: overlay, merge, replacement, fallback

### Media

**Session Media**:
Binary content (images, files) belonging to a Session's Conversation, stored once in the Archive under its content hash and preserved like a Capture.
_Avoid_: attachment, asset (for the durable bytes)

**Media Materialization**:
Making Session Media available beside Views at stable content-addressed paths, by whichever per-machine method suits the filesystem. Views reference those paths and never change when the method does.
_Avoid_: asset copy, export

### Digests

**Digest**:
A model-written account of one unit of Dialog: a title, the Session's Intent, a summary proportional to how much happened, a TL;DR when the summary is long, and a Classification of the kind of work. Derived from a Conversation, paid for once, and kept as durable data rather than regenerated.
_Avoid_: summary (as the artifact), analysis, annotation, description

**Topic**:
The short, stable subject a Session's first Digest chooses for the whole Session, shared across its Session Days.
_Avoid_: theme, subject line, session name

**Intent**:
What the user set out to achieve in a Session, in the user's own direction rather than the agent's plan. The first Digest establishes it from the user's opening turns; later Session Days carry it forward and change it only when the user restates or changes direction. Each Session Day's Digest records the Intent in effect that day.
_Avoid_: goal (a Goal is a Codex Message kind), objective, plan, purpose

**Facet**:
What sets one Session Day apart within its Session's Topic. A Session Day's title is `<Topic> - <facet>`, or the Topic alone when the day has no facet. A facet never repeats the Topic.
_Avoid_: subtitle, day title (for the facet alone)

**Locked Title**:
A Session Day's title fixed for naming its rendered file; ordinary Digest updates leave it unchanged. Replacing it requires an explicit Retitle or Digest repair.
_Avoid_: filename, slug, permanent title

**Session Title**:
The very short model-written name for a whole Session, seeded from its Topic and refreshed as Session Days accrue. A failed refresh preserves the prior Session Digest and remains pending for retry without regenerating fresh Day Digests. What Title Write-back pushes.
_Avoid_: native title, thread name (for the archive-side artifact)

**Classification**:
The closed vocabulary naming the kind of work in a Session Day — build, fix, research, qa, test, write, plan, ops, automation — assigned by the Digest with the primary first, or pinned by a Kind.
_Avoid_: category, label, tag, type

**Dialog Fingerprint**:
The hash of a Session Day's Dialog path stored in its Digest; the Digest is fresh while the fingerprint and covered range still match. A Fingerprint Backfill adopts the current fingerprint into an older Digest whose covered range still matches, with no model call.
_Avoid_: checksum, content hash, freshness key

**Rejected Digest**:
A Digest the model produced that the tool refused to store because it failed to parse, violated the schema, or broke a rule. Kept verbatim beside the Session Day's Digest, with the reason and the time of the attempt, so a bad batch can be diagnosed later; never rendered.
_Avoid_: rejected response, failure (the run's count), error (the message)

**Digest Status**:
The live record of the latest digest or repair-digests run in `digest-status.json`: its planned items (Session Days and Session titles) by Source, how many have settled or failed, the item in flight, and an ETA from the recent rate. A run still marked running whose process no longer holds the status lock is stale.
_Avoid_: progress file, heartbeat, run log

**Capacity Wait**:
The pause a Digest run enters when the model server answers HTTP 507 because it cannot load the model. All workers stop sending; one request is retried per interval until capacity returns or the wait runs out, which ends the run with the remaining items left for the next run rather than failed.
_Avoid_: backoff (per-item), outage, retry (a Rejected Digest's correction)

**Redo**:
Model work that regenerates an existing Digest — a Session Day Digest whose covered range or Dialog Fingerprint no longer matches (or is forced), or an existing Session title refreshed after its Session Days change. Opposed to **new** work, which creates a Digest that did not exist. A dry-run counts both, and many redone Session Days after an upgrade mean the release is invalidating existing Digests.
_Avoid_: re-digest (as a count), stale (a Session Day's state, not the work), migration

**Native Name**:
The name a Store carries for a Session or Orchestrator Thread in its own name field. Empty for most older Sessions; harnesses that auto-name threads fill it from the first turn.
_Avoid_: title (Codex's `title` column is the first user message, not a name), label, summary

**Pushed Name**:
The Native Name that Title Write-back last wrote for a Session or linked Orchestrator Thread, remembered in the Session's Digest (keyed separately by Orchestrator Thread id) so the tool recognises its own earlier work.
_Avoid_: owned name, our title

**External Name**:
A Native Name that Title Write-back did not write: chosen by a person, or generated by the harness itself. The Store records no origin, so the two cannot be told apart. Kept untouched unless the Source is configured to overwrite External Names.
_Avoid_: foreign, protected, user-chosen (the origin is unknown)

**Title Write-back**:
The one sanctioned write to a native Store: pushing a Session Title into the Store's own name field through an explicit command. Each targeted Session or linked Orchestrator Thread gets one Disposition: **Push** (empty Native Name, or the Pushed Name), **Unchanged** (already equal), **Keep** (External Name on a Source that does not overwrite), or **Missing** (the Store no longer holds the Session).
_Avoid_: sync, push (bare), rename, foreign

### Views

**Repository Resolution**:
A durable observation of a recorded working directory on its Host: a Git origin,
an existing path without an origin, or a missing directory. Exact T3 Project
evidence can resolve a missing workspace. Successful host/path answers persist
across failed lookups and Rebuild. Settled observations are reused until the Capture revision changes; later T3
evidence can resolve a missing workspace without repeating its Git lookup. T3
repository evidence prefers the origin identity over an upstream canonical key.
It is separate from Source-recorded Git facts.
A missing workspace with a recorded branch and no repository is unresolved and
renders Loose while retaining its original path.


**Project**:
A durable grouping of Sessions associated with the same work. A verified Native Project supplies Source-owned grouping; otherwise a normalized `owner/repo` repository key groups recorded worktrees and checkouts together, or the recorded workspace path supplies the grouping. An explicit Project Assignment takes precedence. Name Tables select display names by exact recorded path, then Native Project identity or repository key. Project membership is a View classification, not part of Session identity.
_Avoid_: workspace (for the grouping), folder, repository

**Native Project**:
A Source-owned Project identity, its observed display name, and a Session’s recorded membership. Its identity survives a rename and distinguishes equally named Projects. It does not imply a filesystem workspace or an explicit Project Assignment.
_Avoid_: project path (for the native identity), custom GPT (for a verified ChatGPT Project)

**Project Assignment**:
An explicit association of one Session with a Project, independent of its recorded workspace. It applies across that Session’s Session Days and supplies inherited project context to its Subagents.
_Avoid_: project override, manual project, recovered project

**Dialog Statistics**:
The user and assistant Message counts and character totals of one complete Conversation. They describe the Session’s current Dialog, independent of Session Day boundaries and Subagent Conversations.
_Avoid_: importance score, eligibility, activity metrics

**Kind**:
A named, user-configured View classification: match conditions over a Session's recorded working path, Source, first user Message, or Dialog Statistics, together with the Treatments applied to the Sessions it claims. Kinds are ordered; the first matching Kind claims a top-level Session and its Subagent children (an orphaned child is evaluated directly). A Session claimed by no Kind renders normally under its Project, or Loose without one.
_Avoid_: rule, route, filter, category, class

**Treatment**:
What a claiming Kind applies to a Session: a placement (Omitted, Loose, or a configured output destination), Digest gating, a pinned Classification, and, in the future, rendering adjustments. Treatments shape Views and Digests only — Captures and Conversations are never affected.
_Avoid_: action (reserved for tool calls), effect, output kind

**Omitted**:
The placement Treatment that produces no Transcript and no index entry for a Session. Views-only and reversible: the Capture and Conversation are untouched, and a change in the Conversation or Kind can restore the Transcript on a later Render.
_Avoid_: hidden, excluded, skipped, suppressed, ignored

**Loose**:
The View classification for a Session that belongs to no Project, because no working path was recorded, because Repository Resolution marks a missing Git workspace unresolved, or because a Kind places it Loose. Loose Sessions render chronologically by month, never as a fake Project; the original working path, if any, remains part of the Conversation. Formerly "Unfiled".
_Avoid_: unfiled, unknown project, miscellaneous project, fake project, stray

**Workday**:
The unit of "a day of work" used by Views and activity queries: a local date in a configured timezone, beginning at a configurable day-start time rather than necessarily at midnight. A user or Agent Message starts a turn and owns the Workday; following assistant Messages inherit it. Not a calendar day and never derived from Capture file times.
_Avoid_: day, date, calendar day (when this configurable unit is meant)

**Session Day**:
The portion of one Session attributed to one Workday: every turn whose user Message falls in that Workday, with the assistant Messages that turn owns, even when they run past the boundary.
_Avoid_: day slice, segment, chunk, daily transcript

**Ongoing**:
The state of a Session Day whose last turn may still have been running when its Capture was taken: a last Message within ten minutes of Capture; for Host-run Sources, trailing Actions after the last Message also establish this state. Cleared by a later Capture that shows more.
_Avoid_: live, open, unfinished, running, in progress

**View**:
Any output rendered from the Archive — markdown transcripts, manifests, indexes. Regenerable and disposable by definition.
_Avoid_: simplified output, batch (as a product)

**Generated Reference**:
An Adapter-produced source URL or preserved artifact target with a captured display label. The shared renderer gives repeated targets one Markdown footnote per Transcript, independently of ordinary user-authored links. References are disposable presentation data reconstructed from Captures.
_Avoid_: citation alias, rich link

**Render**:
Producing a View from Conversations.
_Avoid_: simplify, write (for this act)

**Template**:
A user-owned file that renders one Session, Session Day, Subagent, or index from the render data model; the built-in default reproduces the standard Transcript.
_Avoid_: theme, format string, profile

**Layout**:
The configured path patterns deciding where each rendered file lands under the View output directory.
_Avoid_: folder structure, output pattern

**Name Table**:
A configured mapping from a raw Source, Host, or Project path/repository key to the display name Views show for it; unmapped values render raw. Entries come inline from `config.toml` or from Name Table files that an external tool generates; each key has exactly one origin.
_Avoid_: alias map, lookup, dictionary

**Transcript**:
The rendered markdown View of one Conversation, designed for agents to read first and humans second.
_Avoid_: markdown (as a noun for the artifact)

**Session Lead**:
An external declaration that an agent led one Session, matched by Source/native identity independently of Host, Message roles, and Subagent relationships. A named agent carries a name; an anonymous agent does not. Durable files preserve the latest declaration even before its Capture arrives. Producer Origin may name a Requester Session; this does not establish a Subagent relationship.
_Avoid_: author (for the declaration), session owner, human/agent role

**Requester Session**:
The Session whose agent requested another Session, declared by a Session Lead's `origin.requestedBy` exact Source/native identity. Each delegated Session Day links its Requester Session's rendered Session Day on the same Workday, otherwise its latest rendered day before that Workday; if neither exists, it stays unresolved. Lead `recordedAt` is never a temporal anchor. Unknown producer-defined sources and a request naming the same Session stay unresolved too.

**Session Lead Feed**:
A configured, tool-neutral append-only stream of Session Lead declarations. Its opaque per-feed Bookmark advances only after each page is durably published.

### Branches

**Conversation Branch**:
An alternate continuation sharing earlier Messages with another continuation of the same Session. Its native Message ancestry determines the shared prefix; it is not a Subagent or another Session.

**Branch Segment**:
A maximal sequence of Messages without a Fork in a Conversation. Each Segment owns its dialogue once and may lead to several alternate continuations. It shares Session identity and Provenance with the rest of its Conversation; it is not a Subagent or a separate Session.

**Fork**:
The point after a Message where a Conversation has more than one continuation. An edit of the opening user Message is not a Fork: each opening starts its own Session.
_Avoid_: branch point, split

**Shared Segment**:
A Branch Segment that ends at a Fork. It is the common context of every continuation after it and has no Digest of its own.
_Avoid_: prefix, root branch, trunk

**Path End**:
A Branch Segment that ends without a Fork, completing one path through the Conversation. Only Path Ends receive Digests, written with the whole path before them as context.
_Avoid_: leaf, tip, selected branch

**Input Token Count** — Tokens in a Digest model request after the configured model’s chat template and generation prefix are rendered. Includes correction Messages; measured locally, never inferred from character or byte length. Separate from the Dialog Fingerprint and Digest freshness.
