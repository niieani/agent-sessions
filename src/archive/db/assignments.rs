use super::Db;
use crate::assignments::{ProjectAssignment, ProjectAssignments};
use anyhow::Result;
use rusqlite::params;

impl Db {
    pub fn has_session_identity(&self, source: &str, native_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE source = ?1 AND native_id = ?2)",
            params![source, native_id],
            |row| row.get(0),
        )?)
    }

    pub fn replace_project_assignments(&mut self, assignments: &ProjectAssignments) -> Result<()> {
        let transaction = self.conn.transaction()?;
        transaction.execute("DELETE FROM project_assignments", [])?;
        for assignment in assignments.values() {
            transaction.execute("INSERT INTO project_assignments(source,native_id,project,assigned_at_ms,reason) VALUES (?1,?2,?3,?4,?5)", params![assignment.source, assignment.native_id, assignment.project, assignment.assigned_at_ms, assignment.reason])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn project_assignments(&self) -> Result<ProjectAssignments> {
        let mut statement = self.conn.prepare("SELECT source,native_id,project,assigned_at_ms,reason FROM project_assignments ORDER BY source,native_id")?;
        let records = statement.query_map([], |row| {
            Ok(ProjectAssignment {
                schema_version: 1,
                source: row.get(0)?,
                native_id: row.get(1)?,
                project: row.get(2)?,
                assigned_at_ms: row.get(3)?,
                reason: row.get(4)?,
            })
        })?;
        let mut assignments = ProjectAssignments::new();
        for record in records {
            let record = record?;
            assignments.insert((record.source.clone(), record.native_id.clone()), record);
        }
        Ok(assignments)
    }
}
