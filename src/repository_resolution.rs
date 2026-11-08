//! Durable capture-time repository evidence, separate from native Git snapshots.
use crate::{
    adapters::{
        Capture, CaptureMeta, CaptureOutcome, adapter_for, collect_sidecars, write_capture_meta,
    },
    archive::Archive,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lookup {
    Resolved(String),
    Path,
    Missing,
}
/// Independent results per recorded working path; errors remain retryable.
pub type LookupResults = BTreeMap<String, std::result::Result<Lookup, String>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub source_revision: (i64, u64),
    #[serde(default)]
    pub branch_recorded: bool,
    pub path: String,
    pub lookup: Lookup,
    pub unresolved: bool,
}
// A permission prompt under launchd will never be answered. Fifteen seconds
// tolerates ordinary Git startup; two minutes caps all added lookup work for a
// Host (at most eight fully stalled paths), leaving time for Digest/Render.
pub const GIT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);
pub const HOST_LOOKUP_BUDGET: Duration = Duration::from_secs(120);

#[doc(hidden)]
pub fn bounded_command_output(command: Command, limit: Duration) -> Result<std::process::Output> {
    crate::bounded_process::output(command, limit)
}

pub trait Resolver {
    fn lookup(&self, path: &str) -> Result<Lookup>;
    fn lookup_before(&self, path: &str, deadline: Instant) -> Result<Lookup> {
        if Instant::now() >= deadline {
            bail!("Host repository lookup budget exhausted; retry later");
        }
        self.lookup(path)
    }
    fn lookup_many(&self, paths: &[String]) -> Result<LookupResults> {
        let deadline = Instant::now() + HOST_LOOKUP_BUDGET;
        Ok(paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    self.lookup_before(path, deadline)
                        .map_err(|e| format!("{e:#}")),
                )
            })
            .collect())
    }
    /// Counting seam for resolver-owned legacy T3 extraction and Export.
    fn extracted(&self) {}
}
pub struct GitResolver;
pub struct BoundedGitResolver {
    per_lookup: Duration,
    deadline: Instant,
}
impl GitResolver {
    pub fn with_limits(per_lookup: Duration, budget: Duration) -> BoundedGitResolver {
        BoundedGitResolver {
            per_lookup,
            deadline: Instant::now() + budget,
        }
    }
    fn lookup_until(path: &str, deadline: Instant) -> Result<Lookup> {
        if Instant::now() >= deadline {
            bail!("Host repository lookup budget exhausted; retry later");
        }
        // Never stat the recorded path, or use Command::current_dir: either
        // can block the parent on TCC/removable volumes before a child exists.
        // Git performs all such IO after exec, inside the timed process group.
        let run = |args: &[&str]| -> Result<std::process::Output> {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(path)
                .args(args)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C");
            bounded_command_output(command, deadline.saturating_duration_since(Instant::now()))
        };
        let repo = run(&["rev-parse", "--git-dir"])?;
        if !repo.status.success() {
            let error = String::from_utf8_lossy(&repo.stderr);
            if error.contains("Permission denied") || error.contains("Operation not permitted") {
                bail!("git workspace access denied; retry later: {error}");
            }
            if error.starts_with("fatal: cannot change to ")
                && error.trim_end().ends_with(": No such file or directory")
            {
                return Ok(Lookup::Missing);
            }
            if error.contains("not a git repository") {
                // Git's discovery collapses denied HEAD/objects/refs reads
                // into "not a git repository". Confirm absence independently,
                // inside the SAME bounded group/deadline, never in the parent.
                let mut probe = Command::new("/bin/sh");
                probe
                    .args(["-c", NON_REPOSITORY_PROBE, "repository-access-probe", path])
                    .env("LC_ALL", "C");
                let output = bounded_command_output(
                    probe,
                    deadline.saturating_duration_since(Instant::now()),
                )?;
                if !output.status.success() {
                    bail!(
                        "cannot confirm non-repository workspace; retry later: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                return Ok(Lookup::Path);
            }
            bail!(
                "git lookup failed: {}",
                String::from_utf8_lossy(&repo.stderr)
            );
        }
        let remote = run(&["config", "--get", "remote.origin.url"])?;
        if remote.status.code() == Some(1) {
            return Ok(Lookup::Path);
        }
        if !remote.status.success() {
            bail!(
                "git origin lookup failed: {}",
                String::from_utf8_lossy(&remote.stderr)
            );
        }
        let url = String::from_utf8(remote.stdout)?.trim().to_owned();
        if crate::repository_key::repository_key(&url).is_none() {
            return Ok(Lookup::Path);
        }
        Ok(Lookup::Resolved(url))
    }
}
// cd and ls run only after exec. A surviving/inaccessible .git entry makes
// Git's negative discovery ambiguous; only ENOENT at every ancestor confirms
// Path. Physical cd follows symlinked workspaces like Git. No path is embedded
// in shell source. /bin/ls is available on both supported Host platforms.
const NON_REPOSITORY_PROBE: &str = r#"
cd -P -- "$1" || exit 1
while :; do
    if error=$(/bin/ls -d .git 2>&1); then
        printf '%s\n' 'Git rejected an existing .git entry' >&2
        exit 1
    fi
    case "$error" in
        *'Permission denied'*|*'Operation not permitted'*) printf '%s\n' "$error" >&2; exit 1 ;;
        *': No such file or directory') ;;
        *) printf '%s\n' "$error" >&2; exit 1 ;;
    esac
    previous=$PWD
    cd -P .. || exit 1
    [ "$PWD" = "$previous" ] && exit 0
