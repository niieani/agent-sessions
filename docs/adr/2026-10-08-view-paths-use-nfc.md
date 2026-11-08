---
status: accepted
---
# View paths use NFC on every platform

APFS resolves NFC and NFD names to the same inode; Linux filesystems generally
store them as distinct entries. Requiring inode equality during orphan
reconciliation therefore deleted live NFD Views on Linux after Render wrote an
NFC duplicate. Normalizing only the sweep would retain duplicate notes.

Render normalizes the complete layout result to NFC before assigning collision
suffixes. Transcript, index and JSON paths, navigation links, manifest checks
and filesystem writes all consume those canonical paths. Durable titles and
metadata retain their original Unicode text; the output root is user-owned and
is not respelled.

Before writing any Views, Render inspects existing entries for live paths and
their ancestors. Multiple entries equivalent under NFC fail before mutation,
even when their content is identical: choosing one would silently discard a
file. Otherwise Render explicitly renames live NFD entries, children first,
through a temporary sibling to update stored APFS spellings reliably. The inode,
content and mtime survive; git can recognize unchanged notes as renames. A failed
rename rolls back; if rollback also fails, the staged entry is retained and the
error reports its recovery location. No duplicate note is generated.

Git detects renames by comparing content. Regression coverage verifies `R100`
renames between snapshots of the actual directory entries on both platforms.
Render does not edit Git's index. On APFS, an index already storing NFD names can
retain those old entries during `git add -A` because they still resolve to the
canonical files; that index may need an explicit refresh by its owner.

The sweep treats NFC equivalence as View identity independently of filesystem
identity. Case equivalence remains a filesystem-specific rule: case-only matches
must resolve to the same inode. This preserves the existing handling of Project
case changes without treating separate Linux case variants as one file.

Keeping NFD spellings indefinitely was rejected: canonical NFC links would not
resolve to those entries on Linux. Normalizing only scalar layout variables was
rejected: template literals and path-valued variables can also contain NFD.
The existing direct `unicode-normalization` dependency supplies NFC; no new
dependency or dependency update is needed.
