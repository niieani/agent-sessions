//! Native Project metadata belongs to one Host projection, never a filesystem path.

use super::Db;
use crate::adapters::CaptureMeta;
use crate::model::NativeProject;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::fmt::Write as _;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS conversation_native_projects (
    source TEXT NOT NULL,
    native_id TEXT NOT NULL,
    host TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (source, native_id, host)
);";

pub(super) fn replace(
    tx: &Transaction<'_>,
    meta: &CaptureMeta,
    project: Option<&NativeProject>,
) -> Result<()> {
    if let Some(project) = project {
        tx.execute(
            "INSERT INTO conversation_native_projects (source,native_id,host,payload)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(source,native_id,host) DO UPDATE SET payload=excluded.payload",
            params![
                meta.source,
                meta.native_id,
                meta.host,
                serde_json::to_string(project)?
            ],
        )?;
    } else {
        tx.execute(
            "DELETE FROM conversation_native_projects WHERE source=?1 AND native_id=?2 AND host=?3",
            params![meta.source, meta.native_id, meta.host],
        )?;
    }
    Ok(())
}

impl Db {
    /// Reconcile operation-scoped native metadata without re-extracting Dialog.
    pub fn apply_native_projects(
        &mut self,
        source: &str,
        context: &crate::adapters::ExtractionContext,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let adapter = crate::adapters::adapter_for(source)
            .ok_or_else(|| anyhow::anyhow!("Source Adapter absent"))?;
        let identities = {
            let mut statement =
                tx.prepare("SELECT DISTINCT native_id FROM session_revisions WHERE source=?1")?;
            statement
                .query_map([source], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for native_id in identities {
            let Some(project) = context
                .native_projects
                .get(adapter.capture_native_id(&native_id))
            else {
                continue;
            };
            tx.execute(
                "DELETE FROM conversation_native_projects WHERE source=?1 AND native_id=?2",
                params![source, native_id],
            )?;
            if let Some(project) = project {
                tx.execute("INSERT INTO conversation_native_projects(source,native_id,host,payload) SELECT source,native_id,host,?3 FROM session_revisions WHERE source=?1 AND native_id=?2",params![source,native_id,serde_json::to_string(project)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Metadata from the elected Host only. A winning None never uses a loser.
    pub fn native_project(&self, session_id: i64) -> Result<Option<NativeProject>> {
        let raw: Option<String> = self.conn.query_row(
            "SELECT p.payload FROM conversation_native_projects p
             JOIN sessions s ON s.source=p.source AND s.native_id=p.native_id AND s.winner_host=p.host
             WHERE s.id=?1",
            [session_id], |row| row.get(0),
        ).optional()?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .transpose()
    }
}

pub(super) fn append_dump(conn: &Connection, out: &mut String) -> Result<()> {
    let mut statement = conn.prepare(
        "SELECT source,native_id,host,payload FROM conversation_native_projects
         ORDER BY source,native_id,host",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        writeln!(
            out,
            "native-project {}/{} host={} payload={}",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::Db;
    use crate::model::{Conversation, Message, NativeProject, Role};

    fn conversation(host: &str, project_id: &str, text: &str) -> Conversation {
        let mut conversation = super::super::tests::conversation("shared");
        conversation.provenance.host = host.into();
        conversation.native_project = Some(NativeProject {
            source: "claude".into(),
            native_id: project_id.into(),
            name: Some(format!("Project {project_id}")),
        });
        conversation.messages.push(Message {
            role: Role::User,
            kind: Default::default(),
            text: text.into(),
            timestamp_ms: Some(1),
            actions_before: 0,
        });
        conversation
    }

    #[test]
    fn project_follows_elected_host_and_refresh_clears_without_loser_fallback() {
        for winner_first in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let mut db = Db::open(&directory.path().join("db.sqlite")).unwrap();
            let mut winner = conversation("winner", "winning", "Complete winning dialogue");
            let loser = conversation("loser", "losing", "old");
            let ordered = if winner_first {
                [&winner, &loser]
            } else {
                [&loser, &winner]
            };
            for conversation in ordered {
                db.upsert_conversation(conversation).unwrap();
            }
            let row = db.sessions_with_dialog().unwrap().remove(0);
            assert_eq!(row.winner_host.as_deref(), Some("winner"));
            assert_eq!(row.native_project, winner.native_project);
            assert_eq!(db.native_project(row.id).unwrap(), winner.native_project);
            assert_eq!(
                db.session_row(row.id).unwrap().unwrap().native_project,
                winner.native_project
            );
            assert!(row.project_path.is_none());

            winner.native_project.as_mut().unwrap().name = Some("Renamed Project".into());
            db.upsert_conversation(&winner).unwrap();
            assert_eq!(db.native_project(row.id).unwrap(), winner.native_project);
            winner.native_project = None;
            db.upsert_conversation(&winner).unwrap();
            assert!(db.native_project(row.id).unwrap().is_none());
            assert!(
                db.session_row(row.id)
                    .unwrap()
                    .unwrap()
                    .native_project
                    .is_none()
            );
        }
    }

    #[test]
    fn project_changes_with_projection_winner_and_keeps_source_identity_separate() {
        let directory = tempfile::tempdir().unwrap();
        let mut db = Db::open(&directory.path().join("db.sqlite")).unwrap();
        let first = conversation("first", "first-project", "First dialogue");
        let session = db.upsert_conversation(&first).unwrap();
        let second = conversation("second", "second-project", "More complete second dialogue");
        db.upsert_conversation(&second).unwrap();
        assert_eq!(db.native_project(session).unwrap(), second.native_project);

        let mut other_source = first.clone();
        other_source.provenance.source = "codex".into();
        other_source.native_project.as_mut().unwrap().source = "codex".into();
        let other = db.upsert_conversation(&other_source).unwrap();
        assert_eq!(
            db.native_project(other).unwrap(),
            other_source.native_project
        );
        assert_eq!(db.native_project(session).unwrap(), second.native_project);

        db.retire_session_identity("claude", "shared").unwrap();
        assert!(db.native_project(session).unwrap().is_none());
        assert_eq!(
            db.native_project(other).unwrap(),
            other_source.native_project
        );
        let orphaned: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_native_projects WHERE source='claude'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphaned, 0);
    }

    #[test]
    fn native_metadata_participates_in_rebuild_equivalence_and_corruption_fails() {
        let directory = tempfile::tempdir().unwrap();
        let mut db = Db::open(&directory.path().join("original.sqlite")).unwrap();
        let mut rebuilt = Db::open(&directory.path().join("rebuilt.sqlite")).unwrap();
        let conversation = conversation("host", "native-project", "Dialogue");
        let session = db.upsert_conversation(&conversation).unwrap();
        rebuilt.upsert_conversation(&conversation).unwrap();
        assert_eq!(db.content_dump().unwrap(), rebuilt.content_dump().unwrap());
        let mut renamed = conversation.clone();
        renamed.native_project.as_mut().unwrap().name = Some("Changed native name".into());
        rebuilt.upsert_conversation(&renamed).unwrap();
        assert_ne!(db.content_dump().unwrap(), rebuilt.content_dump().unwrap());

        db.conn
            .execute(
                "UPDATE conversation_native_projects SET payload='invalid-json'",
                [],
            )
            .unwrap();
        assert!(db.native_project(session).is_err());
        assert!(db.session_row(session).is_err());
    }

    #[test]
    fn nonextractable_host_removes_only_its_native_project_projection() {
        let directory = tempfile::tempdir().unwrap();
        let mut db = Db::open(&directory.path().join("db.sqlite")).unwrap();
        let winner = conversation("winner", "winning", "Complete winning dialogue");
        let loser = conversation("loser", "losing", "old");
        let session = db.upsert_conversation(&winner).unwrap();
        db.upsert_conversation(&loser).unwrap();

        db.upsert_nonextractable_from_capture(&super::super::tests::meta(&loser, 0, 3))
            .unwrap();
        assert_eq!(db.native_project(session).unwrap(), winner.native_project);
        let remaining: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_native_projects WHERE host='loser'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);

        db.upsert_nonextractable_from_capture(&super::super::tests::meta(&winner, 0, 25))
            .unwrap();
        assert!(db.session_row(session).unwrap().is_none());
        assert!(db.native_project(session).unwrap().is_none());
    }
}