done
"#;

impl Resolver for GitResolver {
    fn lookup(&self, path: &str) -> Result<Lookup> {
        Self::lookup_until(path, Instant::now() + GIT_LOOKUP_TIMEOUT)
    }
    fn lookup_before(&self, path: &str, deadline: Instant) -> Result<Lookup> {
        Self::lookup_until(path, deadline.min(Instant::now() + GIT_LOOKUP_TIMEOUT))
    }
}
impl Resolver for BoundedGitResolver {
    fn lookup(&self, path: &str) -> Result<Lookup> {
        self.lookup_before(path, self.deadline)
    }
    fn lookup_before(&self, path: &str, deadline: Instant) -> Result<Lookup> {
        GitResolver::lookup_until(
            path,
            deadline
                .min(self.deadline)
                .min(Instant::now() + self.per_lookup),
        )
    }
}

/// Resolve only on the contributing Host. Rebuild passes None and never probes
/// paths from another Host. Successful host/path evidence is sticky.
#[derive(Default, Debug)]
pub struct RefreshStats {
    pub units_failed: usize,
    pub retry_later: usize,
    pub retry_sample: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct T3Evidence {
    pub repository_url: Option<String>,
    pub workspace_root: Option<String>,
    pub links: Vec<crate::model::OrchestratorLinkFact>,
}

pub fn refresh(
    archive: &Archive,
    host: Option<&str>,
    resolver: &dyn Resolver,
) -> Result<RefreshStats> {
    refresh_indexed(archive, host, resolver, host.is_some())
}
pub(crate) fn refresh_recorded(archive: &Archive, host: &str) -> Result<RefreshStats> {
    refresh_indexed(archive, Some(host), &GitResolver, false)
}
fn refresh_indexed(
    archive: &Archive,
    host: Option<&str>,
    resolver: &dyn Resolver,
    probe: bool,
) -> Result<RefreshStats> {
    let db = archive.db()?;
    let mut stats = RefreshStats::default();
    let sessions = db.repository_candidates(host)?;
    if sessions.is_empty() {
        return Ok(stats);
    }
    db.prepare_repository_t3(archive, host, resolver, &mut stats.units_failed)?;
    let mut candidates = db.repository_workspace_candidates(host)?;
    candidates.extend(sessions);
    let mut sticky = db.repository_sticky(host)?;
    let mut pending = Vec::new();
    let mut paths = std::collections::BTreeSet::new();
    for candidate in candidates {
        let settled = candidate
            .fact
            .as_ref()
            .is_some_and(|f| f.source_revision == candidate.revision);
        if settled
            && !sticky.contains_key(&(candidate.host.clone(), candidate.path.clone()))
            && candidate
                .fact
                .as_ref()
                .is_some_and(|f| f.lookup != Lookup::Missing || candidate.source == "t3")
        {
            continue;
        }
        if !settled
            && probe
            && !sticky.contains_key(&(candidate.host.clone(), candidate.path.clone()))
        {
            paths.insert(candidate.path.clone());
        }
        pending.push(candidate);
    }
    if pending.is_empty() {
        return Ok(stats);
    }
    let answers = if paths.is_empty() {
        BTreeMap::new()
    } else {
        let paths = paths.into_iter().collect::<Vec<_>>();
        match resolver.lookup_many(&paths) {
            Ok(answers) => answers,
            Err(error) => {
                tracing::warn!(%error, "repository batch lookup could not run; retry next pull");
                paths
                    .into_iter()
                    .map(|p| (p, Err(format!("{error:#}"))))
                    .collect()
            }
        }
    };
    let mut t3 = None;
    let mut deferred = std::collections::BTreeSet::new();
    for candidate in pending {
        let key = (candidate.host.clone(), candidate.path.clone());
        let settled = candidate
            .fact
            .as_ref()
            .is_some_and(|f| f.source_revision == candidate.revision);
        let lookup = if let Some(answer) = sticky.get(&key) {
            answer.clone()
        } else if settled || !probe {
            match &candidate.fact {
                Some(f)
                    if f.source_revision == candidate.revision
                        || matches!(f.lookup, Lookup::Resolved(_)) =>
                {
                    f.lookup.clone()
                }
                _ => {
                    // Ingest/Rebuild cannot turn an earlier negative observation
                    // into an answer for a new revision. Captured T3 evidence
                    // can still supply an independent positive answer.
                    if candidate.source == "t3" {
                        continue;
                    }
                    if t3.is_none() {
                        t3 = Some(db.repository_t3_fallbacks()?);
                    }
                    let Some(url) = t3
                        .as_ref()
                        .unwrap()
                        .get(&(candidate.source.clone(), candidate.native_id.clone()))
                    else {
                        continue;
                    };
                    Lookup::Resolved(url.clone())
                }
            }
        } else {
            match answers.get(&candidate.path) {
                Some(Ok(answer)) => answer.clone(),
                Some(Err(error)) => {
                    if candidate.source != "t3" {
                        if t3.is_none() {
                            t3 = Some(db.repository_t3_fallbacks()?);
                        }
                        if let Some(url) = t3
                            .as_ref()
                            .unwrap()
                            .get(&(candidate.source.clone(), candidate.native_id.clone()))
                        {
                            Lookup::Resolved(url.clone())
                        } else {
                            deferred.insert(candidate.path.clone());
                            tracing::warn!(path=candidate.path,%error,"repository lookup retry later");
                            continue;
                        }
                    } else {
                        deferred.insert(candidate.path.clone());
                        tracing::warn!(path=candidate.path,%error,"repository lookup retry later");
                        continue;
                    }
                }
                None => {
                    deferred.insert(candidate.path.clone());
                    continue;
                }
            }
        };
        let lookup = if lookup == Lookup::Missing && candidate.source != "t3" {
            if t3.is_none() {
                t3 = Some(db.repository_t3_fallbacks()?);
            }
            t3.as_ref()
                .unwrap()
                .get(&(candidate.source.clone(), candidate.native_id.clone()))
                .cloned()
                .map(Lookup::Resolved)
                .unwrap_or(lookup)
        } else {
            lookup
        };
        let fact = Fact {
            source_revision: candidate.revision,
            branch_recorded: candidate.branch_recorded,
            path: candidate.path,
            unresolved: lookup == Lookup::Missing && candidate.branch_recorded,
            lookup,
        };
        if candidate.fact.as_ref() == Some(&fact) {
            continue;
        }
        let meta_path = match crate::adapters::capture_paths(
            &archive.captures_dir(),
            &candidate.host,
            &candidate.source,
            &candidate.native_id,
        ) {
            Ok(paths) => paths.meta,
            Err(error) => {
                stats.units_failed += 1;
                tracing::warn!(%error,id=candidate.native_id,"invalid repository Capture path");
                continue;
            }
        };
        let loaded = (|| -> Result<CaptureMeta> {
            Ok(serde_json::from_reader(std::fs::File::open(&meta_path)?)?)
        })();
        let mut meta = match loaded {
            Ok(meta) => meta,
            Err(error) => {
                warn_bad_unit(&error, &meta_path)?;
                stats.units_failed += 1;
                continue;
            }
        };
        if matches!(fact.lookup, Lookup::Resolved(_)) {
            deferred.remove(&fact.path);
            sticky.insert(key, fact.lookup.clone());
        }
        meta.repository_resolution = Some(fact);
        if let Err(error) = write_capture_meta(&meta_path, &meta) {
            warn_bad_unit(&error, &meta_path)?;
            stats.units_failed += 1;
            continue;
        }
        db.index_repository_fact(&meta)?;
    }
    stats.retry_later = deferred.len();
    stats.retry_sample = deferred.into_iter().next();
    Ok(stats)
}
fn captures(root: &Path, failures: &mut usize) -> Result<Vec<Capture>> {
    let mut paths = Vec::new();
    collect_sidecars(root, &mut paths)?;
    paths.sort();
    let mut captures = Vec::new();
    for path in paths {
        let loaded = (|| -> Result<_> {
            let meta: CaptureMeta = serde_json::from_reader(std::fs::File::open(&path)?)?;
            Ok(Capture {
                body_path: crate::archive::body_for_sidecar(&path)?,
                meta_path: path.clone(),
                meta,
                written: false,
                outcome: CaptureOutcome::Unchanged,
            })
        })();
        match loaded {
            Ok(capture) => captures.push(capture),
            Err(error) => {
                warn_bad_unit(&error, &path)?;
                *failures += 1;
            }
        }
    }
    Ok(captures)
}
/// Export-only enrichment of the newly captured spool, never the Archive.
pub(crate) fn refresh_captures(
    root: &Path,
    archive_root: &Path,
    host: Option<&str>,
    resolver: &dyn Resolver,
) -> Result<RefreshStats> {
    let mut stats = RefreshStats::default();
    let mut captures = captures(root, &mut stats.units_failed)?;
    let mut sticky = BTreeMap::new();
    for capture in &captures {
        if let Some(fact) = &capture.meta.repository_resolution
            && matches!(fact.lookup, Lookup::Resolved(_))
        {
            sticky.insert(
                (capture.meta.host.clone(), fact.path.clone()),
                fact.lookup.clone(),
            );
        }
    }
    let snapshot = captures.clone();
    let mut t3 = None;
    let mut contexts = BTreeMap::new();
    let mut looked_up = BTreeMap::new();
    let deadline = Instant::now() + HOST_LOOKUP_BUDGET;
    let mut deferred = std::collections::BTreeSet::new();
    for capture in &mut captures {
        if capture.meta.source == "t3" {
            continue;
        }
        if capture
            .meta
            .repository_resolution
            .as_ref()
            .is_some_and(|f| {
                matches!(f.lookup, Lookup::Resolved(_))
                    && f.source_revision == (capture.meta.source_mtime_ms, capture.meta.source_size)
            })
        {
            continue;
        }
        let (path, branch_recorded) = if let Some(fact) =
            capture.meta.repository_resolution.as_ref().filter(|f| {
                f.source_revision == (capture.meta.source_mtime_ms, capture.meta.source_size)
            }) {
            (fact.path.clone(), fact.branch_recorded)
        } else {
            if !contexts.contains_key(&capture.meta.source) {
                let Some(adapter) = adapter_for(&capture.meta.source) else {
                    continue;
                };
                match adapter.prepare_extraction(archive_root) {
                    Ok(context) => {
                        contexts.insert(capture.meta.source.clone(), context);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "repository preparation failed; retry later");
                        continue;
                    }
                }
            }
            let adapter = adapter_for(&capture.meta.source).expect("prepared adapter");
            resolver.extracted();
            let extracted = match crate::archive::extract_capture(
                adapter,
                capture,
                root,
                &contexts[&capture.meta.source],
            ) {
                Ok(value) => value,
                Err(error) => {
                    stats.units_failed += 1;
                    tracing::warn!(%error, "repository extraction failed; retry later");
                    continue;
                }
            };
            let Some(crate::archive::ExtractedCapture::Conversations(conversations)) = extracted
            else {
                continue;
            };
            let Some(conversation) = conversations.first() else {
                continue;
            };
            if conversation.native_project.is_some()
                || conversation
                    .transcript_facts
                    .git
                    .as_ref()
                    .and_then(|g| g.repository_url.as_ref())
                    .is_some()
            {
                continue;
            }
            let Some(path) = conversation.project_path.clone() else {
                continue;
            };
            let branch_recorded = conversation
                .transcript_facts
                .git
                .as_ref()
                .and_then(|g| g.branch.as_ref())
                .is_some();
            (path, branch_recorded)
        };
        let key = (capture.meta.host.clone(), path.clone());
        let lookup = if let Some(answer) = sticky.get(&key) {
            answer.clone()
        } else if host == Some(capture.meta.host.as_str()) {
            match looked_up
                .entry(key.clone())
                .or_insert_with(|| {
                    resolver
                        .lookup_before(&path, deadline)
                        .map_err(|e| format!("{e:#}"))
                })
                .clone()
            {
                Ok(answer) => answer,
                Err(error) => {
                    if t3.is_none() {
                        t3 = Some(crate::adapters::t3::repository_fallbacks(
                            &snapshot, resolver,
                        )?);
                    }
                    if let Some(url) = t3
                        .as_ref()
                        .unwrap()
                        .get(&(capture.meta.source.clone(), capture.meta.native_id.clone()))
                    {
                        Lookup::Resolved(url.clone())
                    } else {
                        deferred.insert(path.clone());
                        tracing::warn!(host = capture.meta.host, path, %error, "repository lookup could not run; retry on next pull");
                        continue;
                    }
                }
            }
        } else if let Some(fact) = &capture.meta.repository_resolution {
            fact.lookup.clone()
        } else {
            continue;
        };
        let lookup = if lookup == Lookup::Missing {
            if t3.is_none() {
                t3 = Some(crate::adapters::t3::repository_fallbacks(
                    &snapshot, resolver,
                )?);
            }
            t3.as_ref()
                .expect("prepared T3 facts")
                .get(&(capture.meta.source.clone(), capture.meta.native_id.clone()))
                .map(|url| Lookup::Resolved(url.clone()))
                .unwrap_or(lookup)
        } else {
            lookup
        };
        if matches!(lookup, Lookup::Resolved(_)) {
            sticky.insert(key, lookup.clone());
        }
        let unresolved = lookup == Lookup::Missing && branch_recorded;
        let fact = Fact {
            source_revision: (capture.meta.source_mtime_ms, capture.meta.source_size),
            path,
            lookup,
            unresolved,
            branch_recorded,
        };
        if capture.meta.repository_resolution.as_ref() == Some(&fact) {
            continue;
        }
        capture.meta.repository_resolution = Some(fact);
        write_capture_meta(&capture.meta_path, &capture.meta)?;
    }
    stats.retry_later = deferred.len();
    stats.retry_sample = deferred.into_iter().next();
    Ok(stats)
}

