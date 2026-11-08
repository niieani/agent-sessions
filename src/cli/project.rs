use super::{Archive, ArchiveArg};
use anyhow::{Context, Result};
use clap::Subcommand;
use std::process::ExitCode;

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Associate every day of a Session with a Project.
    Assign {
        /// Source:native-id, preserving the full opaque native identity.
        session: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        reason: Option<String>,
        #[command(flatten)]
        archive: ArchiveArg,
    },
    /// Remove an explicit Session-to-Project association.
    Unassign {
        session: String,
        #[command(flatten)]
        archive: ArchiveArg,
    },
}

pub fn run(command: ProjectCommand) -> Result<ExitCode> {
    match command {
        ProjectCommand::Assign {
            session,
            project,
            reason,
            archive,
        } => {
            let (source, native_id) = session
                .split_once(':')
                .context("Session must be SOURCE:NATIVE-ID")?;
            let archive = Archive::open_existing(&archive.archive)?;
            crate::assignments::assign(&archive, source, native_id, &project, reason.as_deref())?;
            println!("assigned {session} to {project}");
        }
        ProjectCommand::Unassign { session, archive } => {
            let (source, native_id) = session
                .split_once(':')
                .context("Session must be SOURCE:NATIVE-ID")?;
            let archive = Archive::open_existing(&archive.archive)?;
            crate::assignments::unassign(&archive, source, native_id)?;
            println!("unassigned {session}");
        }
    }
    Ok(ExitCode::SUCCESS)
}
