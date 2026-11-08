use super::{Archive, ArchiveArg};
use anyhow::{Context, Result};
use clap::Subcommand;
use std::process::ExitCode;
#[derive(Debug, Subcommand)]
pub enum LeadsCommand {
    /// Import agent leadership from configured Session Lead Feeds.
    Sync {
        #[command(flatten)]
        archive: ArchiveArg,
        #[arg(long)]
        feed: Option<String>,
    },
    /// List durable leads, including Sessions not yet captured.
    List {
        #[command(flatten)]
        archive: ArchiveArg,
    },
    /// Show the complete durable lead for SOURCE:NATIVE-ID.
    Show {
        session: String,
        #[command(flatten)]
        archive: ArchiveArg,
    },
}
pub fn run(command: LeadsCommand) -> Result<ExitCode> {
    match command {
        LeadsCommand::Sync { archive, feed } => {
            let archive = Archive::open_existing(&archive.archive)?;
            let count = crate::leads::sync(&archive, feed.as_deref())?;
            println!("synced {count} Session Leads");
        }
        LeadsCommand::List { archive } => {
            let archive = Archive::open_existing(&archive.archive)?;
            crate::leads::lead_files(archive.root())?;
            let leads = crate::archive::Db::open_read_only(&archive.db_path())?.session_leads()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&leads.values().collect::<Vec<_>>())?
            );
        }
        LeadsCommand::Show { session, archive } => {
            let (source, id) = session
                .split_once(':')
                .context("Session must be SOURCE:NATIVE-ID")?;
            let archive = Archive::open_existing(&archive.archive)?;
            crate::leads::lead_files(archive.root())?;
            let leads = crate::archive::Db::open_read_only(&archive.db_path())?.session_leads()?;
            let lead = leads
                .get(&(source.into(), id.into()))
                .with_context(|| format!("no Session Lead for {session}"))?;
            println!("{}", serde_json::to_string_pretty(lead)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}
