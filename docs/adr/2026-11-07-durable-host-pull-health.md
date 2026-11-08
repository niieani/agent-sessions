# Host Pull Health survives Rebuild

Supersedes the disposable-state decision in [Unreachable Hosts](2026-11-03-unreachable-hosts-skip-within-a-grace.md).

Resetting tracking on Rebuild hides overdue Hosts for another grace window and changes an earlier successful Pull into “no successful pull recorded”. Rebuild is routine index maintenance; it should not erase monitoring history. Captures cannot prove a whole Host Pull succeeded, including all Sources and Repository Resolution, so this state cannot be derived from them.

Store Host Pull Health in versioned `host-pulls.json` at the Archive root, keyed by the stable Host name. Publish atomically with file and directory sync before updating its SQLite index. Keep earliest tracking and latest success when merging. On Pull and Rebuild under the Archive writer lock, backfill legacy database records and restore the index from the durable file; Rebuild opens the old database before deleting it only when no durable file exists yet; once migrated, even a corrupt database can be discarded. Missing databases reconstruct from the file alone. Malformed files or unsupported versions fail explicitly rather than silently restarting grace. Existing history already lost to an older Rebuild is unrecoverable. If no durable file exists and SQLite confirms corruption in the legacy database, Rebuild warns that health cannot be recovered and continues repairing the index; permission and other operational errors remain hard failures.

The file joins the durable backup set. SQLite remains disposable, and no native Store is accessed. A single file fits the small fleet and avoids filesystem encoding of Host names. Archive mutations retain the existing single-writer lock requirement.

`[[host]] unreachable-grace` optionally overrides `[pull] unreachable-grace` (default `7d`), validated at load with the same integer duration syntax. This lets intermittent Hosts use a longer grace while always-on Hosts inherit a shorter global value. Zero remains valid, and exactly at the boundary remains within grace. Reports show each Unreachable Host's effective grace.
