use anyhow::Result;

use super::{DigestOptions, DigestStats, ModelClient, ModelRequest};
pub(super) fn day_selected(options: &DigestOptions, day: &str) -> bool {
    if let Some(wanted) = &options.day {
        return wanted == day;
    }
    match (&options.from, &options.through) {
        (Some(from), Some(through)) => from.as_str() <= day && day <= through.as_str(),
        _ => true,
    }
}

pub(super) fn merge_stats(total: &mut DigestStats, mut addition: DigestStats) {
    total.candidates += addition.candidates;
    total.requests += addition.requests;
    total.day_written += addition.day_written;
    total.session_written += addition.session_written;
    total.failed += addition.failed;
    total.input_tokens += addition.input_tokens;
    for (source, tokens) in addition.input_tokens_by_source {
        *total.input_tokens_by_source.entry(source).or_default() += tokens;
    }
    total.previews.append(&mut addition.previews);
}

pub(super) struct UnusedClient;

impl ModelClient for UnusedClient {
    fn complete(&self, _request: &ModelRequest) -> Result<String> {
        anyhow::bail!("dry-run attempted a model request")
    }
}
