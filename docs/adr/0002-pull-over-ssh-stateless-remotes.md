# Fleet collection is pull-over-SSH with stateless remotes

The central host pulls from the fleet; remote machines never push and never hold credentials for or knowledge of the central archive. On a schedule (cron, 1–2×/day), the orchestrator connects to each configured host via the system `ssh` (so `~/.ssh/config`, ProxyJump, agents, and control-master all work for free), deploys or updates the static binary if needed, and invokes a remote export that streams a bundle of captures to stdout. Remotes are fully stateless: all bookmarks live centrally, and the orchestrator tells each remote what to export ("everything since X"). We chose pull because sandboxed environments must not be able to reach the central archive, and stateless remotes because it makes every remote trivially replaceable and every pull idempotent.

## Consequences

- A single central writer on a schedule means no write contention — SQLite is sufficient by design, not by luck. The CLI enforces this with one Archive-local exclusive advisory writer lock across every Pull, Rebuild, and Render; a concurrent writer fails fast instead of racing Capture temporary files or SQLite/View updates.
- The remote CLI surface is minimal (enumerate + capture + export); everything else — extract, store, render — runs centrally.
- Losing a remote's local state loses nothing; losing central bookmarks merely causes a re-pull, which idempotent ingest absorbs.
