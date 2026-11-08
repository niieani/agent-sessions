use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use minijinja::{AutoEscape, Environment, UndefinedBehavior, Value as MiniValue};
use serde_json::{Value, json};

use super::Unit;
use crate::fleet::{ViewConfig, ViewLayoutConfig};
use crate::timefmt;

const DEFAULT_SESSION_LAYOUT: &str = "projects/{{ project_dir }}/{{ default_stem }}.md";
const DEFAULT_LOOSE_LAYOUT: &str = "loose/{{ month }}/{{ default_stem }}.md";
const DEFAULT_SUBAGENT_LAYOUT: &str = "{{ parent_dir }}/subagents/{{ default_stem }}.md";
const DEFAULT_PROJECT_INDEX_LAYOUT: &str = "projects/{{ project_dir }}/INDEX.md";
const DEFAULT_LOOSE_INDEX_LAYOUT: &str = "loose/{{ month }}/INDEX.md";
const BUILTIN_TRANSCRIPT: &str = include_str!("../templates/transcript.md.j2");
const BUILTIN_INDEX: &str = include_str!("../templates/index.md.j2");

pub(super) struct LayoutTemplate {
    env: Environment<'static>,
    pub(super) requires_title: bool,
}

impl LayoutTemplate {
    fn new(name: &str, source: &str) -> Result<Self> {
        let mut env = strict_environment();
        env.add_template_owned("layout".to_string(), source.to_string())
            .with_context(|| format!("views.layout.{name}: invalid template"))?;
        env.get_template("layout")?
            .render(to_mini(&sample_layout_context(true)))
            .with_context(|| format!("views.layout.{name}: unknown layout variable"))?;
        let requires_title = env
            .get_template("layout")?
            .render(to_mini(&sample_layout_context(false)))
            .is_err();
        Ok(Self {
            env,
            requires_title,
        })
    }

    pub(super) fn render(&self, context: &Value) -> Result<PathBuf> {
        let rendered = self.env.get_template("layout")?.render(to_mini(context))?;
        crate::render::filename::path(&validate_relative_path(&crate::path_key::nfc(&rendered))?)
    }
}

pub(super) struct Layouts {
    session: LayoutTemplate,
    loose: LayoutTemplate,
    subagent: LayoutTemplate,
    branch: LayoutTemplate,
    project_index: Option<LayoutTemplate>,
    loose_index: Option<LayoutTemplate>,
    json: Option<LayoutTemplate>,
    pub(super) defaults: bool,
}

impl Layouts {
    pub(super) fn load(config: &ViewConfig) -> Result<Self> {
        let defaults = config.layout.is_none();
        let layout = config
            .layout
            .as_ref()
            .cloned()
            .unwrap_or_else(|| ViewLayoutConfig {
                session: Some(DEFAULT_SESSION_LAYOUT.into()),
                loose: Some(DEFAULT_LOOSE_LAYOUT.into()),
                subagent: Some(DEFAULT_SUBAGENT_LAYOUT.into()),
                branch: None,
                project_index: Some(DEFAULT_PROJECT_INDEX_LAYOUT.into()),
                loose_index: Some(DEFAULT_LOOSE_INDEX_LAYOUT.into()),
                json: None,
            });
        let required = |name: &str, value: Option<String>, default: &str| {
            LayoutTemplate::new(name, value.as_deref().unwrap_or(default))
        };
        Ok(Self {
            session: required("session", layout.session, DEFAULT_SESSION_LAYOUT)?,
            loose: required("loose", layout.loose, DEFAULT_LOOSE_LAYOUT)?,
            subagent: required("subagent", layout.subagent, DEFAULT_SUBAGENT_LAYOUT)?,
            branch: required(
                "branch",
                layout.branch,
                "{% if base_dir %}{{ base_dir }}/{% endif %}{{ base_stem }}{{ branch_suffix }}.md",
            )?,
            project_index: if defaults || layout.project_index.is_some() {
                Some(LayoutTemplate::new(
                    "project-index",
                    layout
                        .project_index
                        .as_deref()
                        .unwrap_or(DEFAULT_PROJECT_INDEX_LAYOUT),
                )?)
            } else {
                None
            },
            loose_index: if defaults || layout.loose_index.is_some() {
                Some(LayoutTemplate::new(
                    "loose-index",
                    layout
                        .loose_index
                        .as_deref()
                        .unwrap_or(DEFAULT_LOOSE_INDEX_LAYOUT),
                )?)
            } else {
                None
            },
            json: layout
                .json
                .as_deref()
                .map(|value| LayoutTemplate::new("json", value))
                .transpose()?,
            defaults,
        })
    }

    pub(super) fn branch_path(
        &self,
        unit: &Unit,
        candidate: &Path,
        mut context: Value,
    ) -> Result<PathBuf> {
        if unit.branch.is_none() {
            return Ok(candidate.to_path_buf());
        }
        context["base_dir"] = json!(
            candidate
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_string_lossy()
        );
        context["base_stem"] = json!(
            candidate
                .file_stem()
                .context("branch filename missing")?
                .to_string_lossy()
        );
        context["branch_suffix"] = json!(super::branches::suffix(unit));
        self.branch.render(&context)
    }
    pub(super) fn transcript(&self, unit: &Unit) -> &LayoutTemplate {
        if unit.child {
            &self.subagent
        } else if unit.group.loose {
            &self.loose
        } else {
            &self.session
        }
    }

