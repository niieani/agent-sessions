//! Recovery is derived only from exact, unambiguous Orchestrator identities.
use super::*;
use crate::model::RecoveredVia;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS capture_presence (
 source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL,
 PRIMARY KEY(source,native_id,host)
);
CREATE TABLE IF NOT EXISTS session_recoveries (
 session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 source TEXT NOT NULL, thread_id TEXT NOT NULL, host TEXT NOT NULL
);";

impl Db {
    pub fn has_capture_presence(&self, source: &str, native_id: &str, host: &str) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM capture_presence WHERE source=?1 AND native_id=?2 AND host=?3)",params![source,native_id,host],|row|row.get(0))?)
    }

    /// A preserved Capture remains authoritative evidence even if Extract fails.
    /// This presence fact never elects or fabricates a Conversation revision.
    pub fn record_capture_presence(&self, meta: &CaptureMeta) -> Result<()> {
        self.index_repository_fact(meta)?;
        self.conn.execute("INSERT INTO capture_presence(source,native_id,host) VALUES (?1,?2,?3) ON CONFLICT DO NOTHING", params![meta.source,meta.native_id,meta.host])?;
        Ok(())
    }

    pub(super) fn recovered_via(&self, session_id: i64) -> Result<Option<RecoveredVia>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source,thread_id,host FROM session_recoveries WHERE session_id=?1",
                [session_id],
                |r| {
                    Ok(RecoveredVia {
                        source: r.get(0)?,
                        thread_id: r.get(1)?,
                        host: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}

/// Provider provenance replaces recovery provenance even before link resolution.
pub(super) fn supersede(
    tx: &rusqlite::Transaction<'_>,
    source: &str,
    native_id: &str,
) -> Result<()> {
    tx.execute(
        "DELETE FROM session_hosts WHERE session_id IN (
        SELECT r.session_id FROM session_recoveries r JOIN sessions s ON s.id=r.session_id
        WHERE s.source=?1 AND s.native_id=?2)",
        params![source, native_id],
    )?;
    tx.execute(
        "DELETE FROM session_recoveries WHERE session_id IN (
        SELECT id FROM sessions WHERE source=?1 AND native_id=?2)",
        params![source, native_id],
    )?;
    Ok(())
}

