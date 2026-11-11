use super::Graph;
use crate::adapters::{Capture, CaptureMeta, CaptureOutcome, SessionKind};
use crate::archive::Archive;
use anyhow::{Context, Result, ensure};
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub format: String,
    pub origin: String,
    pub captured_at: i64,
    pub sha256: String,
    pub payload: String,
}
#[derive(Default, Serialize)]
pub struct Stats {
    pub preserved: usize,
    pub unchanged: usize,
    pub extracted: usize,
    pub failed: Vec<String>,
}
fn safe_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "unsafe ChatGPT identity {id:?}"
    );
    Ok(())
}
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn read_evidence(capture: &Capture) -> Result<Vec<Evidence>> {
    let reader = BufReader::new(zstd::stream::read::Decoder::new(File::open(
        &capture.body_path,
    )?)?);
    reader
        .lines()
        .map(|line| {
            let evidence: Evidence = serde_json::from_str(&line?)?;
            ensure!(
                hash(evidence.payload.as_bytes()) == evidence.sha256,
                "ChatGPT Capture evidence hash mismatch"
            );
            Ok(evidence)
        })
        .collect()
}
pub fn read_graph(capture: &Capture) -> Result<Graph> {
    Graph::from_evidence(&capture.meta.native_id, &read_evidence(capture)?)
}
pub(super) fn aggregate_graph(
    capture: &Capture,
    context: &crate::adapters::ExtractionContext,
) -> Result<Graph> {
    let mut records = Vec::new();
    for related in context.related_captures(capture) {
        records.extend(read_evidence(related)?);
    }
    Graph::from_evidence(&capture.meta.native_id, &records)
}
pub(super) fn preserve(
    archive: &Archive,
    id: &str,
    evidence: Evidence,
    account_id: &str,
) -> Result<bool> {
    safe_id(id)?;
    ensure!(!account_id.trim().is_empty(), "accountId missing");
    let metadata = CaptureMeta {
        repository_resolution: None,
        schema_version: 1,
        source: "chatgpt".into(),
        native_id: id.into(),
        host: crate::pull::local_hostname(),
        native_path: evidence.origin.clone(),
        project_dir: None,
        kind: SessionKind::Main,
        source_mtime_ms: evidence.captured_at,
        source_size: evidence.payload.len() as u64,
        native_birth_time_ms: None,
        source_files: vec![],
        native_meta_mtime_ms: None,
        native_meta_size: None,
        captured_at_ms: evidence.captured_at,
        adapter_version: 1,
        superseded_native_ids: vec![],
        needs_retry: true,
        native_meta: Some(serde_json::json!({"accountId":account_id})),
        goal_attachments: vec![],
    };
    Ok(super::capture::merge(&archive.captures_dir(), metadata, vec![evidence])?.written)
}
pub(super) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut f = tempfile::NamedTempFile::new_in(path.parent().context("parent missing")?)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.as_file().sync_all()?;
    f.persist(path)?;
    Ok(())
}

