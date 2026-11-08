//! Per-Host canonical branches follow the existing projection winner.
use super::Db;
use crate::adapters::CaptureMeta;
use crate::model::ConversationBranch;
use anyhow::Result;
use rusqlite::{Transaction, params};
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS conversation_branches (source TEXT NOT NULL, native_id TEXT NOT NULL, host TEXT NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(source,native_id,host));";
pub(super) fn replace(
    tx: &Transaction<'_>,
    meta: &CaptureMeta,
    branches: &[ConversationBranch],
) -> Result<()> {
    tx.execute("INSERT INTO conversation_branches(source,native_id,host,payload) VALUES (?1,?2,?3,?4) ON CONFLICT(source,native_id,host) DO UPDATE SET payload=excluded.payload",params![meta.source,meta.native_id,meta.host,serde_json::to_string(branches)?])?;
    Ok(())
}
impl Db {
    pub fn upsert_conversations_from_capture(
        &mut self,
        conversations: &[crate::model::Conversation],
        meta: &CaptureMeta,
    ) -> Result<usize> {
        let adapter = crate::adapters::adapter_for(&meta.source)
            .ok_or_else(|| anyhow::anyhow!("Source Adapter absent"))?;
        for conversation in conversations {
            anyhow::ensure!(
                adapter.capture_native_id(&conversation.provenance.native_id) == meta.native_id,
                "Session does not belong to Capture"
            );
            let mut projection = meta.clone();
            projection.native_id = conversation.provenance.native_id.clone();
            self.upsert_conversation_from_capture(conversation, &projection)?;
        }
        Ok(conversations.len())
    }

    pub fn branches(&self, session_id: i64) -> Result<Vec<ConversationBranch>> {
        use rusqlite::OptionalExtension;
        let raw:Option<String>=self.conn.query_row("SELECT b.payload FROM conversation_branches b JOIN sessions s ON s.source=b.source AND s.native_id=b.native_id AND s.winner_host=b.host WHERE s.id=?1",[session_id],|r|r.get(0)).optional()?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .transpose()
            .map(Option::unwrap_or_default)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn branches_follow_projection_winner_and_refresh_replaces_obsolete_segments() {
        let directory = tempfile::tempdir().unwrap();
        let mut db = Db::open(&directory.path().join("db.sqlite")).unwrap();
        let mut winner = crate::archive::db::tests::conversation("branching");
        winner.provenance.source = "chatgpt".into();
        winner.provenance.host = "winner".into();
        winner.messages.push(crate::model::Message {
            role: crate::model::Role::User,
            kind: Default::default(),
            text: "Complete winning conversation".into(),
            timestamp_ms: Some(1),
            actions_before: 0,
        });
        winner.branches = vec![ConversationBranch {
            id: "winner-root".into(),
            parent_id: None,
            fork_message_id: None,
            selected: true,
            messages: winner.messages.clone(),
            trailing_actions: 0,
        }];
        let session = db.upsert_conversation(&winner).unwrap();
        let mut stale = winner.clone();
        stale.provenance.host = "stale".into();
        stale.messages[0].text = "old".into();
        stale.branches[0].id = "stale-root".into();
        db.upsert_conversation(&stale).unwrap();
        assert_eq!(db.branches(session).unwrap()[0].id, "winner-root");
        let branched_dump = db.content_dump().unwrap();
        winner.branches.clear();
        db.upsert_conversation(&winner).unwrap();
        assert!(db.branches(session).unwrap().is_empty());
        assert_ne!(branched_dump, db.content_dump().unwrap());
    }
}
