# Agent Sessions

## Project contracts

- Read [SPEC.md](SPEC.md) before changing behavior; update it in the same change.
- Use the terms in [CONTEXT.md](CONTEXT.md); update the glossary when concepts change.
- Respect [architecture decisions](docs/adr/); record new decisions with their reasoning.
- Follow [PLAN.md](docs/PLAN.md) dependencies; move completed slices to [PLAN-completed.md](docs/PLAN-completed.md).

## Invariants

- Native Stores are read-only. Never take write locks on live agent databases.
- Preserve native evidence in Captures. SQLite and Views must remain rebuildable from durable Archive inputs.
- Stream large native files; keep memory bounded.
- Upgrades preserve existing Digest freshness. New freshness inputs require an archive-only backfill on the normal Digest path and upgrade fixtures. Intentional regeneration must be explicit in commit messages and release notes.

## Privacy

Treat repository content and GitHub text as public. Use synthetic examples; exclude private information and real user data from code, fixtures and docs. Review diffs and outgoing artifacts before committing or publishing. Keep private investigation evidence outside version control. Real Archive excerpts handed over to reproduce a bug go under `temp.local/` (gitignored) and never leave it; reproduce them as synthetic fixtures before committing.
