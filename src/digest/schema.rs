use std::collections::BTreeSet;

use anyhow::Result;
use serde::Deserialize;

use super::client::CompletionKind;

pub const CLASSIFICATIONS: &[&str] = &[
    "build",
    "fix",
    "research",
    "qa",
    "test",
    "write",
    "plan",
    "ops",
    "automation",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DayOutput {
    pub topic: Option<String>,
    pub title: String,
    /// `None` only from a continuation that keeps the known Intent.
    pub intent: Option<String>,
    pub tldr: Option<String>,
    pub summary: String,
    pub classification: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FirstDayOutput {
    topic: String,
    title: String,
    intent: String,
    tldr: Option<String>,
    summary: String,
    classification: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContinuationDayOutput {
    title: String,
    intent: Option<String>,
    tldr: Option<String>,
    summary: String,
    classification: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IntentEstablishingDayOutput {
    title: String,
    intent: String,
    tldr: Option<String>,
    summary: String,
    classification: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTitleOutput {
    pub title: String,
}

pub fn parse_day_output(raw: &str, kind: CompletionKind) -> Result<DayOutput> {
    match kind {
        CompletionKind::FirstSessionDay => {
            let value: FirstDayOutput = serde_json::from_str(raw)?;
            anyhow::ensure!(!value.topic.trim().is_empty(), "Digest Topic is empty");
            Ok(DayOutput {
                topic: Some(value.topic),
                title: value.title,
                intent: Some(value.intent),
                tldr: value.tldr,
                summary: value.summary,
                classification: value.classification,
            })
        }
        CompletionKind::ContinuationSessionDay => {
            let value: ContinuationDayOutput = serde_json::from_str(raw)?;
            Ok(DayOutput {
                topic: None,
                title: value.title,
                intent: value.intent,
                tldr: value.tldr,
                summary: value.summary,
                classification: value.classification,
            })
        }
        CompletionKind::ContinuationSessionDayWithoutIntent => {
            let value: IntentEstablishingDayOutput = serde_json::from_str(raw)?;
            Ok(DayOutput {
                topic: None,
                title: value.title,
                intent: Some(value.intent),
                tldr: value.tldr,
                summary: value.summary,
                classification: value.classification,
            })
        }
        CompletionKind::SessionTitle => anyhow::bail!("Session title is not a Session Day output"),
    }
}

pub fn validate_day_output(output: &DayOutput) -> Result<()> {
    anyhow::ensure!(!output.title.trim().is_empty(), "Digest title is empty");
    anyhow::ensure!(!output.summary.trim().is_empty(), "Digest summary is empty");
    if let Some(intent) = &output.intent {
        anyhow::ensure!(!intent.trim().is_empty(), "Digest intent is empty");
    }
    if let Some(tldr) = &output.tldr {
        anyhow::ensure!(!tldr.trim().is_empty(), "Digest TLDR is empty");
    }
    anyhow::ensure!(
        !output.classification.is_empty(),
        "Digest classification is empty"
    );
    let mut seen = BTreeSet::new();
    for value in &output.classification {
        anyhow::ensure!(
            CLASSIFICATIONS.contains(&value.as_str()),
            "unknown Digest classification {value:?}"
        );
        anyhow::ensure!(
            seen.insert(value),
            "duplicate Digest classification {value:?}"
        );
    }
    Ok(())
}
