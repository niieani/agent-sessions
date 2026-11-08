use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::fleet::DigestConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    /// The first Digest of a Session: establishes its Topic and Intent.
    FirstSessionDay,
    /// A later Session Day with a known Intent; `intent: null` keeps it.
    ContinuationSessionDay,
    /// A later Session Day whose prior Digests carry no Intent: establishes it.
    ContinuationSessionDayWithoutIntent,
    SessionTitle,
}

impl CompletionKind {
    pub(crate) fn sidecar_kind(self) -> &'static str {
        match self {
            Self::FirstSessionDay
            | Self::ContinuationSessionDay
            | Self::ContinuationSessionDayWithoutIntent => "day",
            Self::SessionTitle => "session-title",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCorrection {
    pub raw: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    pub kind: CompletionKind,
    pub system: String,
    pub user: String,
    pub corrections: Vec<ModelCorrection>,
}

impl ModelRequest {
    pub(crate) fn messages(&self) -> Value {
        let mut messages = vec![
            json!({"role": "system", "content": self.system}),
            json!({"role": "user", "content": self.user}),
        ];
        for correction in &self.corrections {
            messages.push(json!({"role": "assistant", "content": correction.raw}));
            messages.push(json!({
                "role": "user",
                "content": format!(
                    "Your previous response was rejected: {}. Return corrected schema-valid JSON only.",
                    correction.error
                )
            }));
        }
        Value::Array(messages)
    }
}

pub trait ModelClient: Sync {
    fn complete(&self, request: &ModelRequest) -> Result<String>;
}

/// A successful HTTP response that contains no usable model output. Unlike a
/// transport/configuration error, this spends the model-output retry budget.
#[derive(Debug)]
pub(super) struct InvalidCompletion {
    pub raw: String,
}

impl std::fmt::Display for InvalidCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Digest model response has no choices[0].message.content")
    }
}
impl std::error::Error for InvalidCompletion {}

pub struct HttpModelClient {
    agent: ureq::Agent,
    endpoint: String,
    api_key: String,
    model: String,
    extra_body: Map<String, Value>,
}

impl HttpModelClient {
    pub fn new(config: &DigestConfig, api_key: String) -> Result<Self> {
        let timeout: Duration = config.timeout()?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .build()
            .into();
        let extra_body = serde_json::to_value(&config.extra_body)?
            .as_object()
            .cloned()
            .context("digest.extra-body must be an object")?;
        Ok(Self {
            agent,
            endpoint: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
            api_key,
            model: config.model.clone(),
            extra_body,
        })
    }
}

impl ModelClient for HttpModelClient {
    fn complete(&self, request: &ModelRequest) -> Result<String> {
        let mut body = Map::new();
        body.insert("model".into(), Value::String(self.model.clone()));
        body.insert("messages".into(), request.messages());
        body.insert("response_format".into(), response_format(request.kind));
        body.extend(self.extra_body.clone());
        let mut response = self
            .agent
            .post(&self.endpoint)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(Value::Object(body))
            .context("Digest model request failed")?;
        let value: Value = response
            .body_mut()
            .read_json()
            .context("parsing Digest model response")?;
        match value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
        {
            Some(content) => Ok(content.to_owned()),
            None => Err(InvalidCompletion {
                raw: serde_json::to_string(&value)?,
            }
            .into()),
        }
    }
}

pub(super) fn response_format(kind: CompletionKind) -> Value {
    let (name, schema) = match kind {
        CompletionKind::FirstSessionDay => (
            "first_session_day_digest",
            day_schema(true, json!({"type": "string", "minLength": 1})),
        ),
        CompletionKind::ContinuationSessionDay => (
            "continuation_session_day_digest",
            day_schema(false, json!({"type": ["string", "null"]})),
        ),
        CompletionKind::ContinuationSessionDayWithoutIntent => (
            "intent_establishing_session_day_digest",
            day_schema(false, json!({"type": "string", "minLength": 1})),
        ),
        CompletionKind::SessionTitle => (
            "session_title",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {"title": {"type": "string"}},
                "required": ["title"]
            }),
        ),
    };
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": name,
            "strict": true,
            "schema": schema
        }
    })
}

/// Session Day output schema; only the first Digest carries a Topic.
fn day_schema(with_topic: bool, intent: Value) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    if with_topic {
        properties.insert("topic".into(), json!({"type": "string", "minLength": 1}));
        required.push("topic");
    }
    properties.insert("title".into(), json!({"type": "string"}));
    properties.insert("intent".into(), intent);
    properties.insert("tldr".into(), json!({"type": ["string", "null"]}));
    properties.insert("summary".into(), json!({"type": "string"}));
    properties.insert(
        "classification".into(),
        json!({
            "type": "array",
            "minItems": 1,
            "items": {"type": "string", "enum": super::CLASSIFICATIONS}
        }),
    );
    required.extend(["title", "intent", "tldr", "summary", "classification"]);
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_and_continuation_days_use_distinct_strict_schemas() {
        let first = response_format(CompletionKind::FirstSessionDay);
        let continuation = response_format(CompletionKind::ContinuationSessionDay);
        let first_schema = &first["json_schema"]["schema"];
        let continuation_schema = &continuation["json_schema"]["schema"];

        assert_eq!(first_schema["properties"]["topic"]["type"], "string");
        assert_eq!(first_schema["properties"]["topic"]["minLength"], 1);
        assert!(
            first_schema["required"]
                .as_array()
                .unwrap()
                .contains(&Value::String("topic".into()))
        );
        assert!(continuation_schema["properties"].get("topic").is_none());
        assert_eq!(continuation_schema["additionalProperties"], false);
    }

    #[test]
    fn only_a_continuation_with_a_known_intent_may_leave_intent_null() {
        let schema = |kind| response_format(kind)["json_schema"]["schema"].clone();
        for kind in [
            CompletionKind::FirstSessionDay,
            CompletionKind::ContinuationSessionDayWithoutIntent,
        ] {
            let schema = schema(kind);
            assert_eq!(schema["properties"]["intent"]["type"], "string");
            assert_eq!(schema["properties"]["intent"]["minLength"], 1);
            assert!(
                schema["properties"].get("topic").is_some()
                    == (kind == CompletionKind::FirstSessionDay)
            );
        }
        let continuation = schema(CompletionKind::ContinuationSessionDay);
        assert_eq!(
            continuation["properties"]["intent"]["type"],
            json!(["string", "null"])
        );
        for kind in [
            CompletionKind::FirstSessionDay,
            CompletionKind::ContinuationSessionDay,
            CompletionKind::ContinuationSessionDayWithoutIntent,
        ] {
            assert!(
                schema(kind)["required"]
                    .as_array()
                    .unwrap()
                    .contains(&Value::String("intent".into()))
            );
        }
    }
}
