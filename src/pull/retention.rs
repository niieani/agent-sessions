use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

/// Finite application-managed retention, as observed on the exporting Host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoreRetention {
    pub days: u64,
    pub setting: String,
    pub settings_path: String,
}

pub fn claude_retention(root: &Path) -> Result<StoreRetention> {
    let path = root
        .parent()
        .context("Claude Store root has no settings parent")?
        .join("settings.json");
    let days = match std::fs::read(&path) {
        Ok(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing Claude settings {}", path.display()))?;
            let object = value
                .as_object()
                .context("Claude settings must be an object")?;
            match object.get("cleanupPeriodDays") {
                None => 30,
                Some(value) => value
                    .as_u64()
                    .context("cleanupPeriodDays must be a nonnegative integer")?,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 30,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    Ok(StoreRetention {
        days,
        setting: "cleanupPeriodDays".into(),
        settings_path: path.display().to_string(),
    })
}

/// Cover Claude's default deletion window and shorter windows, including zero.
/// Longer windows do not threaten routine collection after missed nightly Pulls.
const WARNING_RETENTION_DAYS: u64 = 30;

pub fn store_retention_warnings(stats: &super::PullStats, enabled: bool) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    stats
        .sources
        .iter()
        .filter_map(|source| {
            let retention = source.retention.as_ref()?;
            if retention.days > WARNING_RETENTION_DAYS
                || !seen.insert((&source.host, &source.source))
            {
                return None;
            }
            Some(format!(
                "{}/{}: Store Retention is {} days; {} in {} controls deletion before Capture",
                source.host,
                source.source,
                retention.days,
                retention.setting,
                retention.settings_path
            ))
        })
        .collect()
}

pub fn warn_store_retention(stats: &super::PullStats, enabled: bool) {
    for warning in store_retention_warnings(stats, enabled) {
        tracing::warn!("{warning}");
    }
}
