//! Local model tokenization, independent of model transport and archive selection.
use super::ModelRequest;
use crate::fleet::DigestConfig;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

pub trait TokenCounter: Sync {
    fn count_text(&self, text: &str) -> Result<usize>;
    fn count(&self, request: &ModelRequest) -> Result<usize>;
}

pub struct LocalTokenizer {
    tokenizer: tokenizers::Tokenizer,
    environment: minijinja::Environment<'static>,
    variables: serde_json::Map<String, Value>,
    prompt_format: crate::fleet::TokenizerPromptFormat,
}

impl LocalTokenizer {
    pub fn load(config: &DigestConfig) -> Result<Self> {
        let dir = config.tokenizer.as_ref().context("Digest requires digest.tokenizer: a local HF model directory containing tokenizer.json and tokenizer_config.json")?;
        let mut tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow!("loading Digest tokenizer {}: {e}", dir.display()))?;
        tokenizer
            .with_truncation(None)
            .map_err(|e| anyhow!("disabling tokenizer truncation: {e}"))?;
        tokenizer.with_padding(None);
        let metadata: Value = serde_json::from_reader(
            std::fs::File::open(dir.join("tokenizer_config.json"))
                .context("opening Digest tokenizer_config.json")?,
        )?;
        let template = metadata
            .get("chat_template")
            .and_then(Value::as_str)
            .context("Digest tokenizer_config.json requires a string chat_template")?;
        let mut environment = minijinja::Environment::new();
        // Match Transformers' Jinja environment for HF chat templates.
        environment.set_trim_blocks(true);
        environment.set_lstrip_blocks(true);
        environment.set_unknown_method_callback(|_state, value, method, args| {
            if let (Some(text), [argument]) = (value.as_str(), args)
                && let Some(needle) = argument.as_str()
            {
                match method {
                    "startswith" => return Ok(minijinja::Value::from(text.starts_with(needle))),
                    "endswith" => return Ok(minijinja::Value::from(text.ends_with(needle))),
                    _ => {}
                }
            }
            Err(minijinja::Error::new(
                minijinja::ErrorKind::UnknownMethod,
                format!("unsupported chat-template method {method}"),
            ))
        });
        environment.add_function(
            "raise_exception",
            |message: String| -> std::result::Result<String, minijinja::Error> {
                Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    message,
                ))
            },
        );
        environment.add_template_owned("chat", template.to_owned())?;
        let mut variables = metadata
            .as_object()
            .cloned()
            .context("tokenizer config must be an object")?;
        if let Some(kwargs) = config.extra_body.get("chat_template_kwargs") {
            let kwargs = serde_json::to_value(kwargs)?;
            variables.extend(
                kwargs
                    .as_object()
                    .context("chat_template_kwargs must be an object")?
                    .clone(),
            );
        }
        variables.insert("add_generation_prompt".into(), json!(true));
        let counter = Self {
            tokenizer,
            environment,
            variables,
            prompt_format: config.tokenizer_prompt_format,
        };
        counter
            .count(&ModelRequest {
                kind: super::CompletionKind::SessionTitle,
                system: "test".into(),
                user: "test".into(),
                corrections: vec![],
            })
            .context("validating Digest chat template")?;
        Ok(counter)
    }
}
impl TokenCounter for LocalTokenizer {
    fn count_text(&self, text: &str) -> Result<usize> {
        self.tokenizer
            .encode(text, false)
            .map(|e| e.len())
            .map_err(|e| anyhow!("counting Digest text: {e}"))
    }
    fn count(&self, request: &ModelRequest) -> Result<usize> {
        let mut variables = self.variables.clone();
        let mut messages = request.messages();
        if self.prompt_format == crate::fleet::TokenizerPromptFormat::Omlx {
            let format = super::client::response_format(request.kind);
            let name = format["json_schema"]["name"]
                .as_str()
                .context("missing schema name")?;
            let schema = serde_json::to_string_pretty(&format["json_schema"]["schema"])?;
            // oMLX 0.7 api/tool_calling.py::build_json_system_prompt and
            // server.py::_inject_json_instruction; count only, never send twice.
            messages[0]["content"] = json!(format!(
                "{}\n\nYou must respond with valid JSON matching the '{name}' schema.\n\nJSON Schema:\n```json\n{schema}\n```\n\nRespond with only the JSON object, no additional text or explanation.",
                request.system
            ));
        }
        variables.insert("messages".into(), messages);
        let rendered = self
            .environment
            .get_template("chat")?
            .render(minijinja::value::Serde(&variables))
            .context("rendering Digest chat template")?;
        self.tokenizer
            .encode(rendered, false)
            .map(|e| e.len())
            .map_err(|e| anyhow!("counting Digest input tokens: {e}"))
    }
}

/// Middle-cut the largest remaining Message until the complete prompt fits.
/// Every candidate is re-encoded; token boundaries may merge after a cut.
pub(super) fn fit_messages(
    counter: &dyn TokenCounter,
    messages: &[crate::model::Message],
    limit: usize,
    build: impl Fn(&[crate::model::Message]) -> ModelRequest,
) -> Result<(ModelRequest, bool, usize)> {
    let mut messages = messages.to_vec();
    let originals: Vec<_> = messages.iter().map(|m| m.text.clone()).collect();
    let mut lengths: Vec<_> = originals.iter().map(|s| s.chars().count()).collect();
    let mut sizes = originals
        .iter()
        .map(|s| counter.count_text(s))
        .collect::<Result<Vec<_>>>()?;
    let mut truncated = false;
    loop {
        let request = build(&messages);
        let tokens = counter.count(&request)?;
        if tokens <= limit {
            return Ok((request, truncated, tokens));
        }
        let index = sizes
            .iter()
            .enumerate()
            .filter(|(i, _)| lengths[*i] > 0)
            .max_by_key(|(_, size)| *size)
            .map(|(i, _)| i)
            .context("Digest fixed Context and message markers exceed max-input-tokens")?;
        lengths[index] = lengths[index] * 3 / 4;
        messages[index].text = middle(&originals[index], lengths[index]);
        sizes[index] = counter.count_text(&messages[index].text)?;
        truncated = true;
    }
}

fn middle(text: &str, keep: usize) -> String {
    let start = text
        .char_indices()
        .nth(keep.div_ceil(2))
        .map_or(text.len(), |(i, _)| i);
    let end = text
        .char_indices()
        .rev()
        .nth(keep / 2)
        .map_or(0, |(i, c)| i + c.len_utf8());
    format!(
        "{}\n[message middle truncated]\n{}",
        &text[..start],
        &text[end..]
    )
}
