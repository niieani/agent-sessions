//! Decode known Feed fields with cursor/field diagnostics; ignore future response fields.
use super::{LedBy, Origin, SessionLead};
use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

impl SessionLead {
    pub(super) fn from_wire(value: Value) -> Result<Self> {
        let cursor = value
            .get("cursor")
            .and_then(Value::as_str)
            .unwrap_or("<missing or invalid>");
        decode(&value).with_context(|| format!("Session Lead cursor {cursor:?}"))
    }
}

fn field<T: DeserializeOwned>(value: &Value, key: &str, path: &str) -> Result<T> {
    let raw = value
        .get(key)
        .with_context(|| format!("missing field {path}"))?;
    serde_json::from_value(raw.clone()).with_context(|| format!("invalid field {path}"))
}
fn optional<T: DeserializeOwned>(value: &Value, key: &str, path: &str) -> Result<Option<T>> {
    value.get(key).map(|_| field(value, key, path)).transpose()
}
fn decode(value: &Value) -> Result<SessionLead> {
    anyhow::ensure!(value.is_object(), "lead must be an object");
    let by = value.get("ledBy").context("missing field ledBy")?;
    anyhow::ensure!(by.is_object(), "invalid field ledBy: expected object");
    let origin = value
        .get("origin")
        .map(|raw| -> Result<Origin> {
            let fields = raw
                .as_object()
                .context("invalid field origin: expected object")?;
            let kind = field(raw, "kind", "origin.kind")?;
            let fields = fields
                .keys()
                .filter(|key| key.as_str() != "kind")
                .map(|key| Ok((key.clone(), field(raw, key, &format!("origin.{key}"))?)))
                .collect::<Result<_>>()?;
            Ok(Origin { kind, fields })
        })
        .transpose()?;
    let lead = SessionLead {
        cursor: field(value, "cursor", "cursor")?,
        source: field(value, "source", "source")?,
        session_id: field(value, "sessionId", "sessionId")?,
        led_by: LedBy {
            kind: field(by, "kind", "ledBy.kind")?,
            name: optional(by, "name", "ledBy.name")?,
        },
        host: optional(value, "host", "host")?,
        origin,
        recorded_at: field(value, "recordedAt", "recordedAt")?,
    };
    lead.validate_fields()?;
    Ok(lead)
}
