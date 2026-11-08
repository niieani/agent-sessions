//! Sidecar metadata merging for Bundle ingest: what a retained Capture's
//! Provenance takes from an incoming sidecar whose body it did not adopt.

use crate::adapters::{CaptureMeta, CapturedGoalAttachment, GoalAttachmentStatus};

pub(super) fn capture_metadata_is_newer(incoming: &CaptureMeta, existing: &CaptureMeta) -> bool {
    incoming.repository_resolution != existing.repository_resolution
        || incoming.native_birth_time_ms != existing.native_birth_time_ms
        || incoming.adapter_version > existing.adapter_version
        || native_meta_is_newer(incoming, existing)
        || merged_goal_attachments(incoming, existing) != existing.goal_attachments
        || incoming
            .superseded_native_ids
            .iter()
            .any(|id| !existing.superseded_native_ids.contains(id))
}

pub(super) fn t3_attachment_body_is_richer(incoming: &CaptureMeta, existing: &CaptureMeta) -> bool {
    if incoming.source != crate::adapters::t3::SOURCE {
        return false;
    }
    let statuses = |meta: &CaptureMeta| {
        meta.native_meta
            .as_ref()
            .and_then(|value| value["attachments"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|attachment| {
                Some((
                    attachment["attachmentId"].as_str()?.to_owned(),
                    attachment["status"].as_str()?.to_owned(),
                ))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let incoming = statuses(incoming);
    let existing = statuses(existing);
    incoming.iter().any(|(id, status)| {
        status == "captured" && existing.get(id).is_none_or(|status| status != "captured")
    })
}

pub(super) fn merge_capture_metadata(
    existing: &CaptureMeta,
    incoming: &CaptureMeta,
) -> CaptureMeta {
    let mut merged = existing.clone();
    if !existing
        .repository_resolution
        .as_ref()
        .is_some_and(|f| matches!(f.lookup, crate::repository_resolution::Lookup::Resolved(_)))
        && incoming.repository_resolution.is_some()
    {
        merged.repository_resolution = incoming.repository_resolution.clone();
    }
    merged.schema_version = merged.schema_version.max(incoming.schema_version);
    merged.adapter_version = merged.adapter_version.max(incoming.adapter_version);
    merged.captured_at_ms = merged.captured_at_ms.max(incoming.captured_at_ms);
    merged.goal_attachments = merged_goal_attachments(incoming, existing);
    if incoming.captured_at_ms >= existing.captured_at_ms {
        merged.native_birth_time_ms = incoming.native_birth_time_ms;
    }
    merged
        .superseded_native_ids
        .extend(incoming.superseded_native_ids.iter().cloned());
    merged.superseded_native_ids.sort();
    merged.superseded_native_ids.dedup();
    if native_meta_is_newer(incoming, existing) {
        merged.native_meta = incoming.native_meta.clone();
        merged.native_meta_mtime_ms = incoming.native_meta_mtime_ms;
        merged.native_meta_size = incoming.native_meta_size;
    }
    merged
}

fn merged_goal_attachments(
    incoming: &CaptureMeta,
    existing: &CaptureMeta,
) -> Vec<CapturedGoalAttachment> {
    let mut merged = existing.goal_attachments.clone();
    for candidate in &incoming.goal_attachments {
        match merged
            .iter_mut()
            .find(|stored| stored.native_path == candidate.native_path)
        {
            None => merged.push(candidate.clone()),
            Some(stored)
                if should_replace_goal_attachment(candidate, stored, incoming, existing) =>
            {
                *stored = candidate.clone();
            }
            Some(_) => {}
        }
    }
    merged
}

fn should_replace_goal_attachment(
    incoming: &CapturedGoalAttachment,
    existing: &CapturedGoalAttachment,
    incoming_meta: &CaptureMeta,
    existing_meta: &CaptureMeta,
) -> bool {
    match (incoming.status, existing.status) {
        (GoalAttachmentStatus::Resolved, GoalAttachmentStatus::Resolved) => {
            incoming.text != existing.text
                && incoming_meta.captured_at_ms > existing_meta.captured_at_ms
        }
        (GoalAttachmentStatus::Resolved, _) => true,
        (_, GoalAttachmentStatus::Resolved | GoalAttachmentStatus::Rejected) => false,
        (GoalAttachmentStatus::Rejected, _) => true,
        _ => false,
    }
}

fn native_meta_is_newer(incoming: &CaptureMeta, existing: &CaptureMeta) -> bool {
    match (
        incoming.native_meta_mtime_ms.zip(incoming.native_meta_size),
        existing.native_meta_mtime_ms.zip(existing.native_meta_size),
    ) {
        (Some(incoming), Some(existing)) => incoming > existing,
        (Some(_), None) => true,
        _ => false,
    }
}
