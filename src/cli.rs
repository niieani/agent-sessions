//! The clap surface. Every subcommand is a stub until its slice lands.

mod chatgpt;
mod digest;
mod leads;
mod origins;
mod project;
mod repair;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{ArgAction, Args, Parser, Subcommand};

use crate::archive::Archive;
use crate::fleet::{FleetConfig, FleetPull, HostConfig, SystemSsh, pull_hosts, summary_lines};
use crate::pull::{self, StoreRoots};
use crate::render;

fn report_render(stats: &render::RenderStats) {
    println!("unresolved repositories: {}", stats.unresolved_repositories);
    println!(
        "rendered {} transcripts, {} indexes, {} JSON Views, {} changed Views, removed {} orphaned Views",
        stats.transcripts, stats.indexes, stats.json_views, stats.changed, stats.removed
    );
    println!(
        "render claims: {}",
        stats
            .claims
            .iter()
            .map(|claim| format!("{} {}", claim.name, claim.sessions))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// A durable, tool-independent archive of AI agent conversations.
#[derive(Debug, Parser)]
#[command(name = "agent-sessions", version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("ASESSIONS_BUILD_HASH"), ")"), about, propagate_version = true)]
pub struct Cli {
    /// Increase diagnostic detail (-vv for trace logging).
    #[arg(short, long, global = true, action = ArgAction::Count, conflicts_with = "quiet")]
    verbose: u8,
    /// Reduce diagnostic detail (-qq shows errors only).
    #[arg(short, long, global = true, action = ArgAction::Count, conflicts_with = "verbose")]
    quiet: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct ArchiveArg {
    /// The Archive directory (captures/, archive.db, views/).
    #[arg(long, value_name = "DIR")]
    archive: PathBuf,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Ingest ChatGPT native export and browser evidence into an explicit Archive.
    Chatgpt {
        #[command(subcommand)]
        command: chatgpt::ChatGptCommand,
    },
    /// Inspect or import durable Host origin evidence.
    Origins {
        #[command(subcommand)]
        command: origins::OriginsCommand,
    },
    /// Maintain agent leadership supplied by external feeds.
    Leads {
        #[command(subcommand)]
        command: leads::LeadsCommand,
    },
    /// Maintain durable Session-to-Project assignments.
    Project {
        #[command(subcommand)]
        command: project::ProjectCommand,
    },
    /// Collect from configured Hosts (localhost included) and ingest into
    /// the Archive. Hosts come from <archive>/config.toml ([[host]] entries);
    /// without any configured [[host]] entries, a single localhost entry is
    /// synthesized from the flags below.
    Pull {
        #[command(flatten)]
        archive: ArchiveArg,
        /// Claude Code Store root for the synthesized localhost entry
        /// (default: ~/.claude/projects). Ignored when config.toml exists.
        #[arg(long, value_name = "DIR")]
        claude_root: Option<PathBuf>,
        /// Codex Store root for the synthesized localhost entry (default:
        /// ~/.codex, spanning sessions/ and archived_sessions/). Ignored
        /// when config.toml exists.
        #[arg(long, value_name = "DIR")]
        codex_root: Option<PathBuf>,
        /// Pi Store root for the synthesized localhost entry (default:
        /// ~/.pi/agent/sessions, or $PI_CODING_AGENT_DIR/sessions). Ignored
        /// when config.toml exists.
        #[arg(long, value_name = "DIR")]
        pi_root: Option<PathBuf>,
        /// Cursor Store database (default: the platform Cursor state.vscdb).
        #[arg(long, value_name = "FILE")]
        cursor_root: Option<PathBuf>,
        /// T3 Store base (default: ~/.t3; Store at userdata/statev2.sqlite).
        #[arg(long, value_name = "DIR")]
        t3_root: Option<PathBuf>,
        /// Browser companion capture Store.
        #[arg(long, value_name = "DIR")]
        chatgpt_root: Option<PathBuf>,
        /// Host name for the synthesized localhost entry (default: this
        /// machine's hostname). Ignored when config.toml exists.
        #[arg(long, value_name = "NAME")]
        host: Option<String>,
        /// Re-enumerate this Source on every Host, ignoring its Bookmark.
        #[arg(long, value_parser = ["claude", "codex", "pi", "cursor", "t3", "chatgpt"])]
        full_source: Option<String>,
        /// Also regenerate Views after ingesting.
        #[arg(long)]
        render: bool,
    },
    /// Stream a Bundle of everything newer than a Bookmark to stdout
    /// (remote side of `pull`; requires no Archive, logs to stderr only).
    #[command(hide = true)]
    ResolveRepository {
        #[arg(long, conflicts_with = "paths_json")]
        path: Option<String>,
        #[arg(long, required_unless_present = "path")]
        paths_json: Option<String>,
        #[arg(long, hide = true)]
        budget_ms: Option<u64>,
    },
    Export {
        /// Working-path glob to exclude before Capture (repeatable).
        #[arg(long)]
        exclude_path: Vec<String>,
        #[arg(long, hide = true)]
        skip_repository_lookup: bool,
        /// Export one Source (`claude`, `codex`, or `pi`). Required with
        /// `--since`, because Bookmarks are per Source. Omit for a full
        /// export of every available Source.
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,
        /// Export only Sessions with a native mtime above this Bookmark
        /// (epoch milliseconds); requires `--source`.
        #[arg(long, value_name = "MS")]
        since: Option<i64>,
        /// Claude Code Store root (default: ~/.claude/projects).
        #[arg(long, value_name = "DIR")]
        claude_root: Option<PathBuf>,
        /// Codex Store root (default: ~/.codex — spans sessions/ and
        /// archived_sessions/).
        #[arg(long, value_name = "DIR")]
        codex_root: Option<PathBuf>,
        /// Pi Store root (default: ~/.pi/agent/sessions, or
        /// $PI_CODING_AGENT_DIR/sessions).
        #[arg(long, value_name = "DIR")]
        pi_root: Option<PathBuf>,
        /// Cursor Store database (default: the platform Cursor state.vscdb).
        #[arg(long, value_name = "FILE")]
        cursor_root: Option<PathBuf>,
        /// T3 Store base (default: ~/.t3; Store at userdata/statev2.sqlite).
        #[arg(long, value_name = "DIR")]
        t3_root: Option<PathBuf>,
        /// Browser companion capture Store.
        #[arg(long, value_name = "DIR")]
        chatgpt_root: Option<PathBuf>,
        /// Host name to record in Provenance (default: this machine's
        /// hostname). The central `pull` passes its configured host name.
        #[arg(long, value_name = "NAME")]
        host: Option<String>,
    },
    /// Regenerate archive state/indexes from durable files; never render Views.
    /// Never touches native Stores.
    Rebuild {
        #[command(flatten)]
        archive: ArchiveArg,
    },
    /// Render Transcript Views from Conversations.
    Render {
        /// Emit a machine-readable render summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        archive: ArchiveArg,
        /// Include pseudoXML counts for omitted tool calls.
        #[arg(long)]
        show_action_counts: bool,
    },
    /// Materialize Session Media at stable View-relative paths.
    Materialize {
        #[command(flatten)]
        archive: ArchiveArg,
        #[arg(long, value_enum)]
        method: Option<crate::media::MaterializationMethod>,
    },
    /// Generate durable model-written Digests for closed Session Days.
    Digest(digest::DigestArgs),
    /// Explicitly repair generated Digest titles and content.
    RepairDigests {
        #[command(flatten)]
        options: repair::RepairArgs,
    },
    /// Push model-written Session Titles into supported local native Stores.
    PushTitles {
        #[command(flatten)]
        archive: ArchiveArg,
        /// Restrict write-back to `claude` or `codex`.
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,
        /// Print the write plan without changing Stores or Digests.
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-lock Session Day titles and report resulting View renames.
    Retitle {
        #[command(flatten)]
        archive: ArchiveArg,
        /// Restrict changes to one source-native session id.
        native_id: Option<String>,
        /// Restrict changes to one Workday of the selected session.
        #[arg(long, value_name = "YYYY-MM-DD", requires = "native_id")]
        day: Option<String>,
        /// Plan without changing Digest files.
        #[arg(long)]
        dry_run: bool,
        /// Emit the rename list as structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// List Sessions with Dialog activity on a given Workday.
    Activity {
        #[command(flatten)]
        archive: ArchiveArg,
        /// The Workday to query, `YYYY-MM-DD` in the configured Workday
        /// timezone (see `[views] timezone` / `day-start`).
        #[arg(long, value_name = "YYYY-MM-DD")]
        day: String,
        /// Emit structured JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Full-text search over Conversations.
    Search,
    /// Resolve one exact T3 Thread id to its provider Session.
    Show {
        #[command(flatten)]
        archive: ArchiveArg,
        /// Exact T3 Thread id.
        id: String,
    },
    /// Inspect one archived Session, including source-native diagnostics.
    Info {
        #[command(flatten)]
        archive: ArchiveArg,
        /// Source-native session id. Searches Sources unless --source is set.
        native_id: String,
        /// Restrict lookup to one Source.
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,
        /// Inspect this preserved Host copy instead of the elected winner.
        #[arg(long, value_name = "HOST")]
        host: Option<String>,
        /// Emit structured JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

/// Parse arguments, initialize logging, and dispatch. The bin's entire body.
pub fn run() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();

    let default_filter = match (cli.verbose, cli.quiet) {
        (2.., _) => "trace",
        (1, _) => "debug",
        (_, 2..) => "error",
        (_, 1) => "warn",
        _ => "info",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    match cli.command {
        Command::Chatgpt { command } => chatgpt::run(command),
        Command::Project { command } => project::run(command),
        Command::Origins { command } => origins::run(command),
        Command::Leads { command } => leads::run(command),
        Command::Materialize { archive, method } => {
            let archive = Archive::open_existing(&archive.archive)?;
            let _lock = archive.acquire_writer_lock()?;
            let config = FleetConfig::load(archive.root())?.unwrap_or_default();
            let method = method
                .or(config.media.materialization)
                .context("materialization method required: --method or media.materialization")?;
            let count = crate::render::materialize_media(&archive, method)?;
            println!("Materialized {count} Session Media objects");
            Ok(ExitCode::SUCCESS)
        }
        Command::Pull {
            archive,
            claude_root,
            codex_root,
            pi_root,
            cursor_root,
            t3_root,
            chatgpt_root,
            host,
            render: also_render,
            full_source,
        } => {
            let archive = Archive::open(&archive.archive)?;
            let hosts = match FleetConfig::load(archive.root())? {
                Some(config) if !config.hosts.is_empty() => config.hosts,
                _ => vec![HostConfig::localhost(
                    host.unwrap_or_else(pull::local_hostname),
                    claude_root,
                    codex_root,
                    pi_root,
                    cursor_root,
                    t3_root,
                    chatgpt_root,
                )],
            };
            let self_binary = std::env::current_exe().context("locating agent-sessions binary")?;
            let fleet = pull_hosts(
                &archive,
                &hosts,
                &FleetPull {
                    transport: &SystemSsh,
                    local_binary: &self_binary,
                    full_source: full_source.as_deref(),
                },
            )?;
            for line in summary_lines(&fleet, hosts.len()) {
                println!("{line}");
            }
            if also_render {
                let rstats = render::render_all(&archive)?;
                report_render(&rstats);
            }
            Ok(if fleet.failed() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Render {
            json,
            archive,
            show_action_counts,
        } => {
            let archive = Archive::open(&archive.archive)?;
            let stats = render::render_all_with_options(
                &archive,
                render::RenderOptions { show_action_counts },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                report_render(&stats);
            }
            Ok(if stats.preparation_errors.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::RepairDigests { options } => repair::run(options),
        Command::Digest(args) => digest::run(args),
        Command::Rebuild { archive } => {
            let archive = Archive::open(&archive.archive)?;
            let stats = crate::archive::rebuild(&archive)?;
            println!(
                "rebuilt: {} captures found, {} conversations extracted, {} orchestrator threads extracted, {} skipped, {} failed",
                stats.captures_found,
                stats.conversations_extracted,
                stats.orchestrator_threads_extracted,
                stats.captures_skipped,
                stats.captures_failed
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::ResolveRepository {
            path,
            paths_json,
            budget_ms,
        } => {
            use crate::repository_resolution::Resolver;
            let resolver = crate::repository_resolution::GitResolver::with_limits(
                crate::repository_resolution::GIT_LOOKUP_TIMEOUT,
                budget_ms
                    .map(std::time::Duration::from_millis)
                    .unwrap_or(crate::repository_resolution::HOST_LOOKUP_BUDGET)
                    .min(crate::repository_resolution::HOST_LOOKUP_BUDGET),
            );
            if let Some(path) = path {
                println!("{}", serde_json::to_string(&resolver.lookup(&path)?)?);
            } else {
                let paths: Vec<String> = serde_json::from_str(paths_json.as_deref().unwrap())?;
                println!("{}", serde_json::to_string(&resolver.lookup_many(&paths)?)?);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Export {
            exclude_path,
            skip_repository_lookup,
            source,
            since,
            claude_root,
            codex_root,
            pi_root,
            cursor_root,
            t3_root,
            chatgpt_root,
            host,
        } => {
            let mut roots = StoreRoots::default();
            if let Some(root) = claude_root {
                roots.claude = root;
            }
            if let Some(root) = codex_root {
                roots.codex = root;
            }
            if let Some(root) = pi_root {
                roots.pi = root;
            }
            if let Some(root) = cursor_root {
                roots.cursor = root;
            }
            if let Some(root) = t3_root {
                roots.t3 = root;
                roots.t3_explicit = true;
            }
            if let Some(root) = chatgpt_root {
                roots.chatgpt = root;
            }
            let host = host.unwrap_or_else(pull::local_hostname);
            if since.is_some() && source.is_none() {
                bail!("`export --since` requires `--source`");
            }
            // stdout is the Bundle — nothing else may be printed to it.
            let stdout = std::io::stdout().lock();
            let mut writer = std::io::BufWriter::new(stdout);
            let stats = crate::bundle::export_with_repository_lookup(
                &mut writer,
                &roots,
                &host,
                source.as_deref(),
                since,
                !skip_repository_lookup,
                &exclude_path,
            )?;
            use std::io::Write as _;
            writer.flush()?;
            tracing::info!(
                host,
                sessions = stats.sessions_found,
                since,
                "export complete"
            );
            Ok(if stats.sessions_failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::PushTitles {
            archive,
            source,
            dry_run,
        } => {
            let archive = Archive::open_existing(&archive.archive)?;
            let result = crate::push_titles::run(
                &archive,
                &crate::push_titles::PushTitleOptions { source, dry_run },
            )?;
            for (source, error) in &result.source_errors {
                eprintln!("{source}: {error}");
            }
            for plan in &result.plans {
                let action = match plan.disposition {
                    crate::push_titles::PushTitleDisposition::Push => "push",
                    crate::push_titles::PushTitleDisposition::Unchanged => "unchanged",
                    crate::push_titles::PushTitleDisposition::Keep => "keep",
                    crate::push_titles::PushTitleDisposition::Missing => "missing",
                };
                println!(
                    "{action} {}/{}: {:?} -> {:?}",
                    plan.source, plan.native_id, plan.current, plan.title
                );
            }
            println!(
                "push-titles{}: {} push, {} unchanged, {} keep, {} missing",
                if dry_run { " dry-run" } else { "" },
                result.pushed,
                result.unchanged,
                result.kept,
                result.missing,
            );
            Ok(if result.source_errors.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Activity { archive, day, json } => {
            let archive = Archive::open(&archive.archive)?;
            let active = crate::activity::sessions_active_on(&archive, &day)?;
            if json {
                #[derive(serde::Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Row<'a> {
                    source: &'a str,
                    native_id: &'a str,
                    title: Option<&'a str>,
                    project_path: Option<&'a str>,
                    created_at_ms: Option<i64>,
                    updated_at_ms: Option<i64>,
                    ongoing: bool,
                }
                let rows: Vec<Row<'_>> = active
                    .iter()
                    .map(|s| -> anyhow::Result<Row<'_>> {
                        Ok(Row {
                            source: &s.source,
                            native_id: &s.native_id,
                            title: s.title.as_deref(),
                            project_path: s.project_path.as_deref(),
                            created_at_ms: s.created_at_ms,
                            updated_at_ms: s.updated_at_ms,
                            ongoing: crate::activity::session_is_ongoing_on(&archive, s, &day)?,
                        })
                    })
                    .collect::<anyhow::Result<_>>()?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).expect("serializing activity rows")
                );
            } else {
                for s in &active {
                    let title = s.title.as_deref().unwrap_or("");
                    println!("{}/{}\t{}", s.source, s.native_id, title);
                }
                println!("{} sessions active on {day}", active.len());
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Retitle {
            archive,
            native_id,
            day,
            dry_run,
            json,
        } => {
            let archive = Archive::open(&archive.archive)?;
            let result = crate::retitle::run(
                &archive,
                &crate::retitle::RetitleOptions {
                    native_id,
                    day,
                    dry_run,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result.renames)?);
            } else {
                for rename in &result.renames {
                    println!(
                        "{} → {}",
                        rename.old_path.display(),
                        rename.new_path.display()
                    );
                }
                println!(
                    "retitle{}: {} titles re-locked, {} View renames",
                    if dry_run { " dry-run" } else { "" },
                    result.relocked,
                    result.renames.len()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Search => bail!("`search` is not implemented yet (slice 8)"),
        Command::Show { archive, id } => {
            let archive = Archive::open(&archive.archive)?;
            let lookup = archive
                .db()?
                .lookup_session_by_orchestrator_id(crate::adapters::t3::SOURCE, &id)?
                .with_context(|| format!("no resolved T3 Thread with exact id {id:?}"))?;
            println!(
                "{}/{} on {} -> {}/{}",
                lookup.orchestrator_source,
                lookup.orchestrator_native_id,
                lookup.orchestrator_host,
                lookup.session_source,
                lookup.session_native_id
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Info {
            archive,
            native_id,
            source,
            host,
            json,
        } => {
            let archive = Archive::open(&archive.archive)?;
            let info = crate::info::inspect_session(
                &archive,
                &native_id,
                source.as_deref(),
                host.as_deref(),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print!("{}", crate::info::format_human(&info));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn verbosity_flags_are_global_and_mutually_exclusive() {
        assert!(
            Cli::try_parse_from(["agent-sessions", "-v", "render", "--archive", "/tmp/a"]).is_ok()
        );
        assert!(
            Cli::try_parse_from(["agent-sessions", "render", "--archive", "/tmp/a", "-qq"]).is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "agent-sessions",
                "-v",
                "-q",
                "render",
                "--archive",
                "/tmp/a"
            ])
            .is_err()
        );
    }

    #[test]
    fn pull_and_render_parse_with_archive() {
        Cli::try_parse_from([
            "agent-sessions",
            "pull",
            "--archive",
            "/tmp/a",
            "--claude-root",
            "/tmp/store",
            "--host",
            "testhost",
        ])
        .unwrap();
        Cli::try_parse_from([
            "agent-sessions",
            "render",
            "--archive",
            "/tmp/a",
            "--show-action-counts",
        ])
        .unwrap();
        Cli::try_parse_from(["agent-sessions", "rebuild", "--archive", "/tmp/a"]).unwrap();
    }

    #[test]
    fn digest_parses_day_and_dry_run() {
        let cli = Cli::try_parse_from([
            "agent-sessions",
            "digest",
            "--archive",
            "/tmp/archive",
            "--day",
            "2026-09-01",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Digest(digest::DigestArgs {
                run: Some(digest::RunArgs {
                    day: Some(day),
                    dry_run: true,
                    ..
                }),
                command: None,
            }) if day == "2026-09-01"
        ));
    }

    #[test]
    fn digest_json_and_redo_warn_fraction_require_dry_run() {
        for flag in [&["--json"][..], &["--redo-warn-fraction", "0.1"]] {
            let mut args = vec!["agent-sessions", "digest", "--archive", "/tmp/archive"];
            args.extend_from_slice(flag);
            assert!(Cli::try_parse_from(&args).is_err(), "{flag:?}");
            args.push("--dry-run");
            Cli::try_parse_from(&args).unwrap();
        }
    }

    #[test]
    fn digest_parses_forced_session_day() {
        let cli = Cli::try_parse_from([
            "agent-sessions",
            "digest",
            "--archive",
            "/tmp/archive",
            "--session",
            "codex:01a0b788-4ce9-7e92-bddb-73311ea8df88",
            "--day",
            "2026-09-18",
            "--force",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Digest(digest::DigestArgs {
                run: Some(digest::RunArgs {
                    session: Some(session),
                    day: Some(day),
                    force: true,
                    ..
                }),
                ..
            }) if session == "codex:01a0b788-4ce9-7e92-bddb-73311ea8df88" && day == "2026-09-18"
        ));
        for incomplete in [
            &["--day", "2026-09-18", "--force"][..],
            &["--session", "codex:x", "--force"][..],
        ] {
            let error = Cli::try_parse_from(
                ["agent-sessions", "digest", "--archive", "/tmp/archive"]
                    .into_iter()
                    .chain(incomplete.iter().copied()),
            )
            .unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument
            );
        }
    }

    #[test]
    fn digest_parses_read_only_omitted_listing() {
        let cli = Cli::try_parse_from([
            "agent-sessions",
            "digest",
            "--archive",
            "/tmp/archive",
            "--list-omitted",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Digest(digest::DigestArgs {
                run: Some(digest::RunArgs {
                    list_omitted: true,
                    ..
                }),
                ..
            })
        ));
    }

    #[test]
    fn digest_parses_bounded_workday_range() {
        let cli = Cli::try_parse_from([
            "agent-sessions",
            "digest",
            "--archive",
            "/tmp/archive",
            "--from",
            "2025-12-01",
            "--through",
            "2025-12-30",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Digest(digest::DigestArgs {
                run: Some(digest::RunArgs {
                    from: Some(from),
                    through: Some(through),
                    ..
                }),
                ..
            }) if from == "2025-12-01" && through == "2025-12-30"
        ));
    }

    #[test]
    fn digest_rejects_an_unpaired_workday_range() {
        let error = Cli::try_parse_from([
            "agent-sessions",
            "digest",
            "--archive",
            "/tmp/archive",
            "--from",
            "2025-12-01",
        ])
        .unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn digest_status_parses_without_run_arguments() {
        let cli = Cli::try_parse_from([
            "agent-sessions",
            "digest",
            "status",
            "--archive",
            "/tmp/archive",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Digest(digest::DigestArgs {
                command: Some(digest::DigestCommand::Status { .. }),
                run: None,
            })
        ));
        let error = Cli::try_parse_from([
            "agent-sessions",
            "digest",
            "--archive",
            "/tmp/archive",
            "status",
        ])
        .unwrap_err();
        assert_ne!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        let error =
            Cli::try_parse_from(["agent-sessions", "digest", "--day", "2026-09-01"]).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn push_titles_parses_source_and_dry_run() {
        let cli = Cli::try_parse_from([
            "agent-sessions",
            "push-titles",
            "--archive",
            "/tmp/archive",
            "--source",
            "codex",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::PushTitles {
                source: Some(source),
                dry_run: true,
                ..
            } if source == "codex"
        ));
    }

    #[test]
    fn retitle_parses_selection_dry_run_and_json() {
        let cli = Cli::try_parse_from([
            "agent-sessions",
            "retitle",
            "native-id",
            "--archive",
            "/tmp/archive",
            "--day",
            "2026-09-01",
            "--dry-run",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Retitle {
                native_id: Some(native_id),
                day: Some(day),
                dry_run: true,
                json: true,
                ..
            } if native_id == "native-id" && day == "2026-09-01"
        ));
        assert!(
            Cli::try_parse_from([
                "agent-sessions",
                "retitle",
                "--archive",
                "/tmp/archive",
                "--day",
                "2026-09-01",
            ])
            .is_err()
        );
    }

    #[test]
    fn export_parses_with_flags() {
        Cli::try_parse_from([
            "agent-sessions",
            "export",
            "--since",
            "1755600000000",
            "--source",
            "claude",
            "--claude-root",
            "/tmp/store",
            "--host",
            "laptop",
        ])
        .unwrap();
        // All flags optional: a bare export is a full export.
        Cli::try_parse_from(["agent-sessions", "export"]).unwrap();
    }

    #[test]
    fn stub_subcommands_parse() {
        Cli::try_parse_from(["agent-sessions", "search"]).unwrap();
        Cli::try_parse_from(["agent-sessions", "show", "--archive", "/tmp/a", "thread-id"])
            .unwrap();
    }

    #[test]
    fn info_parses_with_selection_and_json_flags() {
        Cli::try_parse_from([
            "agent-sessions",
            "info",
            "native-id",
            "--archive",
            "/tmp/a",
            "--source",
            "codex",
            "--host",
            "laptop",
            "--json",
        ])
        .unwrap();
    }
}
