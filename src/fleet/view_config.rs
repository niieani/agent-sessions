//! `[views]` configuration: View Kind classification, layout, and names.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use regex::Regex;
use serde::Deserialize;

/// `[views]` settings in `<archive>/config.toml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ViewConfig {
    #[serde(default)]
    pub kind: IndexMap<String, ViewKindConfig>,
    /// Workday definition shared by Render and activity queries.
    #[serde(flatten)]
    pub workday: crate::workday::WorkdaySettings,
    #[serde(default)]
    pub unit: ViewUnit,
    pub output: Option<PathBuf>,
    pub template: Option<PathBuf>,
    pub index_template: Option<PathBuf>,
    pub layout: Option<ViewLayoutConfig>,
    #[serde(default)]
    pub source_names: IndexMap<String, String>,
    #[serde(default)]
    pub host_names: IndexMap<String, String>,
    /// Keys are absolute recorded paths or normalized repository keys.
    #[serde(default)]
    pub project_names: IndexMap<String, String>,
    /// Name Table files merged into the tables above at load; relative paths
    /// resolve against the Archive root.
    #[serde(default)]
    pub name_tables: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ViewUnit {
    #[default]
    Session,
    SessionDay,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ViewLayoutConfig {
    pub session: Option<String>,
    pub loose: Option<String>,
    pub subagent: Option<String>,
    pub branch: Option<String>,
    pub project_index: Option<String>,
    pub loose_index: Option<String>,
    pub json: Option<String>,
}

/// One ordered, named View classification and its Treatments.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ViewKindConfig {
    #[serde(rename = "match")]
    pub matches: Option<OneOrMany<ViewMatchConfig>>,
    pub omit: Option<bool>,
    pub loose: Option<bool>,
    pub output: Option<String>,
    pub digest: Option<bool>,
    pub classification: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    pub fn as_slice(&self) -> &[T] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

/// One AND-composed match block. Repeated blocks are OR-composed.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ViewMatchConfig {
    pub dialog: Option<crate::dialog::DialogConditions>,
    pub path: Option<PathConditions>,
    pub message: Option<MessageConditions>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PathConditions {
    pub glob: Option<OneOrMany<String>>,
    pub is: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MessageConditions {
    pub is: Option<String>,
    pub starts_with: Option<OneOrMany<String>>,
    pub ends_with: Option<OneOrMany<String>>,
    pub contains: Option<OneOrMany<String>>,
    pub matches: Option<OneOrMany<String>>,
}

impl ViewConfig {
    pub(super) fn validate(&self) -> Result<()> {
        crate::workday::WorkdayConfig::new(&self.workday)
            .context("views: invalid Workday configuration")?;
        for (name, value) in self.layout_templates() {
            let mut env = minijinja::Environment::new();
            env.add_template("layout", value)
                .with_context(|| format!("views.layout.{name}: invalid template"))?;
        }
        for (name, path) in [
            ("output", self.output.as_ref()),
            ("template", self.template.as_ref()),
            ("index-template", self.index_template.as_ref()),
        ]
        .into_iter()
        .chain(
            self.name_tables
                .iter()
                .map(|path| ("name-tables", Some(path))),
        ) {
            if path.is_some_and(|path| path.as_os_str().is_empty()) {
                bail!("views.{name}: path must not be empty");
            }
        }
        let kind_name = Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").expect("static regex");
        for (name, kind) in &self.kind {
            if !kind_name.is_match(name) {
                bail!("views.kind.{name}: Kind name must be kebab-case");
            }
            let Some(matches) = kind.matches.as_ref() else {
                bail!("views.kind.{name}: at least one match block is required");
            };
            if matches.as_slice().is_empty() {
                bail!("views.kind.{name}: at least one match block is required");
            }
            for block in matches.as_slice() {
                validate_match(name, block)?;
            }
            let placements = usize::from(kind.omit.is_some())
                + usize::from(kind.loose.is_some())
                + usize::from(kind.output.is_some());
            if placements > 1 {
                bail!("views.kind.{name}: at most one of omit, loose, or output is allowed");
            }
            if kind.omit == Some(false) || kind.loose == Some(false) {
                bail!("views.kind.{name}: boolean placement Treatments must be true");
            }
            if let Some(output) = &kind.output {
                validate_output(name, output)?;
            }
            if kind.digest == Some(true) {
                bail!("views.kind.{name}: digest Treatment may only be false");
            }
            if kind.omit == Some(true) && (kind.digest.is_some() || kind.classification.is_some()) {
                bail!("views.kind.{name}: omit cannot be combined with digest or classification");
            }
            if let Some(classification) = &kind.classification {
                validate_classification(name, classification)?;
            }
        }
        Ok(())
    }

    fn layout_templates(&self) -> Vec<(&'static str, &str)> {
        let Some(layout) = &self.layout else {
            return Vec::new();
        };
        [
            ("session", layout.session.as_deref()),
            ("loose", layout.loose.as_deref()),
            ("subagent", layout.subagent.as_deref()),
            ("branch", layout.branch.as_deref()),
            ("project-index", layout.project_index.as_deref()),
            ("loose-index", layout.loose_index.as_deref()),
            ("json", layout.json.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect()
    }
}

fn validate_classification(kind: &str, values: &[String]) -> Result<()> {
    if values.is_empty() {
        bail!("views.kind.{kind}: classification must not be empty");
    }
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        if !crate::digest::CLASSIFICATIONS.contains(&value.as_str()) {
            bail!("views.kind.{kind}: unknown classification {value:?}");
        }
        if !seen.insert(value) {
            bail!("views.kind.{kind}: duplicate classification {value:?}");
        }
    }
    Ok(())
}

fn validate_match(kind: &str, block: &ViewMatchConfig) -> Result<()> {
    if let Some(dialog) = &block.dialog {
        dialog
            .validate()
            .with_context(|| format!("views.kind.{kind}"))?;
    }
    let path_has = block
        .path
        .as_ref()
        .is_some_and(|path| path.glob.is_some() || path.is.is_some());
    let message_has = block.message.as_ref().is_some_and(|message| {
        message.is.is_some()
            || message.starts_with.is_some()
            || message.ends_with.is_some()
            || message.contains.is_some()
            || message.matches.is_some()
    });
    if !path_has && !message_has && block.source.is_none() && block.dialog.is_none() {
        bail!("views.kind.{kind}: empty match block");
    }
    if let Some(path) = &block.path
        && let Some(globs) = &path.glob
    {
        require_nonempty(kind, "match.path.glob", globs)?;
        for pattern in globs.as_slice() {
            glob::Pattern::new(pattern).with_context(|| {
                format!("views.kind.{kind}: invalid match.path.glob {pattern:?}")
            })?;
        }
    }
    if let Some(message) = &block.message {
        for (field, values) in [
            ("starts-with", message.starts_with.as_ref()),
            ("ends-with", message.ends_with.as_ref()),
            ("contains", message.contains.as_ref()),
            ("matches", message.matches.as_ref()),
        ] {
            if let Some(values) = values {
                require_nonempty(kind, &format!("match.message.{field}"), values)?;
            }
        }
        if let Some(patterns) = &message.matches {
            for pattern in patterns.as_slice() {
                Regex::new(pattern).with_context(|| {
                    format!("views.kind.{kind}: invalid match.message.matches {pattern:?}")
                })?;
            }
        }
    }
    if let Some(source) = &block.source
        && !matches!(
            source.as_str(),
            "claude" | "codex" | "pi" | "cursor" | "vscode" | "chatgpt"
        )
    {
        bail!("views.kind.{kind}: unknown agent Source {source:?}");
    }
    Ok(())
}

fn require_nonempty<T>(kind: &str, field: &str, values: &OneOrMany<T>) -> Result<()> {
    if values.as_slice().is_empty() {
        bail!("views.kind.{kind}: {field} array must not be empty");
    }
    Ok(())
}

fn validate_output(kind: &str, output: &str) -> Result<()> {
    let relative = output.strip_prefix("./").unwrap_or(output);
    if relative.is_empty()
        || relative.starts_with('/')
        || relative
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        bail!("views.kind.{kind}: malformed output {output:?}");
    }
    Ok(())
}
