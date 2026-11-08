//! Slice 15 — Capture time and Session Day Ongoing state.

use agent_sessions::model::{Message, MessageKind, Role};
use agent_sessions::session_day::is_ongoing_for;

fn message(timestamp_ms: i64) -> Message {
    Message {
        role: Role::Assistant,
        kind: MessageKind::Dialog,
        text: "done".into(),
        timestamp_ms: Some(timestamp_ms),
        actions_before: 0,
    }
}

#[test]
fn trailing_actions_make_the_last_session_day_ongoing() {
    assert!(is_ongoing_for("codex", 3_600_000, &[message(1_000)], 1));
}

#[test]
fn a_message_within_ten_minutes_of_capture_is_ongoing() {
    assert!(is_ongoing_for("codex", 600_999, &[message(1_000)], 0));
    assert!(!is_ongoing_for("codex", 601_000, &[message(1_000)], 0));
}

#[test]
fn a_later_larger_capture_can_clear_ongoing() {
    assert!(is_ongoing_for("codex", 500_000, &[message(1_000)], 1));
    assert!(!is_ongoing_for(
        "codex",
        2_000_000,
        &[message(1_000), message(1_200_000)],
        0
    ));
}

#[test]
fn hosted_old_actions_and_unanswered_users_do_not_establish_ongoing() {
    assert!(!agent_sessions::session_day::is_ongoing_for(
        "chatgpt",
        3_600_000,
        &[message(1_000)],
        4
    ));
    assert!(agent_sessions::session_day::is_ongoing_for(
        "chatgpt",
        500_000,
        &[message(1_000)],
        4
    ));
}
