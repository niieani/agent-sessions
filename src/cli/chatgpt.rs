use super::{Archive, ArchiveArg};
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;
use std::process::ExitCode;
#[derive(Debug, Subcommand)]
pub enum ChatGptCommand {
    IngestExport {
        #[command(flatten)]
        archive: ArchiveArg,
        #[arg(long)]
        zip: PathBuf,
        #[arg(long)]
        account_id: String,
    },
    IngestExportMedia {
        #[command(flatten)]
        archive: ArchiveArg,
        #[arg(long)]
        zip: PathBuf,
        #[arg(long)]
        account_id: String,
    },
    Status {
        #[command(flatten)]
        archive: ArchiveArg,
    },
}
pub fn run(command: ChatGptCommand) -> Result<ExitCode> {
    let stats = match command {
        ChatGptCommand::IngestExport {
            archive,
            zip,
            account_id,
        } => crate::chatgpt::ingest_export(&Archive::open(&archive.archive)?, &zip, &account_id)?,
        ChatGptCommand::IngestExportMedia {
            archive,
            zip,
            account_id,
        } => {
            let report = crate::chatgpt::ingest_export_media(
                &Archive::open_existing(&archive.archive)?,
                &zip,
                &account_id,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(if report.failed.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        ChatGptCommand::Status { archive } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::chatgpt::status(&Archive::open_existing(
                    &archive.archive
                )?)?)?
            );
            return Ok(ExitCode::SUCCESS);
        }
    };
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(if stats.failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
