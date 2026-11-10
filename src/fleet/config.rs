//! Fleet configuration: `[[host]]` entries and the top-level
//! `<archive>/config.toml` schema.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::Deserialize;

use super::view_config::ViewConfig;
use crate::adapters::AVAILABLE_SOURCES;
use crate::pull::StoreRoots;
use crate::timefmt::parse_duration;

/// Where the remote binary lives by convention: relative to the remote
/// user's home (ssh commands run from `$HOME`), overridable per Host.
pub const DEFAULT_REMOTE_BIN: &str = ".agent-sessions/bin/agent-sessions";

/// One `[[host]]` entry in `<archive>/config.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct HostConfig {
    /// The Host name recorded in Provenance and keying Bookmarks. Stable —
    /// renaming it makes the archive treat the machine as a new Host.
    pub name: String,
    /// ssh destination (`user@host` or a `~/.ssh/config` alias). Absent =
    /// this machine: its Stores are read directly, no ssh involved.
    pub ssh: Option<String>,
    /// Claude Code Store root override (default: `~/.claude/projects`).
    pub claude_root: Option<PathBuf>,
    /// Codex Store root override (default: `~/.codex`).
    pub codex_root: Option<PathBuf>,
    /// Pi Store root override (default: `~/.pi/agent/sessions`).
    pub pi_root: Option<PathBuf>,
    /// Cursor Store database override.
    pub cursor_root: Option<PathBuf>,
    /// T3 Store base override (contains `userdata/statev2.sqlite`).
    pub t3_root: Option<PathBuf>,
    pub chatgpt_root: Option<PathBuf>,
    /// Remote binary path override (default: [`DEFAULT_REMOTE_BIN`]).
    pub remote_bin: Option<String>,
    /// Override `[pull] unreachable-grace` for this Host.
    pub unreachable_grace: Option<String>,
}

impl HostConfig {
    /// The localhost entry `pull` synthesizes when no config file exists.
    pub fn localhost(
        name: String,
        claude_root: Option<PathBuf>,
        codex_root: Option<PathBuf>,
        pi_root: Option<PathBuf>,
        cursor_root: Option<PathBuf>,
        t3_root: Option<PathBuf>,
        chatgpt_root: Option<PathBuf>,
    ) -> Self {
        Self {
            name,
            ssh: None,
            claude_root,
            codex_root,
            pi_root,
            cursor_root,
            t3_root,
            chatgpt_root,
            remote_bin: None,
            unreachable_grace: None,
        }
    }

    pub fn unreachable_grace<'a>(&'a self, pull: &'a crate::pull::PullConfig) -> &'a str {
        self.unreachable_grace
            .as_deref()
            .unwrap_or(&pull.unreachable_grace)
    }

    pub(crate) fn store_roots(&self) -> StoreRoots {
        let mut roots = StoreRoots::default();
        if let Some(root) = &self.claude_root {
            roots.claude = root.clone();
        }
        if let Some(root) = &self.codex_root {
            roots.codex = root.clone();
        }
        if let Some(root) = &self.pi_root {
            roots.pi = root.clone();
        }
        if let Some(root) = &self.cursor_root {
            roots.cursor = root.clone();
        }
        if let Some(root) = &self.t3_root {
            roots.t3 = root.clone();
            roots.t3_explicit = true;
        }
        if let Some(root) = &self.chatgpt_root {
            roots.chatgpt = root.clone();
        }
        roots
    }

    pub(super) fn remote_bin(&self) -> &str {
        self.remote_bin.as_deref().unwrap_or(DEFAULT_REMOTE_BIN)
    }

    pub(super) fn source_root_override(&self, source: &str) -> Option<&Path> {
        match source {
            crate::adapters::claude::SOURCE => self.claude_root.as_deref(),
            crate::adapters::codex::SOURCE => self.codex_root.as_deref(),
            crate::adapters::pi::SOURCE => self.pi_root.as_deref(),
            crate::adapters::cursor::SOURCE => self.cursor_root.as_deref(),
            crate::adapters::t3::SOURCE => self.t3_root.as_deref(),
            "chatgpt" => self.chatgpt_root.as_deref(),
            _ => None,
        }
    }
}

