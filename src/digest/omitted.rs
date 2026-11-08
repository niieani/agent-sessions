use std::path::PathBuf;

use anyhow::Result;

use super::files;
use crate::archive::Archive;
use crate::archive::db::Db;
use crate::fleet::FleetConfig;

/// Return existing Digest directories for top-level Sessions omitted by the
/// current Kind configuration. The database is opened read-only and no Digest
/// is created, edited, or removed.
pub fn list_omitted(archive: &Archive) -> Result<Vec<PathBuf>> {
    let fleet = FleetConfig::load(archive.root())?.unwrap_or_default();
    let db = Db::open_read_only(&archive.db_path())?;
    let mut paths = Vec::new();
    for session in db.sessions_with_dialog()? {
        let messages = db.messages(session.id)?;
        if crate::render::digest_policy(&session, &messages, &fleet.views)?.omitted {
            let path = files::digest_dir(archive.root(), &session.source, &session.native_id)?;
            if path.is_dir() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