pub(super) struct ArraySeed<'a, F>(pub(super) &'a mut F);
impl<'de, F: FnMut(Box<RawValue>) -> Result<()>> DeserializeSeed<'de> for ArraySeed<'_, F> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de, F: FnMut(Box<RawValue>) -> Result<()>> Visitor<'de> for ArraySeed<'_, F> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("conversation array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(value) = seq.next_element::<Box<RawValue>>()? {
            (self.0)(value).map_err(serde::de::Error::custom)?;
        }
        Ok(())
    }
}
pub fn ingest_export(archive: &Archive, zip: &Path, account_id: &str) -> Result<Stats> {
    let _lock = archive.acquire_writer_lock()?;
    let listing = Command::new("/usr/bin/unzip")
        .args(["-Z1"])
        .arg(zip)
        .output()?;
    ensure!(listing.status.success(), "cannot enumerate export ZIP");
    let captured_at = crate::adapters::mtime_ms(&std::fs::metadata(zip)?);
    let mut stats = Stats::default();
    let mut shards = String::from_utf8(listing.stdout)?
        .lines()
        .filter(|n| n.starts_with("conversations") && n.ends_with(".json"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    shards.sort();
    ensure!(
        !shards.is_empty(),
        "ZIP contains no conversations JSON shards"
    );
    for shard in shards {
        let mut child = Command::new("/usr/bin/unzip")
            .args(["-p"])
            .arg(zip)
            .arg(&shard)
            .stdout(Stdio::piped())
            .spawn()?;
        let input = BufReader::new(child.stdout.take().unwrap());
        let mut de = serde_json::Deserializer::from_reader(input);
        let result = ArraySeed(&mut |raw: Box<RawValue>| {
            let payload = raw.get();
            let value: Value = serde_json::from_str(payload)?;
            let id = value
                .get("id")
                .or_else(|| value.get("conversation_id"))
                .and_then(Value::as_str)
                .context("export conversation id missing")?;
            let origin = format!("export:{shard}:{id}");
            let evidence = Evidence {
                format: "export".into(),
                origin,
                captured_at,
                sha256: hash(payload.as_bytes()),
                payload: payload.into(),
            };
            if preserve(archive, id, evidence, account_id)? {
                stats.preserved += 1
            } else {
                stats.unchanged += 1
            }
            Ok(())
        })
        .deserialize(&mut de)
        .context(format!("reading {shard}"));
        if result.is_err() {
            let _ = child.kill();
        }
        let exit = child.wait()?;
        result?;
        ensure!(exit.success(), "extracting {shard} failed");
    }
    let media = super::export_media::ingest(archive, zip, account_id)?;
    stats.failed.extend(media.failed);
    let extraction = extract_import(archive)?;
    stats.extracted = extraction.extracted;
    stats.failed.extend(extraction.failed);
    Ok(stats)
}
pub fn captures(archive: &Archive) -> Result<Vec<Capture>> {
    let mut out = vec![];
    for host in std::fs::read_dir(archive.captures_dir())? {
        let base = host?.path().join("chatgpt");
        if !base.is_dir() {
            continue;
        }
        let mut sidecars = Vec::new();
        crate::adapters::collect_sidecars(&base, &mut sidecars)?;
        for meta_path in sidecars {
            let meta = serde_json::from_reader(File::open(&meta_path)?)?;
            out.push(Capture {
                body_path: crate::archive::body_for_sidecar(&meta_path)?,
                meta_path,
                meta,
                written: false,
                outcome: CaptureOutcome::Unchanged,
            });
        }
    }
    out.sort_by(|a, b| (&a.meta.native_id, &a.meta.host).cmp(&(&b.meta.native_id, &b.meta.host)));
    Ok(out)
}
pub(super) fn extract_import(archive: &Archive) -> Result<Stats> {
    let mut stats = Stats::default();
    let mut db = archive.db()?;
    let context = super::projects::prepare(archive.root())?;
    for mut capture in captures(archive)? {
        db.record_capture_presence(&capture.meta)?;
        match crate::archive::extract_capture(
            &super::ChatGptAdapter,
            &capture,
            &archive.captures_dir(),
            &context,
        ) {
            Ok(Some(crate::adapters::AdapterExtraction::Conversations(conversations))) => {
                let count = context.store_conversations(&mut db, &conversations, &capture.meta)?;
                capture.set_needs_retry(false)?;
                stats.extracted += count;
            }
            Ok(None) => {
                db.upsert_nonextractable_from_capture(&capture.meta)?;
                capture.set_needs_retry(false)?;
            }
            Ok(Some(_)) => anyhow::bail!("ChatGPT cannot produce Orchestrator facts"),
            Err(e) => stats
                .failed
                .push(format!("{}: {e:#}", capture.meta.native_id)),
        }
    }
    Ok(stats)
}
pub fn status(archive: &Archive) -> Result<Value> {
    let mut conversations = BTreeMap::new();
    let mut cutoff = super::revision::Revision::new(&serde_json::json!(0))?;
    let saved_assets = super::media::saved_asset_ids(archive)?;
    let context = super::projects::prepare(archive.root())?;
    for captures in context.aggregate_captures.values() {
        let graph = aggregate_graph(&captures[0], &context)?;
        if !graph.has_native_history {
            continue;
        }
        let revision = graph
            .native_revision
            .as_ref()
            .context("ChatGPT update_time missing")?;
        cutoff = cutoff.max(revision.clone());
        let descriptors = graph.generated_assets();
        let generated = descriptors
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let missing = generated
            .difference(&saved_assets)
            .cloned()
            .collect::<Vec<_>>();
        let complete = graph.validate().is_ok();
        let in_progress = graph.in_progress_message_ids();
        // Advertised Versions not yet evidenced. Gates checkpoints and makes the
        // collector re-collect; never a request-target list (the collector targets
        // only nodes its own responses advertise).
        let missing_versions = graph
            .advertised_versions
            .iter()
            .filter(|id| {
                !graph.fetched_versions.contains(*id)
                    && !graph.export_covered_versions.contains(*id)
            })
            .collect::<Vec<_>>();
        conversations.insert(graph.id.clone(),serde_json::json!({"updatedAt":revision.number,"messageIds":graph.nodes.keys().collect::<Vec<_>>(),"complete":complete,"missingVersions":missing_versions,"exportCoveredVersionIds":graph.export_covered_versions,"inProgressMessageIds":in_progress,"checkpointEligible":complete && missing_versions.is_empty() && in_progress.is_empty(),"title":graph.title,"generatedAssetIds":generated,"generatedAssets":descriptors.values().collect::<Vec<_>>(),"savedGeneratedAssetIds":generated.intersection(&saved_assets).collect::<Vec<_>>(),"missingGeneratedAssetIds":missing,"mediaComplete":missing.is_empty()}));
    }
    for (id, entry) in &mut conversations {
        if let Some(Some(project)) = context.native_projects.get(id) {
            entry["nativeProject"] = serde_json::to_value(project)?;
        }
    }
    Ok(
        serde_json::json!({"format":"asessions-chatgpt-status-v1","source":"chatgpt","cutoff":cutoff.number,"conversations":conversations}),
    )
}
