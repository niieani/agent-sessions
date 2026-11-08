# Session Lead Feed v1

A tool-neutral append-only feed declaring Sessions led by agents. Producers publish native Session identities; consumers match exactly on `(source, sessionId)`. `host` is evidence, not part of identity. A lead can precede the archived Session. This protocol neither changes Conversation roles nor declares a Subagent relationship.

## Request

`GET <feed-url>?after=<cursor>&limit=<n>`

- `after`: optional opaque cursor, URL-encoded as a query value. Omitted means from the beginning. Return entries strictly after it in feed order.
- `limit`: integer 1–500 inclusive; default 200. Return at most this many entries.
- Producers must ignore unknown query parameters for forward compatibility.
- Optional authentication: `Authorization: Bearer <token>`.

Consumers must not interpret cursor values, compare them lexically or numerically, or construct a cursor. Strict increase means the producer's feed order, not string order. Cursors must remain valid across producer restarts and identify stable positions. A producer unable to honor an existing cursor returns 400; it must not silently restart the feed.

## Successful response

HTTP 200, JSON:

```json
{
  "version": 1,
  "leads": [
    {
      "cursor": "opaque-position-1",
      "source": "codex",
      "sessionId": "019f-example-native-id",
      "ledBy": { "kind": "agent", "name": "Coordinator" },
      "host": "workstation",
      "origin": { "kind": "service", "jobId": "job-42" },
      "recordedAt": "2026-10-08T12:00:00Z"
    },
    {
      "cursor": "opaque-position-2",
      "source": "claude",
      "sessionId": "example-native-id",
      "ledBy": { "kind": "agent" },
      "recordedAt": "2026-10-08T12:01:00+00:00"
    }
  ],
  "nextCursor": "opaque-position-2",
  "more": false
}
```

Required fields: `version`, `leads`, `nextCursor`, `more`; each lead requires `cursor`, `source`, `sessionId`, `ledBy`, `recordedAt`.

- `version`: integer, exactly 1.
- `cursor`: nonempty string, strictly increasing in producer feed order, unique per feed entry.
- `source`: exactly `codex` or `claude`, the asessions Source names.
- `sessionId`: nonempty native Session identity, never an Orchestrator Thread id or archive filename. No control characters.
- `ledBy.kind`: exactly `agent`. `ledBy.name`: nonempty string present for a named agent; absent for an anonymous agent. Do not send null or an empty name.
- `host`: optional nonempty string, hostname as the machine reports it. Consumers must not use it to restrict matching.
- `origin`: optional object with a nonempty string `kind` and any additional nonempty string-valued fields. Its keys and values remain producer-defined provenance, except the optional well-known fields below which consumers may interpret.
- `recordedAt`: RFC 3339 string, time the producer recorded or re-emitted the declaration. Declaration replacement metadata only: consumers must not use it for temporal attribution, Session Day selection, or any other temporal behavior. Replacement order is feed order, never timestamp order. Origin enrichment or a later run may re-emit a lead with a new `recordedAt` for work requested much earlier.
- `nextCursor`: the last returned lead's cursor on a nonempty page. On an empty page, preserve `after`; null only when no cursor was requested and no entry exists.
- `more`: boolean; true means another page exists. A page with `more: true` must be nonempty and advance the cursor. False means caught up at the time of this response; later requests may return new entries.

Optional fields are omitted, not null. Consumers must ignore unknown response fields in the envelope, lead, and `ledBy` objects for forward compatibility. `origin` intentionally permits additional string fields; all its values must be nonempty. Known fields remain strictly validated, and unknown versions remain errors.

### Optional well-known Origin fields

- `requestedBy`: `"<source>:<native-session-id>"`, identifying the Session whose agent requested this Session. The source prefix and native id are nonempty strings; the id has no control characters. The source prefix is producer-defined, with no consumer source allow-list. Split at the first colon; native ids may themselves contain colons. This is exact identity evidence, independent of `host`, and does not declare a native Subagent relationship. Malformed values are lead-validation errors identifying the cursor and `origin.requestedBy`. A well-formed identity with an unknown source or Session is valid and remains unresolved; it must not stop Sync or make stored leads fail Rebuild, Render, List, or Show.
- `t3ThreadId`: nonempty T3 Orchestrator Thread id. For `origin.kind == "t3"`, consumers may combine it with the producer's nonempty `machine` field to form a `"<machine>:<t3ThreadId>"` reference. It never replaces `sessionId` or identifies the Requester Session.

All other Origin fields remain producer-defined nonempty strings. Example Assistant's T3 Origin also carries `machine`, `providerThreadId`, `runState`, and optionally `latestRunId`; consumers preserve them without inferring Session relationships.

A later lead for the same `(source, sessionId)` replaces the earlier one, including removing a previously present name, host, or origin. Leads are never deleted. The feed must retain its entries and their order so replay reproduces the current declarations. An empty result is not a deletion.

## Errors and consumer obligations

HTTP 400 for malformed or unknown `after`, or invalid `limit`:

```json
{ "error": { "code": "invalid_cursor", "message": "Unknown cursor" } }
```

