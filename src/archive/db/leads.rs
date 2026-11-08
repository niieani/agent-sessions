use super::Db;
use crate::leads::{SessionLead, SessionLeads};
use anyhow::Result;
use rusqlite::params;
impl Db {
    pub fn replace_session_leads(&mut self, leads: &SessionLeads) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM session_leads", [])?;
        for lead in leads.values() {
            tx.execute(
                "INSERT INTO session_leads(source,native_id,record_json) VALUES (?1,?2,?3)",
                params![lead.source, lead.session_id, serde_json::to_string(lead)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Update only the committed page; unrelated rows are untouched.
    pub fn upsert_session_leads(&mut self, leads: &[SessionLead]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for lead in leads {
            lead.validate()?;
            tx.execute(
                "INSERT INTO session_leads(source,native_id,record_json) VALUES (?1,?2,?3) ON CONFLICT(source,native_id) DO UPDATE SET record_json=excluded.record_json",
                params![lead.source, lead.session_id, serde_json::to_string(lead)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn session_leads(&self) -> Result<SessionLeads> {
        anyhow::ensure!(
            self.conn.table_exists(None, "session_leads")?,
            "Session Leads index is missing; run `agent-sessions pull` or `agent-sessions rebuild` first"
        );
        let mut stmt = self
            .conn
            .prepare("SELECT record_json FROM session_leads ORDER BY source,native_id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut leads = SessionLeads::new();
        for row in rows {
            let lead: SessionLead = serde_json::from_str(&row?)?;
            lead.validate()?;
            leads.insert((lead.source.clone(), lead.session_id.clone()), lead);
        }
        Ok(leads)
    }
}
