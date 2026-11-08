# Session Media is content-addressed Archive data, materialized beside Views

Status: accepted. Archived image links are reaffirmed by [the promotion golden decision](2026-10-21-promotion-goldens-preserve-origin-and-media.md).

Images and files belonging to Conversations are durable Archive data, stored once under their content hash in a source-neutral Session Media layer and covered by backup and rebuild like Captures. Adapters supply Session Media from native evidence (ChatGPT downloads, base64 images inside Claude and Codex logs); no Source owns its own media folder. Native message JSON is not Session Media and is never copied beside Views.

Views reference Session Media through stable content-addressed paths relative to the View root. A separate Media Materialization step fills those paths by a per-machine method (copy, copy-on-write clone, hard link or symbolic link), chosen for the filesystem and re-run when the Views move. Rendered text therefore never changes when storage moves between volumes, and a future viewer can resolve the same hashes from the Archive directly.

## Considered Options

- **Render-time choice among copy, clone, hard link, symlink, relative or absolute Archive paths.** Each ties rendered text or disk usage to one filesystem layout and breaks or duplicates when the Vault moves.
