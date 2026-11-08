use super::{Archive, ArchiveArg};
use anyhow::Result;
use clap::Subcommand;
use std::process::ExitCode;
#[derive(Debug, Subcommand)]
pub enum OriginsCommand {
    /// Report all multi-Host Sessions, including ambiguous attribution.
    List {
        #[command(flatten)]
        archive: ArchiveArg,
    },
}
pub fn run(command: OriginsCommand) -> Result<ExitCode> {
    match command {
        OriginsCommand::List { archive } => {
            let archive = Archive::open_existing(&archive.archive)?;
            let db = crate::archive::Db::open_read_only(&archive.db_path())?;
            for (source, native_id, winner, hosts) in db.origin_sessions()? {
                if hosts.len() < 2 {
                    continue;
                }
                let attribution = db.host_attribution(&source, &native_id)?;
                let copy_hosts = attribution
                    .as_ref()
                    .map(|a| a.copy_hosts(&hosts))
                    .unwrap_or_default();
                println!(
                    "{}",
                    serde_json::json!({"source":source,"nativeId":native_id,"captureHost":winner,"hosts":hosts,"copyHosts":copy_hosts,"attribution":attribution})
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
