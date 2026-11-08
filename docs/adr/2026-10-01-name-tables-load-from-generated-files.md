---
status: accepted
---
# Name Tables load from generated files

The owner's notes system already records which repository and working copies each of their Projects owns, and which machine each Host is. Hand-copying that into `[views.project-names]` drifts: Sessions in unmapped repositories render under raw basenames, so agents reading Transcripts cannot link the Project the notes system knows. Name Tables therefore also load from files listed in `[views] name-tables`, which an external tool regenerates from its own source of truth before each Render. Inline entries remain for mappings no generator covers.

The Archive stays agnostic of where names come from: a Name Table file has the same three tables as `[views]` and nothing else. Every key has exactly one origin — inline, or one file — so a duplicate is a load error rather than a precedence rule; a generator that starts covering a hand-kept entry makes the owner delete the inline copy instead of silently shadowing it. Project keys may be written as remote URLs and are normalized with the same repository-key rule Render uses, so generators never re-implement normalization.

## Considered Options

- Reading Markdown notes and frontmatter directly — rejected: couples the Archive to one notes system's file layout, property names, and host vocabulary.
- An include that may override inline entries — rejected: an override hides which mapping is in force; two origins for one key is a configuration mistake to surface.
- Prefix (subtree) matching for path keys — not adopted here: repository keys already group worktrees and checkouts; path-only Sessions recorded below a Project's root remain unmapped until a need justifies a separate matching rule.

## Consequences

A missing or malformed Name Table file fails every command that loads `config.toml`, as an invalid inline table would; generators should replace the file atomically and leave the previous file in place when they fail. Renaming a Project in the source of truth renames its View directory on the next Render; orphan reconciliation sweeps the old paths, and Digests are unaffected because a Project name change never makes a Digest stale.
