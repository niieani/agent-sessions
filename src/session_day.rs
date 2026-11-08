//! Session Day behavior shared by Render, Digest, and activity output.

use crate::model::Message;

/// Whether the final Session Day may still have been running at Capture.
///
/// Only the Session's final day can be Ongoing. Callers pass that day's
/// Messages and the Conversation's trailing Action count. A Message exactly
/// ten minutes before Capture is closed; absent timestamps cannot establish
/// recency.
pub fn is_ongoing_for(
    source: &str,
    captured_at_ms: i64,
    messages: &[Message],
    trailing_actions: u32,
) -> bool {
    if trailing_actions > 0 && !crate::adapters::is_hosted_source(source) {
        return true;
    }
    messages
        .iter()
        .rev()
        .find_map(|message| message.timestamp_ms)
        .is_some_and(|last_message_ms| {
            captured_at_ms >= last_message_ms && (captured_at_ms - last_message_ms) < 600_000
        })
}
