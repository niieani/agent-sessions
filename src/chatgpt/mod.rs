//! ChatGPT native evidence ingestion and graph-preserving Transcript Views.
mod branches;
mod capture;
mod export_media;
mod graph;
mod ingest;
mod media;
pub mod presentation;
pub(crate) mod projects;
mod revision;
mod sessions;
mod snapshots;
mod store;
mod versions;

pub use export_media::ingest_export_media;
pub use graph::{Graph, Node};
pub use ingest::{Evidence, ingest_export, status};

use crate::adapters::{Adapter, Capture, EnumerateGate, SessionRef};
use crate::model::Conversation;
use anyhow::Result;
use std::path::Path;

pub struct ChatGptAdapter;
impl Adapter for ChatGptAdapter {
    fn capture_native_id<'a>(&self, native_id: &'a str) -> &'a str {
        native_id.split_once('~').map_or(native_id, |(id, _)| id)
    }
    fn extract_unit(
        &self,
        capture: &Capture,
        _parent: Option<&Capture>,
        context: &crate::adapters::ExtractionContext,
    ) -> Result<Option<crate::adapters::AdapterExtraction>> {
        let graph = ingest::aggregate_graph(capture, context)?;
        if !graph.has_native_history {
            return Ok(None);
        }
        Ok(Some(crate::adapters::AdapterExtraction::Conversations(
            graph.sessions(capture.provenance())?,
        )))
    }

    fn settle_bundle(
        &self,
        body: &mut dyn std::io::Read,
        meta: &crate::adapters::CaptureMeta,
        captures: &Path,
    ) -> Result<Option<crate::adapters::CaptureOutcome>> {
        let decoder = zstd::stream::read::Decoder::new(body)?;
        use std::io::BufRead;
        let records = std::io::BufReader::new(decoder)
            .lines()
            .map(|line| Ok(serde_json::from_str::<Evidence>(&line?)?))
            .collect::<Result<Vec<_>>>()?;
        for record in &records {
            anyhow::ensure!(
                ingest::hash(record.payload.as_bytes()) == record.sha256,
                "ChatGPT Capture evidence hash mismatch"
            );
        }
        Ok(Some(
            capture::merge(captures, meta.clone(), records)?.outcome,
        ))
    }

    fn prepare_extraction(
        &self,
        archive_root: &Path,
    ) -> Result<crate::adapters::ExtractionContext> {
        projects::prepare(archive_root)
    }
    fn render_material(
        &self,
        capture: &Capture,
        archive_root: &Path,
        context: &crate::adapters::ExtractionContext,
    ) -> Result<crate::model::RenderMaterial> {
        presentation::material(&ingest::aggregate_graph(capture, context)?, archive_root)
    }
    fn source(&self) -> &'static str {
        "chatgpt"
    }
    fn enumerate(&self, root: &Path, gate: &EnumerateGate) -> Result<Vec<SessionRef>> {
        store::enumerate(root, gate)
    }
    fn session_working_path(&self, _session: &SessionRef) -> Result<Option<String>> {
        Ok(None)
    }

    fn capture_working_path(&self, _capture: &Capture) -> Result<Option<String>> {
        Ok(None)
    }

    fn capture(&self, session: &SessionRef, captures: &Path, host: &str) -> Result<Capture> {
        store::capture(session, captures, host)
    }
    fn extract(&self, capture: &Capture) -> Result<Option<Conversation>> {
        let graph = ingest::read_graph(capture)?;
        if !graph.has_native_history {
            return Ok(None);
        }
        graph.validate()?;
        Ok(Some(graph.conversation(capture.provenance())?))
    }
}