/// The fleet: `[[host]]` entries in `<archive>/config.toml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FleetConfig {
    #[serde(default)]
    pub media: MediaConfig,
    #[serde(default, rename = "lead-feeds")]
    pub lead_feeds: Vec<crate::leads::FeedConfig>,
    #[serde(default, rename = "host")]
    pub hosts: Vec<HostConfig>,
    /// Regenerable View classification and layout settings.
    #[serde(default)]
    pub views: ViewConfig,
    pub digest: Option<DigestConfig>,
    #[serde(default)]
    pub pull: crate::pull::PullConfig,
    #[serde(default)]
    pub titles: IndexMap<String, crate::push_titles::TitleConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MediaConfig {
    pub materialization: Option<crate::media::MaterializationMethod>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TokenizerPromptFormat {
    #[default]
    ChatTemplate,
    /// oMLX's response_format schema injection when grammar decoding is unavailable.
    Omlx,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DigestConfig {
    pub base_url: String,
    pub model: String,
    /// Local HF model directory, containing tokenizer.json and tokenizer_config.json.
    pub tokenizer: Option<std::path::PathBuf>,
    #[serde(default)]
    pub tokenizer_prompt_format: TokenizerPromptFormat,
    pub api_key_env: String,
    #[serde(default = "default_digest_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_max_input_tokens")]
    pub max_input_tokens: usize,
    #[serde(default = "default_request_timeout")]
    pub request_timeout: String,
    #[serde(default = "default_digest_retries")]
    pub retries: usize,
    /// While the model server answers 507 (Insufficient Storage), how long to
    /// pause between single retry requests.
    #[serde(default = "default_capacity_retry_interval")]
    pub capacity_retry_interval: String,
    /// How long to keep retrying after the first 507 before ending the pass.
    #[serde(default = "default_capacity_max_wait")]
    pub capacity_max_wait: String,
    /// How often a running Digest logs its progress line and refreshes
    /// `digest-status.json` even when no work settled.
    #[serde(default = "default_progress_interval")]
    pub progress_interval: String,
    #[serde(default)]
    pub prompts: DigestPromptConfig,
    #[serde(default)]
    pub extra_body: toml::Table,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DigestPromptConfig {
    pub day: Option<String>,
    pub session_title: Option<String>,
}

fn default_digest_concurrency() -> usize {
    2
}

fn default_max_input_tokens() -> usize {
    60_000
}

fn default_request_timeout() -> String {
    "10m".into()
}

fn default_digest_retries() -> usize {
    2
}

fn default_capacity_retry_interval() -> String {
    "5m".into()
}

fn default_capacity_max_wait() -> String {
    "75m".into()
}

fn default_progress_interval() -> String {
    "5m".into()
}

impl FleetConfig {
    /// Parse a config file's contents.
    pub fn parse(toml_text: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(toml_text).context("parsing fleet config")?;
        let mut names: Vec<&str> = config.hosts.iter().map(|h| h.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() != config.hosts.len() {
            bail!("fleet config: host names must be unique");
        }
        for (source, titles) in &config.titles {
            anyhow::ensure!(
                AVAILABLE_SOURCES.contains(&source.as_str()),
                "titles: unknown Source {source:?}"
            );
            anyhow::ensure!(
                !titles.overwrite_external || matches!(source.as_str(), "codex" | "t3"),
                "titles.{source}.overwrite-external is supported only for Codex and T3"
            );
        }
        config.views.validate()?;
        config.pull.validate()?;
        for host in &config.hosts {
            parse_duration(host.unreachable_grace(&config.pull)).with_context(|| {
                format!(
                    "host {:?}: unreachable-grace must be an integer followed by ms, s, m, h, or d",
                    host.name
                )
            })?;
        }
        crate::name_tables::normalize_inline(&mut config.views)?;
        if let Some(digest) = &config.digest {
            digest.validate()?;
        }
        Ok(config)
    }

    /// Load `<archive>/config.toml`; `Ok(None)` when absent.
    pub fn load(archive_root: &Path) -> Result<Option<Self>> {
        let path = archive_root.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut config =
                    Self::parse(&text).with_context(|| format!("in {}", path.display()))?;
                crate::name_tables::merge_files(&mut config.views, archive_root)
                    .with_context(|| format!("in {}", path.display()))?;
                Ok(Some(config))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

impl DigestConfig {
    pub fn timeout(&self) -> Result<std::time::Duration> {
        parse_duration(&self.request_timeout).with_context(
            || "digest.request-timeout must be an integer followed by ms, s, m, h, or d",
        )
    }

    pub fn progress_interval(&self) -> Result<std::time::Duration> {
        let interval = parse_duration(&self.progress_interval).with_context(
            || "digest.progress-interval must be an integer followed by ms, s, m, h, or d",
        )?;
        anyhow::ensure!(
            !interval.is_zero(),
            "digest.progress-interval must be greater than zero"
        );
        Ok(interval)
    }

    pub fn capacity_retry_interval(&self) -> Result<std::time::Duration> {
        let interval = parse_duration(&self.capacity_retry_interval).with_context(
            || "digest.capacity-retry-interval must be an integer followed by ms, s, m, h, or d",
        )?;
        anyhow::ensure!(
            !interval.is_zero(),
            "digest.capacity-retry-interval must be greater than zero"
        );
        Ok(interval)
    }

    pub fn capacity_max_wait(&self) -> Result<std::time::Duration> {
        parse_duration(&self.capacity_max_wait).with_context(
            || "digest.capacity-max-wait must be an integer followed by ms, s, m, h, or d",
        )
    }

    fn validate(&self) -> Result<()> {
        if self.base_url.trim().is_empty()
            || !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://"))
        {
            bail!("digest.base-url must be an HTTP(S) URL");
        }
        if self.model.trim().is_empty() {
            bail!("digest.model must not be empty");
        }
        if self.api_key_env.trim().is_empty() {
            bail!("digest.api-key-env must not be empty");
        }
        if self.concurrency == 0 {
            bail!("digest.concurrency must be greater than zero");
        }
        if self.max_input_tokens == 0 {
            bail!("digest.max-input-tokens must be greater than zero");
        }
        if let Some(day) = &self.prompts.day {
            for placeholder in ["{classifications}", "{summary_budget}"] {
                if !day.contains(placeholder) {
                    bail!("digest.prompts.day must contain {placeholder}");
                }
            }
        }
        if self
            .prompts
            .session_title
            .as_ref()
            .is_some_and(|prompt| prompt.trim().is_empty())
        {
            bail!("digest.prompts.session-title must not be empty");
        }
        self.timeout()?;
        self.progress_interval()?;
        self.capacity_retry_interval()?;
        self.capacity_max_wait()?;
        for reserved in ["model", "messages", "response_format"] {
            if self.extra_body.contains_key(reserved) {
                bail!("digest.extra-body must not override {reserved}");
            }
        }
        Ok(())
    }
}