/// Corrupt/missing archival units are isolated; operational IO errors stay explicit.
pub(crate) fn warn_bad_unit(error: &anyhow::Error, path: &Path) -> Result<()> {
    if let Some(io) = error
        .chain()
        .find_map(|e| e.downcast_ref::<std::io::Error>())
        && !matches!(
            io.kind(),
            std::io::ErrorKind::InvalidData
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::NotFound
                | std::io::ErrorKind::InvalidInput
                | std::io::ErrorKind::NotADirectory
                | std::io::ErrorKind::IsADirectory
                | std::io::ErrorKind::PermissionDenied
        )
        && !(io.kind() == std::io::ErrorKind::Other && io.raw_os_error().is_none())
        && io.raw_os_error() != Some(if cfg!(target_os = "macos") { 63 } else { 36 })
    {
        return Err(anyhow::anyhow!("{}: {error:#}", path.display()));
    }
    tracing::warn!(%error,path=%path.display(),"repository unit invalid; continuing");
    Ok(())
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn filename_errors_are_per_unit_but_disk_io_errors_remain_explicit() {
        for code in [if cfg!(target_os = "macos") { 63 } else { 36 }, 2, 13] {
            assert!(
                warn_bad_unit(
                    &std::io::Error::from_raw_os_error(code).into(),
                    Path::new("capture")
                )
                .is_ok()
            );
        }
        assert!(
            warn_bad_unit(
                &std::io::Error::from_raw_os_error(5).into(),
                Path::new("capture")
            )
            .is_err()
        );
    }
}
