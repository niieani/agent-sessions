use super::*;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(super) struct DigestArgs {
    #[command(subcommand)]
    pub(super) command: Option<DigestCommand>,
    #[command(flatten)]
    pub(super) run: Option<RunArgs>,
}

#[derive(Debug, Subcommand)]
pub(super) enum DigestCommand {
    /// Report the latest digest or repair-digests run from
    /// `digest-status.json`, including whether its process is still alive.
    /// Never takes the Archive writer lock.
    Status {
        #[command(flatten)]
        archive: ArchiveArg,
    },
}

#[derive(Debug, Args)]
pub(super) struct RunArgs {
    /// The Archive directory (captures/, archive.db, views/).
    #[arg(long, value_name = "DIR")]
    pub(super) archive: PathBuf,
    /// Restrict candidates to one Workday.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub(super) day: Option<String>,
    /// Restrict candidates to Workdays on or after this date.
    #[arg(
        long,
        value_name = "YYYY-MM-DD",
        requires = "through",
        conflicts_with = "day"
    )]
    pub(super) from: Option<String>,
    /// Restrict candidates to Workdays on or before this date.
    #[arg(
        long,
        value_name = "YYYY-MM-DD",
        requires = "from",
        conflicts_with = "day"
    )]
    pub(super) through: Option<String>,
    /// Restrict candidates to one Session, as SOURCE:NATIVE-ID.
    #[arg(long, value_name = "SOURCE:NATIVE-ID")]
    pub(super) session: Option<String>,
    /// Regenerate the --day Session Day of --session even when its
    /// Digest is current. Earlier Session Days need current Digests.
    #[arg(long, requires_all = ["session", "day"])]
    pub(super) force: bool,
    /// Print candidates, planned new and redone Digests by Source, and an
    /// estimated duration, without requests, writes, or the writer lock.
    #[arg(long)]
    pub(super) dry_run: bool,
    /// Print the dry-run summary as JSON instead of text.
    #[arg(long, requires = "dry_run")]
    pub(super) json: bool,
    /// Warn when regenerated Session Days exceed this fraction of existing
    /// Day Digests (0 to 1).
    #[arg(long, requires = "dry_run", value_name = "FRACTION", default_value_t = crate::digest::preflight::DEFAULT_REDO_WARN_FRACTION)]
    pub(super) redo_warn_fraction: f64,
    /// Print Digest directories whose Sessions current Kinds omit.
    #[arg(long, conflicts_with_all = ["day", "from", "through", "session", "dry_run"])]
    pub(super) list_omitted: bool,
}

pub(super) fn run(args: DigestArgs) -> anyhow::Result<ExitCode> {
    match (args.command, args.run) {
        (Some(DigestCommand::Status { archive }), _) => status(&archive.archive),
        (None, Some(run)) => digest(run),
        (None, None) => bail!("digest requires --archive or a subcommand"),
    }
}

fn status(archive: &std::path::Path) -> anyhow::Result<ExitCode> {
    let (text, healthy) = crate::digest::progress::report_for(archive)?;
    print!("{text}");
    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn digest(args: RunArgs) -> anyhow::Result<ExitCode> {
    if args.list_omitted {
        let archive = Archive::open_existing(&args.archive)?;
        for path in crate::digest::list_omitted(&archive)? {
            println!("{}", path.display());
        }
        return Ok(ExitCode::SUCCESS);
    }
    let archive = if args.dry_run {
        Archive::open_existing(&args.archive)?
    } else {
        Archive::open(&args.archive)?
    };
    let options = crate::digest::DigestOptions {
        session: args
            .session
            .as_deref()
            .map(crate::digest::SessionKey::parse)
            .transpose()?,
        force: args.force,
        day: args.day,
        from: args.from,
        through: args.through,
        dry_run: args.dry_run,
        now_ms: time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000,
    };
    let stats = crate::digest::run(&archive, &options)?;
    if args.dry_run {
        let preflight = crate::digest::preflight(&archive, &stats.plan, args.redo_warn_fraction)?;
        if let Some(warning) = &preflight.warning {
            eprintln!("WARN: {warning}");
        }
        if args.json {
            println!("{}", serde_json::to_string_pretty(&preflight.to_json())?);
        } else {
            for candidate in &stats.previews {
                println!(
                    "{}/{} {}\t{} tokens{}",
                    candidate.source,
                    candidate.native_id,
                    candidate.workday,
                    candidate.input_tokens,
                    if candidate.truncated {
                        " (truncated)"
                    } else {
                        ""
                    }
                );
            }
            for line in preflight.source_lines() {
                println!("{line}");
            }
            println!("{}", preflight.summary_line());
        }
    } else {
        println!(
            "digest: {} day files, {} session files, {} requests, {} failed, {} fingerprints backfilled",
            stats.day_written,
            stats.session_written,
            stats.requests,
            stats.failed,
            stats.fingerprints_backfilled
        );
    }
    Ok(if stats.failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
