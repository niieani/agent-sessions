//! Dialog identity excludes Action counts, timestamps, and View navigation.
use crate::model::Message;
use sha2::{Digest, Sha256};
pub(super) fn dialog(messages: &[Message]) -> String {
    let content = messages
        .iter()
        .map(|m| (&m.role, &m.kind, &m.text))
        .collect::<Vec<_>>();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&content).expect("Dialog serializes"))
    )
}
