//! Read explicit native reference associations; never infer identity from prose.
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Bindings(BTreeMap<String, Vec<(u8, Value)>>);

enum Lookup {
    Missing,
    Value(String),
    Conflicting,
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn member_path(expression: &str) -> Option<Vec<String>> {
    let path = expression.replace("?.", ".");
    let parts: Vec<_> = path.split('.').map(str::to_owned).collect();
    parts
        .iter()
        .all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
        })
        .then_some(parts)
}

fn reference(value: &Value) -> Option<String> {
    if let Some(id) = value.as_str().or_else(|| value["ref"].as_str()) {
        return Some(id.to_owned());
    }
    Some(format!(
        "turn{}{}{}",
        value["turn_index"].as_u64()?,
        value["ref_type"].as_str()?,
        value["ref_index"].as_u64()?
    ))
}

impl Bindings {
    pub fn new(native: &Value) -> Self {
        let mut out = Self::default();
        let genui = &native["metadata"]["model_dil_v2"]["appData"]["opGenui"];
        if let Some(bindings) = genui["modelDataBindings"].as_object() {
            for (id, value) in bindings {
                out.add(id, 4, value);
            }
        }
        if let Some(results) = genui["componentResults"].as_object() {
            for result in results.values() {
                if result["status"].as_str().is_none_or(|s| s == "resolved") {
                    out.records(&result["state"]);
                }
            }
            if let Some(components) = native["metadata"]["genui_components"].as_array() {
                for component in components {
                    let Some(id) = component["component_resolution_id"].as_str() else {
                        continue;
                    };
                    let Some(result) = results.get(id) else {
                        continue;
                    };
                    if result["status"].as_str().is_some_and(|s| s != "resolved") {
                        continue;
                    }
                    // A scalar resolved Entity value has exactly one owner.
                    if let Some(entities) = component["entities"].as_array()
                        && entities.len() == 1
                        && let Some(id) = reference(&entities[0])
                        && scalar(&result["state"]["value"]).is_some()
                    {
                        out.add(&id, 2, &result["state"]);
                    }
                }
            }
        }
        out
    }

    fn add(&mut self, id: &str, priority: u8, value: &Value) {
        self.0
            .entry(id.to_owned())
            .or_default()
            .push((priority, value.clone()));
    }

    fn records(&mut self, value: &Value) {
        match value {
            Value::Object(object) => {
                if let Some(refs) = value["refs"].as_array() {
                    for id in refs.iter().filter_map(reference) {
                        self.add(&id, 1, value);
                        for action in ["default_on_click_action", "on_click_action", "click_action"]
                        {
                            if let Some(params) =
                                value[action]["payload"]["extraParams"].as_object()
                            {
                                for entity in params.values().filter(|v| v.is_object()) {
                                    self.add(&id, 2, entity);
                                    // Native shopping records expose these view bindings
                                    // through product fields with fixed schema names.
                                    if params.get("product") == Some(entity) {
                                        let mut product = entity.clone();
                                        if let Some(fields) = product.as_object_mut() {
                                            for (display, native) in [
                                                ("displayed_price", "price"),
                                                ("merchant", "merchants"),
                                            ] {
                                                if !fields.contains_key(display)
                                                    && let Some(value) = entity.get(native)
                                                {
                                                    fields.insert(display.into(), value.clone());
                                                }
                                            }
                                            if !fields.contains_key("product_page")
                                                && let Some(url) = entity.get("url")
                                            {
                                                fields.insert(
                                                    "product_page".into(),
                                                    serde_json::json!({"uri":url}),
                                                );
                                            }
                                        }
                                        self.add(&id, 2, &product);
                                    }
                                    if let Some(fields) = entity
                                        .pointer("/__internal/model_data_binding_fields_v1")
                                        .filter(|v| v.is_object())
                                    {
                                        self.add(&id, 3, fields);
                                    }
                                }
                            }
                        }
                    }
                }
                for child in object.values() {
                    self.records(child);
                }
            }
            Value::Array(values) => {
                for child in values {
                    self.records(child);
                }
            }
            _ => {}
        }
    }

    fn lookup(&self, path: &[String]) -> Lookup {
        let Some(candidates) = self.0.get(&path[0]) else {
            return Lookup::Missing;
        };
        let mut best: Option<(u8, String)> = None;
        let mut conflict = false;
        for (priority, candidate) in candidates {
            let mut value = candidate;
            let mut present = true;
            for key in &path[1..] {
                if let Some(child) = value.get(key) {
                    value = child;
                } else {
                    present = false;
                    break;
                }
            }
            let Some(text) = present.then(|| scalar(value)).flatten() else {
                continue;
            };
            match &best {
                Some((rank, previous)) if rank == priority => conflict |= previous != &text,
                Some((rank, _)) if rank > priority => {}
                _ => {
                    best = Some((*priority, text));
                    conflict = false;
                }
            }
        }
        if conflict {
            Lookup::Conflicting
        } else if let Some((_, value)) = best {
            Lookup::Value(value)
        } else {
            Lookup::Missing
        }
    }

    pub fn expression(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if let Ok(value) = serde_json::from_str::<Value>(expression) {
            return scalar(&value);
        }
        match self.lookup(&member_path(expression)?) {
            Lookup::Value(value) => Some(value),
            _ => None,
        }
    }

    pub fn name(&self, id: &str) -> Option<String> {
        for field in ["title", "name", "label", "value"] {
            match self.lookup(&[id.into(), field.into()]) {
                Lookup::Value(value) => return Some(value),
                Lookup::Conflicting => return None,
                Lookup::Missing => {}
            }
        }
        None
    }

    pub fn entity(&self, expression: Option<&str>, id: Option<&str>) -> Option<String> {
        if let Some(expression) = expression {
            if let Ok(value) = serde_json::from_str::<Value>(expression.trim()) {
                return scalar(&value);
            }
            let path = member_path(expression.trim())?;
            match self.lookup(&path) {
                Lookup::Value(value) => return Some(value),
                Lookup::Conflicting => return None,
                Lookup::Missing => {}
            }
            if path.len() != 2 || !matches!(path[1].as_str(), "title" | "name" | "label" | "value")
            {
                return None;
            }
        }
        self.name(id?)
    }
}

pub(super) fn field_label(expression: &str) -> String {
    if member_path(expression.trim())
        .and_then(|p| p.last().cloned())
        .as_deref()
        == Some("displayed_price")
    {
        return "Price not captured".into();
    }
    member_path(expression.trim())
        .and_then(|parts| (parts.len() > 1).then(|| humanize(parts.last().unwrap())))
        .filter(|s| !s.is_empty())
        .map(|label| format!("{label} unavailable"))
        .unwrap_or_else(|| "Expression unavailable".into())
}

pub(super) fn humanize(text: &str) -> String {
    let mut out = String::new();
    let mut previous = None;
    for ch in text.chars() {
        if matches!(ch, '_' | '-') {
            out.push(' ');
        } else {
            if ch.is_uppercase() && previous.is_some_and(char::is_lowercase) {
                out.push(' ');
            }
            if out.is_empty() {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
        }
        previous = Some(ch);
    }
    out
}
