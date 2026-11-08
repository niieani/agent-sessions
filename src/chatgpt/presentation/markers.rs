//! Interpret native marker labels, retaining original payloads as inert evidence.
use super::{
    Presentation,
    bindings::humanize,
    citations::{self, Citations},
};
use crate::model::ReferenceKind;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new("([^]*)").unwrap());

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.trim().is_empty())
}
fn named(value: &Value) -> Option<&str> {
    ["title", "name", "label"]
        .iter()
        .find_map(|key| text(&value[key]))
}
fn source(label: &str, artifact: &str, p: &mut Presentation) -> String {
    p.source_link(label, artifact, ReferenceKind::Label)
}
fn unavailable(kind: &str, label: &str, artifact: &str, p: &mut Presentation) -> String {
    p.issues.push(format!("unresolved-native-marker:{kind}"));
    source(label, artifact, p)
}
fn parsed(value: Option<&&str>) -> Option<Value> {
    serde_json::from_str(value?).ok()
}

fn selections(value: &Value, artifact: &str, p: &mut Presentation) -> Option<String> {
    let selections = value["selections"].as_array()?;
    if selections.is_empty() {
        return None;
    }
    Some(
        selections
            .iter()
            .map(|selection| {
                text(&selection[1])
                    .map(|name| source(name, artifact, p))
                    .unwrap_or_else(|| {
                        unavailable("products", "Product selection unavailable", artifact, p)
                    })
            })
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn generated(value: &Value, artifact: &str, p: &mut Presentation) -> Option<String> {
    if let Some(label) = named(value) {
        return Some(source(label, artifact, p));
    }
    let map = value.get("map_widget")?;
    let points = map["points"].as_array()?;
    let names = points
        .iter()
        .filter_map(named)
        .map(|name| source(name, artifact, p))
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| format!("Map: {}", names.join("; ")))
}

fn marker(payload: &str, artifact: &str, citations: &Citations, p: &mut Presentation) -> String {
    let parts = payload.split('').collect::<Vec<_>>();
    let kind = parts[0];
    if kind.is_empty()
        || kind.len() > 64
        || !kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return unavailable("unknown", "Native widget unavailable", artifact, p);
    }
    let data = parsed(parts.get(1));
    match kind {
        "entity" | "product_entity" | "entity_metadata" => {
            let index = if kind == "entity_metadata" { 2 } else { 1 };
            let name = data.as_ref().and_then(|v| text(&v[index]));
            if let Some(name) = name {
                return source(name, artifact, p);
            }
            unavailable(kind, "Entity unavailable", artifact, p)
        }
        "product" => {
            if let Some(name) = data
                .as_ref()
                .and_then(|v| text(&v["product_name"]).or_else(|| text(&v[1])))
            {
                return source(name, artifact, p);
            }
            unavailable(kind, "Product unavailable", artifact, p)
        }
        "products" => data
            .as_ref()
            .and_then(|v| selections(v, artifact, p))
            .unwrap_or_else(|| unavailable(kind, "Product selection unavailable", artifact, p)),
        "url" => {
            let label = parts
                .get(1)
                .copied()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("URL unavailable");
            if let Some(url) = parts.get(2).and_then(|url| citations::url(url)) {
                p.source_link(label, &url, ReferenceKind::Label)
            } else {
                unavailable(kind, label, artifact, p)
            }
        }
        "navlist" | "video" | "summary" => {
            if let Some(label) = parts.get(1).filter(|s| !s.trim().is_empty()) {
                return source(label, artifact, p);
            }
            unavailable(
                kind,
                &format!("{} unavailable", humanize(kind)),
                artifact,
                p,
            )
        }
        "businesses_map" => {
            let names = parts[1..]
                .iter()
                .filter_map(|part| serde_json::from_str::<Value>(part).ok())
                .filter_map(|value| named(&value).map(str::to_owned))
                .map(|name| source(&name, artifact, p))
                .collect::<Vec<_>>();
            if names.is_empty() {
                unavailable(kind, "Business map", artifact, p)
            } else {
                format!("Map: {}", names.join("; "))
            }
        }
        "genui" => data
            .as_ref()
            .and_then(|value| generated(value, artifact, p))
            .unwrap_or_else(|| {
                let label = data
                    .as_ref()
                    .and_then(Value::as_object)
                    .and_then(|o| (o.len() == 1).then(|| humanize(o.keys().next().unwrap())))
                    .unwrap_or_else(|| "Generated interface".into());
                unavailable(kind, &label, artifact, p)
            }),
        "filecite" => parts
            .get(1)
            .and_then(|id| citations.resolve(&[(*id).into()], p))
            .unwrap_or_else(|| unavailable(kind, "File citation", artifact, p)),
        _ => {
            let label = match kind {
                "memcite" => "Memory citation".into(),
                "image_group" => "Image gallery".into(),
                "i" => "Image references".into(),
                "finance" => "Financial chart".into(),
                "forecast" => "Weather forecast".into(),
                "map" => "Map".into(),
                _ => humanize(kind),
            };
            unavailable(kind, &label, artifact, p)
        }
    }
}

pub(super) fn convert(
    text: &str,
    artifact: &str,
    citations: &Citations,
    p: &mut Presentation,
) -> String {
    // Older responses surround prose spans with highlighting/segment controls.
    // They carry no reference identity; cite markers are interpreted separately.
    // This function receives only prose, so literal code remains unchanged.
    let text = text.replace(['', '', ''], "");
    MARKER
        .replace_all(&text, |capture: &regex::Captures<'_>| {
            marker(&capture[1], artifact, citations, p)
        })
        .into_owned()
}