pub(super) fn materialize(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // Choose one complete Thread, never merge projected Dialog across Threads.
    // Revision wins; exact lexical identity and Host break ties reproducibly.
    let candidates = {
        let mut stmt = tx.prepare(
            "WITH candidates AS (
          SELECT source,native_id,MIN(provider_source) AS provider_source,
                 MIN(provider_native_id) AS provider_native_id
          FROM orchestrator_link_facts WHERE status='candidate'
            AND provider_source IS NOT NULL AND provider_source<>''
            AND provider_native_id IS NOT NULL AND provider_native_id<>''
          GROUP BY source,native_id
          HAVING COUNT(DISTINCT provider_source || char(0) || provider_native_id)=1
        ) SELECT t.source,t.native_id,c.provider_source,c.provider_native_id
          FROM candidates c JOIN orchestrator_threads t USING(source,native_id)
          WHERE NOT EXISTS (SELECT 1 FROM session_revisions r
            WHERE r.source=c.provider_source AND r.native_id=c.provider_native_id)
          AND NOT EXISTS (SELECT 1 FROM capture_presence p
            WHERE p.source=c.provider_source AND p.native_id=c.provider_native_id)
          AND EXISTS (SELECT 1 FROM orchestrator_messages m WHERE m.source=t.source
            AND m.native_id=t.native_id AND m.role IN ('user','assistant') AND trim(m.text)<>'')
          ORDER BY t.logical_revision_ms DESC,t.discriminator DESC,t.adapter_version DESC,
                   t.source,t.native_id,t.winner_host",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    let mut identities = BTreeSet::new();
    let mut retained = BTreeSet::new();
    for (source, thread_id, provider_source, provider_id) in candidates {
        if !identities.insert((provider_source.clone(), provider_id.clone())) {
            continue;
        }
        tx.execute("INSERT INTO sessions (source,native_id,title,project,project_path,
              created_at_ms,updated_at_ms,message_count,starting_model,git_branch,has_dialog,winner_host)
            SELECT ?3,?4,title,project_title,workspace_root,created_at_ms,updated_at_ms,
              (SELECT COUNT(*) FROM orchestrator_messages m WHERE m.source=?1 AND m.native_id=?2
                 AND m.role IN ('user','assistant') AND trim(m.text)<>''), model,branch,1,winner_host
            FROM orchestrator_threads WHERE source=?1 AND native_id=?2
            ON CONFLICT(source,native_id) DO UPDATE SET title=excluded.title,project=excluded.project,
              project_path=excluded.project_path,created_at_ms=excluded.created_at_ms,
              updated_at_ms=excluded.updated_at_ms,message_count=excluded.message_count,
              starting_model=excluded.starting_model,git_branch=excluded.git_branch,
              has_dialog=1,winner_host=excluded.winner_host",
            params![source,thread_id,provider_source,provider_id])?;
        let id: i64 = tx.query_row(
            "SELECT id FROM sessions WHERE source=?1 AND native_id=?2",
            params![provider_source, provider_id],
            |r| r.get(0),
        )?;
        retained.insert(id);
        tx.execute("DELETE FROM messages WHERE session_id=?1", [id])?;
        tx.execute("INSERT INTO messages(session_id,idx,role,text,timestamp_ms)
            SELECT ?3,idx,role,text,timestamp_ms FROM orchestrator_messages
            WHERE source=?1 AND native_id=?2 AND role IN ('user','assistant') AND trim(text)<>'' ORDER BY idx",
            params![source,thread_id,id])?;
        tx.execute("DELETE FROM session_hosts WHERE session_id=?1", [id])?;
        // The Thread Host is provenance; its SQLite path is NOT a provider Store path.
        tx.execute(
            "INSERT INTO session_hosts(session_id,host,native_path)
            SELECT ?3,winner_host,NULL FROM orchestrator_threads WHERE source=?1 AND native_id=?2",
            params![source, thread_id, id],
        )?;
        tx.execute("INSERT INTO session_recoveries(session_id,source,thread_id,host)
            SELECT ?3,source,native_id,winner_host FROM orchestrator_threads WHERE source=?1 AND native_id=?2
            ON CONFLICT(session_id) DO UPDATE SET source=excluded.source,thread_id=excluded.thread_id,host=excluded.host",
            params![source,thread_id,id])?;
    }
    let old = {
        let mut stmt = tx.prepare("SELECT session_id FROM session_recoveries")?;
        stmt.query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for id in old {
        if !retained.contains(&id) {
            tx.execute("DELETE FROM sessions WHERE id=?1 AND NOT EXISTS (
                SELECT 1 FROM session_revisions r WHERE r.source=sessions.source AND r.native_id=sessions.native_id)",[id])?;
            tx.execute("DELETE FROM session_recoveries WHERE session_id=?1", [id])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{conversation, orchestrator};
    use super::*;
    use crate::model::OrchestratorLinkFact;

    #[test]
    fn recovery_refreshes_and_retracts_when_link_becomes_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("db")).unwrap();
        let mut thread = orchestrator("thread", "host", 1);
        db.upsert_orchestrator_projection(&thread).unwrap();
        db.resolve_orchestrator_links().unwrap();
        let first = db.sessions_with_dialog().unwrap().remove(0);
        thread.logical_revision_ms = 2;
        thread.messages[0].text = "new Dialog 🦀\nsecond line".into();
        db.upsert_orchestrator_projection(&thread).unwrap();
        db.resolve_orchestrator_links().unwrap();
        let next = db.sessions_with_dialog().unwrap().remove(0);
        assert_eq!(first.id, next.id);
        assert_eq!(
            db.messages(next.id).unwrap()[0].text,
            thread.messages[0].text
        );
        thread.link_facts.push(OrchestratorLinkFact {
            provider_native_id: Some("conflicting".into()),
            ..thread.link_facts[0].clone()
        });
        thread.logical_revision_ms = 3;
        db.upsert_orchestrator_projection(&thread).unwrap();
        assert_eq!(db.resolve_orchestrator_links().unwrap(), 0);
        assert_eq!(db.session_count().unwrap(), 0);
    }

    #[test]
    fn recovery_chooses_one_thread_independent_of_arrival_order() {
        let a = orchestrator("older", "host-a", 1);
        let mut b = orchestrator("newer", "host-b", 2);
        b.messages[0].text = "newer Thread".into();
        for ordered in [[&a, &b], [&b, &a]] {
            let dir = tempfile::tempdir().unwrap();
            let mut db = Db::open(&dir.path().join("db")).unwrap();
            for thread in ordered {
                db.upsert_orchestrator_projection(thread).unwrap();
                db.resolve_orchestrator_links().unwrap();
            }
            assert_eq!(db.session_count().unwrap(), 1);
            let row = db.sessions_with_dialog().unwrap().remove(0);
            assert_eq!(row.hosts, ["host-b"]);
            assert_eq!(
                row.transcript_facts.recovered_via.unwrap().thread_id,
                "newer"
            );
            assert_eq!(db.messages(row.id).unwrap()[0].text, "newer Thread");
            assert_eq!(db.augmentations_for_session(row.id).unwrap().len(), 2);
        }
    }

    #[test]
    fn real_capture_without_dialog_also_prevents_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("db")).unwrap();
        db.upsert_conversation(&conversation("provider-1")).unwrap();
        db.upsert_orchestrator_projection(&orchestrator("thread", "host", 10))
            .unwrap();
        db.resolve_orchestrator_links().unwrap();
        assert!(db.sessions_with_dialog().unwrap().is_empty());
        let id: i64 = db
            .conn
            .query_row("SELECT id FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert!(db.recovered_via(id).unwrap().is_none());
        assert!(db.messages(id).unwrap().is_empty());
    }

    #[test]
    fn captured_nonextractable_identity_without_session_prevents_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("db")).unwrap();
        db.conn.execute("INSERT INTO session_revisions(source,native_id,host,source_mtime_ms,source_size,adapter_version) VALUES ('claude','provider-1','elsewhere',1,10,1)",[]).unwrap();
        db.upsert_orchestrator_projection(&orchestrator("thread", "host", 10))
            .unwrap();
        assert_eq!(db.resolve_orchestrator_links().unwrap(), 0);
        assert_eq!(db.session_count().unwrap(), 0);
    }

    #[test]
    fn unsupported_or_empty_thread_dialog_does_not_recover() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&dir.path().join("db")).unwrap();
        let mut thread = orchestrator("thread", "host", 10);
        thread.messages[0].role = "tool".into();
        db.upsert_orchestrator_projection(&thread).unwrap();
        db.resolve_orchestrator_links().unwrap();
        assert_eq!(db.session_count().unwrap(), 0);
    }
}
