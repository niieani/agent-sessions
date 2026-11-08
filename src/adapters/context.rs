//! Operation-scoped Source context, including independently preserved Captures.
use super::{Capture, CaptureMeta, ExtractionContext};
use crate::{archive::Db, model::Conversation};
use anyhow::Result;
impl ExtractionContext {
    /// Aggregated Sources refresh every contributing projection together. Host
    /// election can then choose Provenance without dropping Source evidence.
    pub(crate) fn store_conversations(
        &self,
        db: &mut Db,
        conversations: &[Conversation],
        meta: &CaptureMeta,
    ) -> Result<usize> {
        let Some(captures) = self.aggregate_captures.get(&meta.native_id) else {
            return db.upsert_conversations_from_capture(conversations, meta);
        };
        let captured_at = captures
            .iter()
            .map(|c| c.meta.captured_at_ms)
            .max()
            .expect("nonempty Capture group");
        for capture in captures {
            let mut projection_meta = capture.meta.clone();
            projection_meta.captured_at_ms = captured_at;
            let mut projections = conversations.to_vec();
            for conversation in &mut projections {
                conversation.provenance.host = capture.meta.host.clone();
                conversation.provenance.native_path = capture.meta.native_path.clone();
            }
            db.upsert_conversations_from_capture(&projections, &projection_meta)?;
        }
        Ok(conversations.len())
    }
    pub(crate) fn related_captures<'a>(&'a self, capture: &'a Capture) -> &'a [Capture] {
        self.aggregate_captures
            .get(&capture.meta.native_id)
            .map_or_else(|| std::slice::from_ref(capture), Vec::as_slice)
    }
}