    pub(super) fn index(&self, unit: &Unit) -> Option<&LayoutTemplate> {
        if unit.group.loose {
            self.loose_index.as_ref()
        } else {
            self.project_index.as_ref()
        }
    }

    pub(super) fn json(&self) -> Option<&LayoutTemplate> {
        self.json.as_ref()
    }

    pub(super) fn unit_requires_title(&self, unit: &Unit) -> bool {
        (unit.branch.is_some() && self.branch.requires_title)
            || self.transcript(unit).requires_title
            || (!unit.child && self.index(unit).is_some_and(|layout| layout.requires_title))
            || self.json().is_some_and(|layout| layout.requires_title)
    }
}

pub(super) struct Templates {
    env: Environment<'static>,
}

impl Templates {
    pub(super) fn load(config: &ViewConfig) -> Result<Self> {
        let transcript = read_template(config.template.as_deref(), BUILTIN_TRANSCRIPT, "template")?;
        let index = read_template(
            config.index_template.as_deref(),
            BUILTIN_INDEX,
            "index-template",
        )?;
        let mut env = strict_environment();
        env.add_template_owned("transcript".to_string(), transcript)
            .context("views.template: invalid template")?;
        env.add_template_owned("index".to_string(), index)
            .context("views.index-template: invalid template")?;
        Ok(Self { env })
    }

    pub(super) fn transcript(&self, context: &Value) -> Result<String> {
        self.env
            .get_template("transcript")?
            .render(to_mini(context))
            .context("rendering Transcript template")
    }

    pub(super) fn index(&self, context: &Value) -> Result<String> {
        self.env
            .get_template("index")?
            .render(to_mini(context))
            .context("rendering index template")
    }
}

fn strict_environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_keep_trailing_newline(true);
    env.add_filter("yaml", |value: MiniValue| {
        if value.kind() == minijinja::value::ValueKind::Bool {
            value.is_true().to_string()
        } else {
            crate::render::yaml_str(&value.to_string())
        }
    });
    env.add_filter("yaml_native", |value: MiniValue| {
        if value.kind() == minijinja::value::ValueKind::Bool {
            value.is_true().to_string()
        } else {
            crate::render::yaml_native_str(&value.to_string())
        }
    });
    env.add_filter("frontmatter_time", timefmt::frontmatter);
    env.add_filter("table_cell", |value: String| value.replace('|', "\\|"));
    env
}

fn read_template(path: Option<&Path>, builtin: &str, name: &str) -> Result<String> {
    match path {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading views.{name} {}", path.display())),
        None => Ok(builtin.into()),
    }
}

fn validate_relative_path(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    anyhow::ensure!(!value.is_empty(), "View layout rendered an empty path");
    anyhow::ensure!(!path.is_absolute(), "View layout rendered an absolute path");
    anyhow::ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "View layout rendered an unsafe path {value:?}"
    );
    Ok(path)
}

fn sample_layout_context(include_title: bool) -> Value {
    let mut value = json!({
        "project": "project",
        "project_dir": "project",
        "workday": "2026-01-01",
        "month": "2026-01",
        "default_stem": "stem",
        "short_id": "12345678",
        "source": "codex",
        "native_title": "Native name",
        "parent_dir": "projects/project",
        "parent_stem": "parent",
        "base_dir":"projects/project",
        "base_stem":"stem",
        "branch_suffix":" (branch 2)",
    });
    if include_title {
        value["title"] = Value::String("title".into());
    }
    value
}

fn to_mini(value: &Value) -> MiniValue {
    match value {
        Value::Null => MiniValue::from(()),
        Value::Bool(value) => MiniValue::from(*value),
        Value::Number(value) => value
            .as_i64()
            .map(MiniValue::from)
            .or_else(|| value.as_u64().map(MiniValue::from))
            .unwrap_or_else(|| MiniValue::from(value.as_f64().unwrap_or_default())),
        Value::String(value) => MiniValue::from(value.clone()),
        Value::Array(values) => {
            MiniValue::from(values.iter().map(to_mini).collect::<Vec<MiniValue>>())
        }
        Value::Object(values) => MiniValue::from_pairs(
            values
                .iter()
                .map(|(key, value)| (key.clone(), to_mini(value))),
        ),
    }
}

#[cfg(test)]
mod whitespace_tests {
    use super::*;

    #[test]
    fn yaml_filters_render_boolean_values_in_lowercase() {
        let mut env = strict_environment();
        env.add_template(
            "bools",
            "shared-segment: {{ yes | yaml }}\nother: {{ no | yaml_native }}\n",
        )
        .unwrap();
        let rendered = env
            .get_template("bools")
            .unwrap()
            .render(minijinja::context!(yes => true, no => false))
            .unwrap();
        assert_eq!(rendered, "shared-segment: true\nother: false\n");
    }
    #[test]
    fn strict_environment_trims_control_line_whitespace_and_keeps_final_newline() {
        let mut env = strict_environment();
        env.add_template(
            "fixture",
            "---\n{% if show %}\nvalue: {{ value | yaml }}\n{% endif %}\n---\n",
        )
        .unwrap();
        let rendered = env
            .get_template("fixture")
            .unwrap()
            .render(minijinja::context!(show => true, value => "plain"))
            .unwrap();
        assert_eq!(rendered, "---\nvalue: plain\n---\n");
    }
}
