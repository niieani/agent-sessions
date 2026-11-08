use anyhow::Result;

/// A validated model output.
pub(super) enum Checked<T> {
    Valid(T),
    /// Rejected with a correction while attempts remain; once they are
    /// exhausted, `fallback` is accepted instead of failing.
    Correctable {
        fallback: T,
        error: anyhow::Error,
    },
}

use super::process::Worker;
use super::{DigestStats, ModelCorrection, ModelRequest, files};

pub(super) fn complete_validated<T>(
    worker: &Worker<'_>,
    stats: &mut DigestStats,
    source: &str,
    native_id: &str,
    workday: &str,
    mut request: ModelRequest,
    mut validate: impl FnMut(&str) -> Result<Checked<T>>,
) -> Result<T> {
    let (archive, client, config) = (worker.archive, worker.client, worker.config);
    let sidecar = files::digest_dir(archive.root(), source, native_id)?
        .join(format!("{workday}.rejected.jsonl"));
    for attempt in 1..=config.retries.saturating_add(1) {
        anyhow::ensure!(
            worker.counter.count(&request)? <= config.max_input_tokens,
            "Digest request including corrections exceeds max-input-tokens"
        );
        stats.requests += 1;
        let attempted_at_ms = crate::adapters::now_ms();
        let (raw, checked) = match client.complete(&request) {
            Ok(raw) => {
                let checked = validate(&raw);
                (raw, checked)
            }
            Err(error) => match error.downcast_ref::<super::client::InvalidCompletion>() {
                Some(invalid) => (invalid.raw.clone(), Err(error)),
                None => return Err(error),
            },
        };
        let (error, fallback) = match checked {
            Ok(Checked::Valid(value)) => return Ok(value),
            Ok(Checked::Correctable { fallback, error }) => (error, Some(fallback)),
            Err(error) => (error, None),
        };
        let error_text = format!("{error:#}");
        files::append_rejected(
            archive.root(),
            &sidecar,
            &files::RejectedDigest {
                attempted_at_ms,
                kind: request.kind.sidecar_kind(),
                attempt,
                error: &error_text,
                raw: &raw,
            },
        )?;
        worker.progress.rejected();
        tracing::warn!(
            source,
            session = native_id,
            workday,
            kind = request.kind.sidecar_kind(),
            attempt,
            error = error_text,
            "Digest rejected"
        );
        if attempt > config.retries {
            return match fallback {
                Some(fallback) => {
                    tracing::warn!(
                        source,
                        session = native_id,
                        workday,
                        error = error_text,
                        "Digest retries exhausted; accepting the corrected fallback"
                    );
                    Ok(fallback)
                }
                None => Err(error),
            };
        }
        request.corrections.push(ModelCorrection {
            raw,
            error: error_text,
        });
    }
    unreachable!("Digest retry loop always returns")
}

pub(super) fn valid_session_title(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.chars().count() <= 80
}
