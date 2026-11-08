use super::*;

#[derive(Debug, Args)]
pub(super) struct RepairArgs {
    #[command(flatten)]
    archive: ArchiveArg,
    /// Regenerate every eligible Loose Session's Topic, titles and Digests together.
    #[arg(
        long,
        required_unless_present = "clean_project_prefixes",
        conflicts_with = "clean_project_prefixes"
    )]
    loose: bool,
    /// Remove exact Project prefixes from existing titles, without model requests.
    #[arg(long)]
    clean_project_prefixes: bool,
    /// Regenerate even when identical Dialog was successfully repaired before.
    #[arg(long, requires = "loose")]
    force: bool,
    /// Report selected Sessions without requests or writes.
    #[arg(long)]
    dry_run: bool,
}

pub(super) fn run(args: RepairArgs) -> anyhow::Result<ExitCode> {
    let archive = Archive::open_existing(&args.archive.archive)?;
    let stats = crate::digest::repair(
        &archive,
        &crate::digest::RepairOptions {
            mode: if args.loose {
                crate::digest::RepairMode::Loose
            } else {
                crate::digest::RepairMode::ProjectPrefixes
            },
            dry_run: args.dry_run,
            force: args.force,
            now_ms: time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000,
        },
    )?;
    println!(
        "digest repair{}: {} selected, {} repaired, {} already completed, {} requests, {} input tokens, {} failed",
        if args.dry_run { " dry-run" } else { "" },
        stats.selected,
        stats.repaired,
        stats.skipped,
        stats.requests,
        stats.input_tokens,
        stats.failed
    );
    for error in stats.errors {
        eprintln!("{error}");
    }
    Ok(if stats.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
