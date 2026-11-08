# Codex question reply recovery

The Native-format examples for this fix found question replies in 16 Sessions.
This VM has no private Archive; the fixtures are synthetic, using the supplied
native shapes. No private Capture was fetched.

## Recovery on the collection Host

After the eventual release is installed, normal Pull refreshes Codex Captures
whose native files still exist on reachable Hosts (Adapter revision 5 → 6).
This refresh is not sufficient for Capture-only Sessions: run once:

```bash
agent-sessions rebuild --archive /path/to/archive
agent-sessions render --archive /path/to/archive
```

Rebuild re-extracts retained Captures, including Sessions gone from their native
Store, without fetching private data or calling a model. It reconstructs the
whole index, so allow time for the full Archive. Digests remain durable inputs.
No package version, installation, or fleet deployment is part of this fix.

## RELEASING.md step 3 acceptance

The existing released Digest fixtures must stay fresh after unchanged Dialog is
re-extracted from the committed Codex question-replies fixtures. Their machine
tags, item-completed mirrors, and compacted histories do not change Dialog.
The restored fixture adds one user Message with two answers, and only that
Session becomes a Digest candidate. No prompt version, Digest schema, or Dialog
fingerprint algorithm changes.

On the collection Host, use an Archive clone and separate old/new binaries, before any
real Digest generation. Replace these example absolute paths:

```bash
cp -cR /path/to/archive /tmp/codex-reply-release-check
/path/to/0.4.12/agent-sessions digest --archive /tmp/codex-reply-release-check --dry-run --json > /tmp/codex-reply-before.json
/path/to/candidate/agent-sessions rebuild --archive /tmp/codex-reply-release-check
/path/to/candidate/agent-sessions digest --archive /tmp/codex-reply-release-check --dry-run --json > /tmp/codex-reply-after.json
jq '{new, redo, redo_reasons, fingerprints, estimate, warning}' /tmp/codex-reply-before.json /tmp/codex-reply-after.json
```

Do not Pull the fleet from the candidate. If rendering the clone, first redirect
all configured View outputs to a temporary directory, per RELEASING.md.

Compare the original and rebuilt clone's `sessions`/`messages` rows by
`(source,native_id)` (including role, kind, text and native order). All additional
Digest work must correspond to changed Dialog; unchanged Sessions must keep
their old fingerprints and freshness. Inspect each of the 16 known Sessions
using `info --source codex <native-id> --archive /tmp/codex-reply-release-check`
and its rendered Transcript:
all response-item question/answer pairs must occur once in native order, with
long answers complete. Compaction and UI copies must add nothing. Check any
additional changed Session against its raw Capture: unknown leading envelopes
now survive too, so the precise affected set may exceed the 16 question-reply
Sessions. Machine-tag-only Sessions must not change. Compare baseline redo work
before attributing any existing stale Digests to this fix.

Expected: changed Session Days (and any later covered context whose fingerprint
includes the restored Message) may redo, followed by Session Title refresh.
Unaffected Dialog must cause zero additional redo. Exact counts, redo reasons,
duration, and the complete affected Session set require this verification.

## Proposed release-note text

Fixed Codex question replies disappearing from Dialog. Transcripts now retain
each question with its user answer, and preserve unknown user envelopes with a
warning. Known app-navigation, hook and heartbeat injections are filtered
without warnings. Run Rebuild once to recover replies from Capture-only
Sessions; normal Pull refreshes reachable native Sessions automatically.
Recovered Dialog intentionally regenerates affected Digests (question replies
were found in 16 Sessions); unchanged Dialog keeps existing Digests. Before
publication, append the verification dry-run's additional redo count, reasons, and
estimated duration to this note.
