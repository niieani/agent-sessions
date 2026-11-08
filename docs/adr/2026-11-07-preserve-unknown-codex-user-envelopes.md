# Preserve unknown Codex user envelopes

Status: accepted.

Codex delivers both machine context and real user replies as tagged user-role
messages. Dropping unknown tags erased question replies in at least 16 Sessions
in the native-format examples supplied on 2026-11-07. Captures retain the evidence,
but missing Dialog also hides user intent from Transcripts and Digests.

Retain unknown leading tags verbatim in Dialog and warn with the tag name only.
Drop only explicitly classified machine envelopes. Decode known question replies
into question/answer prose; malformed bodies survive with a warning. Never treat
UI mirrors or compacted history as new Messages. This follows the SPEC priority
of preserving user Messages over suppressing newly introduced machine noise.

New machine envelopes may temporarily appear in Dialog until classified; this is
preferable to silently losing user words. Re-extraction restores old Captures.
Adapter revision 6 refreshes reachable Store Sessions through normal Pull;
Capture-only recovery uses the existing explicit Rebuild command. No Digest
schema, prompt version, or fingerprint algorithm changes: unchanged Dialog keeps
its existing Digests fresh, while recovered replies legitimately invalidate
covered Dialog. No native Store writes or migration model calls are needed.
