//! `[pull]` settings in `<archive>/config.toml`.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::timefmt::parse_duration;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PullConfig {
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default = "enabled")]
    pub warn_store_retention: bool,
    /// How long after its last successful Pull an Unreachable Host is still
    /// skipped without failing the Pull. Duration: integer + ms/s/m/h/d.
    #[serde(default = "default_unreachable_grace")]
    pub unreachable_grace: String,
}

fn enabled() -> bool {
    true
}

fn default_unreachable_grace() -> String {
    "7d".into()
}

impl Default for PullConfig {
    fn default() -> Self {
        Self {
            exclude_paths: Vec::new(),
            warn_store_retention: true,
            unreachable_grace: default_unreachable_grace(),
        }
    }
}

impl PullConfig {
    pub fn unreachable_grace(&self) -> Result<Duration> {
        parse_duration(&self.unreachable_grace).with_context(
            || "pull.unreachable-grace must be an integer followed by ms, s, m, h, or d",
        )
    }

    pub(crate) fn validate(&self) -> Result<()> {
        for pattern in &self.exclude_paths {
            glob::Pattern::new(pattern).context("invalid pull.exclude-paths glob")?;
        }
        self.unreachable_grace().map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_grace_defaults_to_seven_days() {
        assert_eq!(
            PullConfig::default().unreachable_grace().unwrap(),
            Duration::from_secs(7 * 86_400)
        );
    }
}
