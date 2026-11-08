# Forks split Shared Segments out of existing Views

Status: accepted

A View file keeps its tail. When a Fork appears inside dialogue that an existing View already shows, the Messages before the Fork move into a new Shared Segment file; the existing file keeps its path, its Digest and every Message after the Fork. Each continuation file names the Shared Segment it continues in frontmatter, so a reader follows a chain of whole files (read the Shared Segment, then the continuation) without positional references. A Shared Segment ends with a reserved `<branches>` block linking its continuations. Every file is a pure function of the current Conversation graph: a Conversation imported already forked renders the same files as one watched forking live.

Only Path Ends receive Digests. Digests stay per Session Day; a Path End's Session Day Digest takes the Shared Segments before it as context, so it describes its path completely while the file shows only its own Messages. Shared Segments carry no Digest, and moving Messages out of a Digested file does not invalidate or regenerate its Digest. Changes that only add navigation never count as Dialog changes.

An edit of the opening user Message is not a Fork: each opening starts its own Session with its own Topic and title, since nothing is shared.

Files share the Session's title. Shared Segment files are distinguished by the short id of the Message at their Fork, and later continuations by `(branch N)` in creation order; neither changes when further Forks appear, because a new continuation is always the newest.

## Considered Options

- **Oldest continuation stays inline; later branches link back to a Message position.** Never moves Messages, but readers of a later branch must stop at a numbered Message inside another file, which needs per-Message anchors whose numbering churns with any rendering change.
- **One file per Branch Segment with an empty root listing branches.** Produced dialogue-free root files whenever the opening Message was edited, and split files on ChatGPT's currently selected path, so an edit could rename what a reader had already seen.
