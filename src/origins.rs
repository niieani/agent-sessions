//! Durable origin attribution, independent of the Capture supplying Dialog.
use crate::{
    archive::{Archive, Db},
    leads::directory,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OriginClaim {
    pub source: String,
    pub native_id: String,
    pub host: String,
    pub kind: ClaimKind,
    pub reference: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimKind {
    T3Execution,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostAttribution {
    pub schema_version: u32,
    pub source: String,
    pub native_id: String,
    pub host: String,
    pub status: AttributionStatus,
    pub claims: BTreeSet<OriginClaim>,
    pub birth_times: BTreeMap<String, Option<i64>>,
    pub birth_matches_start: Option<bool>,
    pub reason: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttributionStatus {
    Confirmed,
    Ambiguous,
    Unverified,
}
pub type Attributions = BTreeMap<(String, String), HostAttribution>;

fn path(root: &Path, source: &str, native_id: &str) -> Result<PathBuf> {
    ensure!(
        crate::adapters::adapter_for(source).is_some(),
        "unknown origin Source {source:?}"
    );
    ensure!(
        !native_id.trim().is_empty() && !native_id.chars().any(char::is_control),
        "invalid origin identity"
    );
    Ok(root.join("host-attributions").join(source).join(format!(
        "{}.json",
        crate::path_key::identity_component(native_id)
    )))
}
impl HostAttribution {
    pub fn copy_hosts(&self, hosts: &[String]) -> Vec<String> {
        if self.status != AttributionStatus::Confirmed {
            return Vec::new();
        }
        hosts
            .iter()
            .filter(|host| *host != &self.host)
            .cloned()
            .collect()
    }
}
/// Only the previous choice is durable. Evidence is always re-derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostPreference {
    pub schema_version: u32,
    pub source: String,
    pub native_id: String,
    pub host: String,
}
impl HostPreference {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 2,
            "unsupported host preference schema; remove round-1 claim records before backfill"
        );
        ensure!(!self.host.trim().is_empty(), "empty origin Host");
        Ok(())
    }
}
type Preferences = BTreeMap<(String, String), HostPreference>;

pub fn lookup(root: &Path, source: &str, native_id: &str) -> Result<Option<HostAttribution>> {
    let db_path = root.join("archive.db");
    if !db_path.exists() {
        return Ok(None);
    }
    Db::open_read_only(&db_path)?.host_attribution(source, native_id)
}
pub fn load(root: &Path) -> Result<Preferences> {
    let mut records = Preferences::new();
    let base = root.join("host-attributions");
    if !directory(&base, false)? {
        return Ok(records);
    }
    for source in fs::read_dir(&base)? {
        let source = source?;
        ensure!(
            source.file_type()?.is_dir(),
            "unexpected host attribution entry {}",
            source.path().display()
        );
        for entry in fs::read_dir(source.path())? {
            let entry = entry?;
            ensure!(
                entry.file_type()?.is_file(),
                "origin record is not a regular file"
            );
            let record: HostPreference = serde_json::from_reader(fs::File::open(entry.path())?)
                .with_context(|| format!("reading {}", entry.path().display()))?;
            record.validate()?;
            ensure!(
                entry.path() == path(root, &record.source, &record.native_id)?,
                "origin identity/path mismatch"
            );
            ensure!(
                records
                    .insert((record.source.clone(), record.native_id.clone()), record)
                    .is_none(),
                "duplicate origin record"
            );
        }
    }
    Ok(records)
}
fn store(root: &Path, record: &HostPreference) -> Result<()> {
    record.validate()?;
    let target = path(root, &record.source, &record.native_id)?;
    directory(&root.join("host-attributions"), true)?;
    directory(target.parent().unwrap(), true)?;
    if target.exists() {
        ensure!(
            fs::symlink_metadata(&target)?.file_type().is_file(),
            "origin record is not a regular file"
        );
    }
    let mut bytes = serde_json::to_vec_pretty(record)?;
    bytes.push(b'\n');
    if fs::read(&target).ok().as_ref() == Some(&bytes) {
        return Ok(());
    }
    let mut staged = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(&target)?;
    fs::File::open(target.parent().unwrap())?.sync_all()?;
    Ok(())
}
/// Freeze existing attribution before ingestion can change the projection winner.
/// Rebuild never reads the old database.
pub fn preserve_current(archive: &Archive, db: &Db) -> Result<()> {
    let preferences = load(archive.root())?;
    for (source, native_id, winner, _) in db.origin_sessions()? {
        if crate::adapters::is_hosted_source(&source) {
            continue;
        }
        if !preferences.contains_key(&(source.clone(), native_id.clone())) {
            let host = db
                .previous_origin_host(&source, &native_id)?
                .unwrap_or(winner);
            store(
                archive.root(),
                &HostPreference {
                    schema_version: 2,
                    source,
                    native_id,
                    host,
                },
            )?;
        }
    }
    Ok(())
}

type CapturedBirths = BTreeMap<(String, String), BTreeMap<String, Option<i64>>>;

fn captured_births(archive: &Archive) -> Result<CapturedBirths> {
    let mut paths = Vec::new();
    crate::adapters::collect_sidecars(&archive.captures_dir(), &mut paths)?;
    let mut births = BTreeMap::new();
    for path in paths {
        let meta: crate::adapters::CaptureMeta = serde_json::from_reader(fs::File::open(&path)?)
            .with_context(|| format!("reading birth evidence {}", path.display()))?;
        births
            .entry((meta.source, meta.native_id))
            .or_insert_with(BTreeMap::new)
            .insert(meta.host, meta.native_birth_time_ms);
    }
    Ok(births)
}

impl HostAttribution {
    fn settle(&mut self, hosts: &[String], first_record_ms: Option<i64>) {
        let mut times: Vec<_> = self
            .birth_times
            .iter()
            .filter_map(|(h, t)| t.map(|t| (t, h.clone())))
            .collect();
        times.sort();
        if hosts.len() > 1
            && times.len() == hosts.len()
            && times[1].0.saturating_sub(times[0].0) >= 1000
        {
            self.host = times[0].1.clone();
            self.status = AttributionStatus::Confirmed;
            self.birth_matches_start = first_record_ms.map(|t| times[0].0.abs_diff(t) <= 1000);
            self.reason = "earliest native birth; gap >= 1s".into();
            return;
        }
        let candidates: BTreeSet<_> = self.claims.iter().map(|c| c.host.clone()).collect();
        if candidates.len() == 1 && hosts.contains(candidates.first().unwrap()) {
            self.host = candidates.into_iter().next().unwrap();
            self.status = AttributionStatus::Confirmed;
            self.reason = "exact T3 execution; birth evidence inconclusive".into();
        } else {
            self.status = if hosts.len() > 1 {
                AttributionStatus::Ambiguous
            } else {
                AttributionStatus::Unverified
            };
            self.reason = "birth evidence missing or tied; retaining previous Host".into();
        }
    }
}

fn decision_needs_warning(
    record: &HostAttribution,
    previous: Option<(&str, Option<&str>)>,
    hosts: usize,
) -> bool {
    previous.is_some_and(|(host, _)| host != record.host)
        || record.claims.iter().any(|claim| claim.host != record.host)
        || (record.status == AttributionStatus::Ambiguous
            && hosts > 1
            && previous.is_none_or(|(_, status)| status != Some("ambiguous")))
}

/// Previous indexed decisions used only to control diagnostic verbosity.
pub(crate) type Decisions = BTreeMap<(String, String), (String, Option<String>)>;

/// Run once after the complete Pull has settled. Captures are the birth evidence;
/// T3 facts come only from the current projection index, never preference files.
pub fn reconcile(archive: &Archive, db: &mut Db) -> Result<()> {
    reconcile_with_previous(archive, db, &Decisions::new())
}

pub(crate) fn reconcile_with_previous(
    archive: &Archive,
    db: &mut Db,
    prior: &Decisions,
) -> Result<()> {
    let preferences = load(archive.root())?;
    let mut births = captured_births(archive)?;
    let mut evidence = db.origin_claims()?;
    let mut records = Attributions::new();
    let starts = db.origin_start_times()?;
    for (source, native_id, winner, hosts) in db.origin_sessions()? {
        if crate::adapters::is_hosted_source(&source) {
            continue;
        }
        let key = (source.clone(), native_id.clone());
        let host = preferences.get(&key).map_or(winner, |p| p.host.clone());
        let observed = births.remove(&key).unwrap_or_default();
        let mut record = HostAttribution {
            schema_version: 2,
            source,
            native_id,
            host,
            status: AttributionStatus::Unverified,
            claims: evidence
                .remove(&key)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            birth_times: hosts
                .iter()
                .map(|h| (h.clone(), observed.get(h).copied().flatten()))
                .collect(),
            birth_matches_start: None,
            reason: String::new(),
        };
        record.settle(&hosts, starts.get(&key).copied().flatten());
        store(
            archive.root(),
            &HostPreference {
                schema_version: 2,
                source: record.source.clone(),
                native_id: record.native_id.clone(),
                host: record.host.clone(),
            },
        )?;
        let previous = db
            .previous_origin_decision(&record.source, &record.native_id)?
            .or_else(|| prior.get(&key).cloned());
        if decision_needs_warning(
            &record,
            previous
                .as_ref()
                .map(|(host, status)| (host.as_str(), status.as_deref())),
            hosts.len(),
        ) {
            tracing::warn!(source=record.source,native_id=record.native_id,retained_host=record.host,birth_times=?record.birth_times,previous_host=?previous.as_ref().map(|(host, _)| host),status=?record.status,claims=?record.claims,reason=record.reason,"origin decision requires attention");
        } else if record.status == AttributionStatus::Ambiguous && hosts.len() > 1 {
            tracing::debug!(
                source = record.source,
                native_id = record.native_id,
                retained_host = record.host,
                "origin ambiguity unchanged"
            );
        }
        records.insert(key, record);
    }
    db.replace_host_attributions(&records)
}

#[cfg(test)]
mod logging_tests {
    use super::*;
    #[test]
    fn stable_ambiguity_is_quiet_but_new_decisions_and_conflicts_warn() {
        let record = HostAttribution {
            schema_version: 2,
            source: "claude".into(),
            native_id: "session".into(),
            host: "studio".into(),
            status: AttributionStatus::Ambiguous,
            claims: BTreeSet::new(),
            birth_times: BTreeMap::new(),
            birth_matches_start: None,
            reason: String::new(),
        };
        assert!(decision_needs_warning(&record, None, 2));
        assert!(!decision_needs_warning(
            &record,
            Some((&record.host, Some("ambiguous"))),
            2
        ));
        let mut prior = record.clone();
        prior.host = "laptop".into();
        assert!(decision_needs_warning(
            &record,
            Some((
                &prior.host,
                Some(if prior.status == AttributionStatus::Confirmed {
                    "confirmed"
                } else {
                    "ambiguous"
                })
            )),
            2
        ));
        prior = record.clone();
        prior.status = AttributionStatus::Confirmed;
        assert!(decision_needs_warning(
            &record,
            Some((
                &prior.host,
                Some(if prior.status == AttributionStatus::Confirmed {
                    "confirmed"
                } else {
                    "ambiguous"
                })
            )),
            2
        ));
        let mut conflict = record.clone();
        for host in ["studio", "laptop"] {
            conflict.claims.insert(OriginClaim {
                source: "claude".into(),
                native_id: "session".into(),
                host: host.into(),
                kind: ClaimKind::T3Execution,
                reference: "t3:thread".into(),
            });
        }
        assert!(decision_needs_warning(
            &conflict,
            Some((&conflict.host, Some("ambiguous"))),
            2
        ));
    }
}
