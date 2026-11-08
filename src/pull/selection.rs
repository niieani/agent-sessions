//! Capture admission and same-Host copy election, shared by Pull and Export.
use crate::adapters::{Adapter, Capture, EnumerateGate, SessionRef};
use anyhow::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(crate) fn excluded_capture(
    adapter: &dyn Adapter,
    capture: &Capture,
    patterns: &[String],
) -> Result<bool> {
    if patterns.is_empty() {
        return Ok(false);
    }
    let path = adapter.capture_working_path(capture)?;
    matches(
        path.as_deref().or(capture.meta.project_dir.as_deref()),
        patterns,
    )
}

fn matches(path: Option<&str>, patterns: &[String]) -> Result<bool> {
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    for pattern in patterns {
        let pattern = glob::Pattern::new(&crate::render::expand_home(pattern))?;
        if path.is_some_and(|path| pattern.matches_path_with(Path::new(path), options)) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug)]
pub(crate) struct Enumeration {
    pub sessions: Vec<SessionRef>,
    pub retry_later: BTreeMap<String, String>,
    pub failed: BTreeMap<String, String>,
}

pub(crate) fn enumerate(
    adapter: &dyn Adapter,
    root: &Path,
    gate: &EnumerateGate,
    patterns: &[String],
) -> Result<Enumeration> {
    // Elect over the complete Store, never over just the changed copies.
    let all = adapter.enumerate(root, &EnumerateGate::default())?;
    let mut winners = BTreeMap::<String, SessionRef>::new();
    let mut retry_later = BTreeMap::new();
    let mut failed = BTreeMap::new();
    for session in all {
        if !patterns.is_empty() {
            let path = match adapter.session_working_path(&session) {
                Ok(path) => path,
                Err(error) if error.is::<crate::adapters::BusyCapture>() => {
                    retry_later.insert(session.native_id.clone(), format!("{error:#}"));
                    continue;
                }
                Err(error) => {
                    failed.insert(session.native_id.clone(), format!("{error:#}"));
                    continue;
                }
            };
            if matches(path.as_deref().or(session.project_dir.as_deref()), patterns)? {
                continue;
            }
        }
        let entry = winners
            .entry(session.native_id.clone())
            .or_insert_with(|| session.clone());
        if session.path < entry.path {
            *entry = session;
        }
    }
    if gate.since().is_some() {
        // Preserve each Adapter's own incremental and retry semantics, after
        // election. A losing copy can never become winner just by changing.
        let eligible: BTreeSet<_> = adapter
            .enumerate(root, gate)?
            .into_iter()
            .map(|s| (s.native_id, s.path))
            .collect();
        winners.retain(|id, session| {
            gate.captured_file(id)
                .is_some_and(|file| file.native_path != session.path)
                || eligible.contains(&(id.clone(), session.path.clone()))
        });
    }
    // Unknown admission on any copy defers this identity; never substitute a
    // losing copy or publish an unverified, possibly excluded Capture.
    // A permanent probe error takes precedence over a busy sibling copy;
    // each Session identity contributes exactly one failed/deferred unit.
    retry_later.retain(|id, _| !failed.contains_key(id));
    winners.retain(|id, _| !retry_later.contains_key(id) && !failed.contains_key(id));
    Ok(Enumeration {
        sessions: winners.into_values().collect(),
        retry_later,
        failed,
    })
}

/// A policy change rebuilds derived state once; durable evidence is untouched.
pub(crate) fn reconcile_index(archive: &crate::archive::Archive) -> Result<()> {
    let config = crate::fleet::FleetConfig::load(archive.root())?.unwrap_or_default();
    if archive
        .db()?
        .capture_exclusions_changed(&config.pull.exclude_paths)?
    {
        let stats = crate::archive::rebuild_locked(archive)?;
        anyhow::ensure!(
            stats.captures_failed == 0,
            "Capture exclusion index rebuild failed for {} units",
            stats.captures_failed
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::claude::ClaudeAdapter;

    struct Probe {
        busy: bool,
        allow_capture: bool,
    }
    impl Adapter for Probe {
        fn source(&self) -> &'static str {
            "claude"
        }
        fn enumerate(&self, root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>> {
            ClaudeAdapter.enumerate(root, gate)
        }
        fn session_working_path(&self, session: &SessionRef) -> Result<Option<String>> {
            if session.native_id == "broken" {
                if self.busy {
                    return Err(
                        crate::adapters::BusyCapture(session.path.display().to_string()).into(),
                    );
                }
                anyhow::bail!("metadata unavailable")
            }
            Ok(None)
        }
        fn capture(&self, session: &SessionRef, dest: &Path, host: &str) -> Result<Capture> {
            assert!(self.allow_capture, "admission must never capture");
            ClaudeAdapter.capture(session, dest, host)
        }
        fn extract(&self, capture: &Capture) -> Result<Option<crate::model::Conversation>> {
            assert!(self.allow_capture, "admission must never extract");
            ClaudeAdapter.extract(capture)
        }
    }

    #[test]
    fn admission_errors_are_isolated_and_never_capture() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        for id in ["broken", "healthy"] {
            std::fs::write(project.join(format!("{id}.jsonl")), "{}\n").unwrap();
        }
        for busy in [false, true] {
            let result = enumerate(
                &Probe {
                    busy,
                    allow_capture: false,
                },
                dir.path(),
                &EnumerateGate::default(),
                &["**/eval/**".into()],
            )
            .unwrap();
            assert_eq!(result.sessions.len(), 1);
            assert_eq!(result.sessions[0].native_id, "healthy");
            assert_eq!(result.retry_later.len(), usize::from(busy));
            assert_eq!(result.failed.len(), usize::from(!busy));
        }
    }
    #[test]
    fn failed_admission_counts_one_unit_and_retries_next_pull() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("store/project");
        std::fs::create_dir_all(&project).unwrap();
        for id in ["broken", "healthy"] {
            std::fs::write(project.join(format!("{id}.jsonl")), "{}\n").unwrap();
        }
        let archive = crate::archive::Archive::open(&dir.path().join("archive")).unwrap();
        std::fs::write(
            archive.root().join("config.toml"),
            "[pull]\nexclude-paths = ['**/eval/**']\n",
        )
        .unwrap();
        let mut db = archive.db().unwrap();
        for _ in 0..2 {
            let stats = super::super::ingest_source(
                &mut db,
                &archive,
                &Probe {
                    busy: false,
                    allow_capture: true,
                },
                &dir.path().join("store"),
                "fixture",
            )
            .unwrap();
            assert_eq!(stats.sessions_found, 2);
            assert_eq!(stats.sessions_failed, 1);
            assert_eq!(db.bookmark("fixture", "claude").unwrap(), None);
            assert!(
                archive
                    .captures_dir()
                    .join("fixture/claude/healthy.meta.json")
                    .exists()
            );
        }
    }

    #[test]
    fn codex_admission_does_not_stream_a_capture() {
        crate::adapters::file_capture::AFTER_STREAM.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(|| panic!("admission captured native content")));
        });
        let result = enumerate(
            &crate::adapters::codex::CodexAdapter,
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/store"),
            &EnumerateGate::default(),
            &["**/eval/**".into()],
        );
        crate::adapters::file_capture::AFTER_STREAM.with(|hook| *hook.borrow_mut() = None);
        let result = result.unwrap();
        assert!(!result.sessions.is_empty());
        assert!(result.failed.is_empty());
    }
}
