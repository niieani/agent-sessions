//! Disposable render comparison cache; deliberately absent from rebuild dumps.
use super::Db;
use anyhow::Result;
use rusqlite::params;
use std::{collections::BTreeMap, path::Path};
#[derive(Debug, Clone)]
pub struct RenderFileState {
    pub semantic: String,
    pub output_hash: String,
    pub kind: String,
}
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS render_file_state (output TEXT NOT NULL,path TEXT NOT NULL,semantic TEXT NOT NULL,output_hash TEXT NOT NULL,kind TEXT NOT NULL,PRIMARY KEY(output,path));";
impl Db {
    pub fn render_file_states(&self, output: &Path) -> Result<BTreeMap<String, RenderFileState>> {
        let mut query = self.conn.prepare(
            "SELECT path,semantic,output_hash,kind FROM render_file_state WHERE output=?1",
        )?;
        Ok(query
            .query_map([output.to_string_lossy().as_ref()], |r| {
                Ok((
                    r.get(0)?,
                    RenderFileState {
                        semantic: r.get(1)?,
                        output_hash: r.get(2)?,
                        kind: r.get(3)?,
                    },
                ))
            })?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn replace_render_file_states(
        &mut self,
        output: &Path,
        states: &BTreeMap<String, RenderFileState>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let output = output.to_string_lossy();
        tx.execute(
            "DELETE FROM render_file_state WHERE output=?1",
            [output.as_ref()],
        )?;
        for (path, state) in states {
            tx.execute("INSERT INTO render_file_state(output,path,semantic,output_hash,kind) VALUES (?1,?2,?3,?4,?5)",params![output,path,state.semantic,state.output_hash,state.kind])?;
        }
        tx.commit()?;
        Ok(())
    }
}