`code` and `message` are required strings; codes are producer-defined. Authentication and server failures may use other non-200 HTTP statuses. Consumers fail loudly on every non-200 status, unknown version, malformed JSON, invalid fields, repeated cursor, stalled pagination, inconsistent nextCursor, or a page exceeding the requested limit. Do not silently discard entries, reset a Bookmark, or treat failure as caught up. Lead validation failures identify the offending cursor and field. Opaque cursor ordering beyond duplicates cannot be verified by a consumer; maintaining it is the producer's responsibility.

A consumer validates a whole page before publishing any lead from it. Publish durable lead files atomically, make their index reconstructible, then atomically advance the per-feed Bookmark. An interruption before Bookmark publication leaves that page eligible for replay. Reprocessing a page must be safe and idempotent. Previously committed pages remain committed if a later request fails.

## asessions consumer

Configure explicitly; there are no built-in feed URLs, agent names, or credentials:

```toml
[[lead-feeds]]
name = "coordinator"
url = "https://agents.example.org/session-leads"
token-file = "secrets/lead-feed-token" # optional; relative to Archive or absolute
```

Use HTTP(S) endpoint URLs without query parameters or fragments. Redirects fail explicitly. Token files contain a single nonempty bearer token, with surrounding whitespace trimmed. Feed names are nonempty and unique. Bookmarks bind a feed name to its URL; use a new feed name when changing an endpoint.

```bash
agent-sessions leads sync --archive /path/to/archive
agent-sessions leads sync --archive /path/to/archive --feed coordinator
agent-sessions leads list --archive /path/to/archive
agent-sessions leads show claude:native-id --archive /path/to/archive
agent-sessions render --archive /path/to/archive
```

Sync requests 200 entries per page until `more` is false. Multiple feeds sync in configuration order; if independent feeds declare the same Session, the most recently imported declaration wins. Cursor values from different feeds are never compared. Producers should avoid competing ownership of one Session.

Files at `leads/<source>/<identity-component>.json` store the complete latest lead in the wire shape; unsafe native IDs use the Archive's path-safe identity encoding. `lead-feeds/<identity-component>.json` stores `{ "version": 1, "url": "...", "cursor": "..." }` per configured feed name. Include both directories in backups. SQLite indexes all leads, including pending Sessions, and rebuild restores them without contacting feeds. Sync holds the Archive writer lock and upserts only each validated page into SQLite before advancing its Bookmark. Render, List, and Show read the committed index; rebuild alone scans durable lead files to reconstruct it. Rerun Sync after an interrupted page, or Rebuild after out-of-band file edits. If you remove a feed from config after its index refresh failed, run `agent-sessions rebuild --archive <dir>`: the page already has durable lead files, but that feed can no longer replay them to repair the index. Rebuild restores the index from all durable declarations; removing a feed does not delete its archived leads. Dotfiles are ignored. Other unexpected entries are skipped with warnings during rebuild and `leads list`; malformed canonical lead JSON still fails rebuild explicitly.

Template context, including machine-readable JSON Views:

- `session.led_by = null` when no declaration exists.
- `session.led_by = { "kind": "agent" }` for an anonymous agent.
- `session.led_by = { "kind": "agent", "name": "Coordinator" }` for a named agent.

No name is invented. Every Session Day of that Session gets the same lead. Templates decide authorship presentation; existing Subagent classification and Message roles remain independent. Run Render after Sync; content comparison rewrites affected notes and preserves unchanged notes. User-owned templates are never edited by Sync.

When Origin is present, `session.led_by.origin` is its complete string map (including `kind`); when absent, the `origin` key is absent. `session.requested_by` resolves separately for each delegated Session Day, anchored on that note's Workday under the configured timezone and day-start. Select the Requester Session's rendered Session Day on the same Workday; if none is rendered, select its latest rendered Session Day before that Workday. If neither exists, expose null. Never select a future requester day. Only targets in the current render plan qualify, including after layout and Digest gates. Lead `recordedAt` never participates in this selection, so re-emitting a lead cannot change the link while Session evidence and rendered targets remain unchanged. A requester naming the lead's own Session stays unresolved, even when another day of that Session is rendered. Session-level Views cannot supply a Session Day target. Unknown identities, missing Captures, omitted requester Sessions, or no rendered requester day on or before the delegated Workday stay unresolved without an error. A same-day note awaiting its Digest may link to an earlier rendered requester day; once the same-day note is rendered, the next Render selects it instead. Unchanged rerenders preserve bytes and mtimes.

Resolved `session.requested_by` exposes `stem`, relative `path`, `workday`, nullable `title`, `source`, and `native_id`, using the collision-resolved note path like `session.subagent.parent_day`. Bundled templates use:

```jinja
{% if session.requested_by %}
session-requested-by: {{ ("[[" ~ session.requested_by.stem ~ "]]") | yaml }}
{% endif %}
{% if session.led_by and session.led_by.origin is defined and session.led_by.origin.kind == "t3" and session.led_by.origin.machine is defined and session.led_by.origin.t3ThreadId is defined %}
session-t3-thread: {{ (session.led_by.origin.machine ~ ":" ~ session.led_by.origin.t3ThreadId) | yaml }}
{% endif %}
```

Rebuild restores archive state/indexes only; it never writes Views. Run Render explicitly under any output lock held by the caller; asessions does not manage that external lock.
