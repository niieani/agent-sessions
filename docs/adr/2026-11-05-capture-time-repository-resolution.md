---
status: accepted
---
# Capture-time repository resolution

A Session's working directory is evidence of where it ran, not a repository
identity. Claude Code usually records a branch without a remote URL. Worktrees
therefore split one repository into several path Projects.

Project Assignments remain authoritative. Automatic naming uses the exact path
Name Table entry first, then verified Native Project membership and a recorded
repository URL, then capture-time Git `origin` lookup on the contributing Host.
For a missing directory, an exact T3 provider identity Link may supply its
Project's captured `repositoryIdentity.origin.canonicalKey`, falling back to
`repositoryIdentity.canonicalKey`. The origin key preserves fork identity: T3's
top-level canonical key can name the upstream repository. Older T3 Projects without
that fact can resolve their captured `workspace_root` on their Host. Conflicting
repository answers do not establish identity. T3 Project rows and Project events
already travel in Captures; there is no new native-store query.

A missing directory with a recorded branch and no resolved repository becomes
Loose with an unresolved marker, retaining the original path. An existing
non-repository directory, a repository without `origin`, or a missing directory
without a branch keeps its path Project. Sending every failed lookup to Loose
would discard legitimate non-Git work. Failure to execute a lookup is a retryable
error, never a negative observation. Settled negative observations are reused until the Capture
revision changes; later T3 evidence can still resolve a Missing workspace.

## Durability and backfill

Capture metadata gains a separate optional `repositoryResolution` fact containing
the recorded path, lookup outcome, and unresolved status. Native payloads and
native Git snapshots are unchanged. Successful host/path facts are sticky and
reusable by later Sessions at that path. Bundle settlement preserves them even
when a later export finds no directory. SQLite indexes these facts separately;
Rebuild reconstructs them from Capture metadata without Git or SSH.

Ordinary `pull --archive DIR` backfills existing Captures, including native
Sessions already expired from their Stores. SSH Pull runs the version/deploy
handshake before using the hidden read-only `resolve-repository` command for
archived paths in batches capped at 100 KiB per complete shell-quoted command,
returning independent results per path. This stays below Linux’s 128 KiB
single-argument limit even after JSON escaping and shell quote expansion. Resolution
uses indexed per-Host Session facts rather than re-extracting the Archive; T3
repository facts are indexed during ordinary extraction, with one-time lazy
backfill for older projections. Corrupt units are warned and counted independently,
while operational IO and database errors remain explicit. Unreachable Hosts are skipped under the existing grace policy;
command failures are warned and retried next Pull. Cross-platform version
mismatches retain the existing explicit failure policy. A normal Pull captures
current T3 data; `pull --archive DIR --full-source t3` refreshes older T3 Captures
when needed. `rebuild --archive DIR` reindexes durable answers and captured T3
fallbacks; it cannot perform a Host lookup.

## Considered Options

- Name Table prefix matching: rejected. T3's folder layout is only one worktree
  convention and would require an entry per repository. Exact path precedence
  and the stance in the 2026-10-01 Name Tables ADR are unchanged.
- T3 data alone: rejected. It covers only T3-launched Sessions.
- Populate the native Git snapshot with inferred URLs: rejected. It would
  misrepresent what the Source recorded.
- Database-only enrichment: rejected. Deleted worktrees cannot be looked up again
  after Rebuild.

## Consequences

Repository lookup is bounded because unattended macOS launchd jobs can block
inside Git on a removable-volume permission prompt. Git gets a 15-second
per-path deadline shared across its commands, and each Host Pull gets two minutes
of total lookup work. Timeout kills the process group (including helpers) and
reaps the child. The maintained `process-wrap` crate provides process-group
ownership and cleanup; a small polling controller provides the deadline and
file-backed output avoids waiting on inherited pipes. The parent never calls
`metadata` or changes directory to a recorded workspace; Git and the negative-
discovery access probe perform that IO after exec inside the bounded group. Refresh otherwise reads only the Archive's
Captures, durable facts and database, not recorded working directories.

Remote exports skip lookup; one post-ingest resolution phase shares the Host
budget across Sources and command-size chunks. Remote batches receive the
remaining budget, with two seconds reserved for startup and response cleanup;
the local SSH process group also has a deadline within the remaining budget.
Timeout, execution failure and paths skipped by budget exhaustion are retry-later
outcomes, never Path or Missing observations. A per-Host summary counts distinct
deferred paths with a sample. These expected unattended permission failures do
not fail Pull, allowing Digest and publishing to run; archive unit, disk and
database failures remain failures. Capture replacement preserves earlier facts,
including their original revision, so failed lookups retain placement and retry
on the next Pull without requiring another native change.

Under launchd on macOS, new paths on removable volumes retain their previous
placement or a path Project until a Pull from a shell can resolve them, unless a
captured T3 Link or Name Table supplies identity. A timeout cannot justify Loose
placement or an unresolved marker. Shell and launchd checks are both required
on the collection Host: a successful interactive lookup does not prove unattended access.

Project identity affects grouping and layout, never Dialog Fingerprints, covered
ranges, paid-for Digests, or Locked Titles. Default View paths move from
`projects/<worktree-folder>/<stem>.md` to `projects/<repository-name>/<stem>.md`
(or the Name Table display name); unresolved paths move to `loose/<month>/`.
The default stem and note title do not change. Custom layouts containing
`project` or `project_dir` may change both directory and basename. Orphan
reconciliation removes old generated paths. Vault wikilinks require a separate
Vault-owned update; this repository never edits that Vault.

The [rollout checklist](2026-11-05-capture-time-repository-resolution.md) records backfill,
View relocation semantics, and the checks that require the test Archive.

Git can conflate unreadable repository metadata with an absent repository.
Explicit permission errors stay retryable; generic non-repository errors require
a shell probe within the same deadline. Existing or inaccessible `.git` entries
at the workspace or its physical ancestors prevent a final Path observation.
This conservatively retries malformed repository metadata too. Real macOS TCC
EPERM prompts still need platform verification; the check does not infer absence
from an opaque permission error. An OS that deliberately conceals denied paths
as ENOENT cannot be distinguished from absence by this check.

Archive writer guards explicitly unlock on Drop before closing. CLOEXEC cannot
prevent a child in the fork-to-exec window from briefly retaining an inherited
open file description. More Git spawns for missing paths exposed that ownership
bug in parallel tests. This is a production lifetime fix, without lock retries
or test serialization. Process-wrap is retained for the already-tested group
cleanup; its process-group option uses std's posix_spawn-compatible API and is
not evidence of a forced fork path.
