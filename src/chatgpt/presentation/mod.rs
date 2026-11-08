//! Source interpretation and inert artifacts, consumed by the shared renderer.
//! No native JavaScript is executed and no website data is fetched here.
mod bindings;
mod citations;
mod components;
mod illustrations;
mod markers;
mod svg;

use super::Graph;
use crate::model::{
    Message, MessageKind, ReferenceKind, ReferenceTarget, RenderAsset, RenderMaterial,
    RenderReference, Role,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Presentation {
    pub text: String,
    pub artifacts: Vec<RenderAsset>,
    pub issues: Vec<String>,
    pub references: Vec<RenderReference>,
}
impl Presentation {
    pub(super) fn source_link(&mut self, label: &str, target: &str, kind: ReferenceKind) -> String {
        let (target, definition) =
            if target.starts_with("https://") || target.starts_with("http://") {
                (ReferenceTarget::Url(target.into()), label.to_owned())
            } else {
                (
                    ReferenceTarget::Url(format!(
                        "asessions-capture:{}",
                        target
                            .trim_start_matches("assets/")
                            .trim_end_matches(".json")
                    )),
                    "Captured response data".into(),
                )
            };
        let reference = RenderReference::new(target, definition, kind);
        let shown = reference.markdown(if kind == ReferenceKind::Citation {
            ""
        } else {
            label
        });
        self.references.push(reference);
        shown
    }
}
fn component(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        "unsafe native artifact identity"
    );
    Ok(value)
}
pub(super) fn native_text(native: &Value) -> Result<String> {
    let content = native
        .get("content")
        .context("Dialog message lacks content")?;
    let parts = content.get("parts").and_then(Value::as_array);
    if let Some(parts) = parts {
        parts
            .iter()
            .map(|part| {
                if matches!(
                    part["content_type"].as_str(),
                    Some("audio_asset_pointer" | "audio")
                ) {
                    return Ok(String::new());
                }
                if let Some(text) = part
                    .as_str()
                    .or_else(|| part.get("text").and_then(Value::as_str))
                {
                    Ok(text.into())
                } else if let Some(pointer) = part.get("asset_pointer").and_then(Value::as_str) {
                    Ok(format!("[Missing image: {pointer} (not captured)]"))
                } else {
                    Ok(format!(
                        "[Unsupported native content: {}]",
                        part["content_type"].as_str().unwrap_or("unknown")
                    ))
                }
            })
            .collect::<Result<Vec<_>>>()
            .map(|parts| {
                parts
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
    } else if let Some(text) = content.get("text").and_then(Value::as_str) {
        Ok(text.into())
    } else {
        Ok(format!(
            "[Unsupported native content: {}]",
            content["content_type"].as_str().unwrap_or("unknown")
        ))
    }
}
fn has_rich(native: &Value) -> bool {
    let m = &native["metadata"];
    m.get("model_dil_v2").is_some_and(|v| !v.is_null())
        || m.get("genui_components")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty())
        || m.get("content_references")
            .and_then(Value::as_array)
            .is_some_and(|v| v.iter().any(|r| r["type"] == "client_defined_widget"))
}
fn interpret_prose(
    text: &str,
    native: &Value,
    artifact: &str,
    base: &str,
    citations: &citations::Citations,
    presentation: &mut Presentation,
) -> String {
    let input = components::normalize_layout(text, native);
    let input = illustrations::project(&input, artifact, base, presentation);
    crate::markdown::map_prose(&input, |text| {
        let shown = components::convert(text, native, artifact, citations, presentation);
        markers::convert(&shown, artifact, citations, presentation)
    })
}
pub fn interpret(native: &Value, conversation_id: &str) -> Result<Presentation> {
    let original = native_text(native)?;
    let mut presentation = Presentation {
        text: original.clone(),
        artifacts: vec![],
        issues: vec![],
        references: vec![],
    };
    if native.pointer("/author/role").and_then(Value::as_str) == Some("user") {
        return Ok(presentation);
    }
    let node = native["id"]
        .as_str()
        .context("native message identity absent")?;
    let base = format!(
        "assets/chatgpt/{}/{}",
        component(conversation_id)?,
        component(node)?
    );
    let artifact = format!("{base}.json");
    let citations = citations::Citations::new(native, &original);
    let structured = native
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts.iter().any(|p| {
                !p.is_string() && p.get("text").is_none() && p.get("asset_pointer").is_none()
            })
        });
    let input = if structured {
        native["content"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| {
                part.as_str()
                    .or_else(|| part.get("text").and_then(Value::as_str))
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        let label = part
                            .get("content_type")
                            .and_then(Value::as_str)
                            .unwrap_or("structured content");
                        presentation.source_link(
                            &format!("Native {label}"),
                            &artifact,
                            ReferenceKind::Label,
                        )
                    })
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        original.clone()
    };
    presentation.text = interpret_prose(
        &input,
        native,
        &artifact,
        &base,
        &citations,
        &mut presentation,
    );
    let rich = has_rich(native);
    if presentation.text.trim().is_empty()
        && rich
        && let Some(fallback) = native
            .pointer("/metadata/model_dil_v2/fallbackMarkdown")
            .and_then(Value::as_str)
    {
        presentation.text = interpret_prose(
            fallback,
            native,
            &artifact,
            &base,
            &citations,
            &mut presentation,
        );
    }
    Ok(presentation)
}

fn generated_link(asset: &str) -> String {
    format!("[Generated image](asessions-media:chatgpt/{asset})")
}
fn media_message(text: String, timestamp_ms: Option<i64>) -> Message {
    Message {
        role: Role::Assistant,
        kind: MessageKind::Dialog,
        text,
        timestamp_ms,
        actions_before: 0,
    }
}
pub fn generated_messages(graph: &Graph) -> Result<BTreeMap<String, Message>> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (asset, reference) in graph.generated_assets() {
        if let Some(id) = reference["messageId"].as_str()
            && graph.nodes.contains_key(id)
        {
            grouped
                .entry(id.into())
                .or_default()
                .push(generated_link(&asset));
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(id, links)| {
            let ts = graph.nodes[&id]
                .message
                .as_ref()
                .and_then(|m| m["create_time"].as_f64())
                .map(|ts| (ts * 1000.) as i64);
            (id, media_message(links.join("\n\n"), ts))
        })
        .collect())
}
pub fn orphaned_media_messages(graph: &Graph) -> Result<Vec<Message>> {
    let links = graph
        .generated_assets()
        .into_iter()
        .filter(|(_, r)| {
            r["messageId"]
                .as_str()
                .is_none_or(|id| !graph.nodes.contains_key(id))
        })
        .map(|(id, _)| generated_link(&id))
        .collect::<Vec<_>>();
    Ok(if links.is_empty() {
        vec![]
    } else {
        vec![media_message(
            format!(
                "Generated images identified by the export; originating messages are absent.\n\n{}",
                links.join("\n\n")
            ),
            graph.created.map(|ts| (ts * 1000.) as i64),
        )]
    })
}

pub fn material(graph: &Graph, archive_root: &Path) -> Result<RenderMaterial> {
    let mut media = Vec::new();
    let mut messages = Vec::new();
    let mut artifacts = Vec::new();
    let mut references = Vec::new();
    for (id, node) in &graph.nodes {
        if let Some(native) = &node.message
            && super::graph::dialog_role(native).is_some()
        {
            let mut native = native.clone();
            if native.get("id").is_none() {
                native["id"] = json!(id);
            }
            let shown = interpret(&native, &graph.id)?;
            if let Some(original) = super::graph::dialog(&native, &graph.id)? {
                messages.push(crate::model::MessagePresentation {
                    original,
                    text: shown.text,
                });
            }
            artifacts.extend(shown.artifacts);
            references.extend(shown.references);
        }
    }
    let generated = graph.generated_assets();
    if generated.is_empty() {
        return Ok(RenderMaterial {
            media,
            messages,
            assets: artifacts,
            references,
        });
    }
    let store = crate::media::MediaStore::new(archive_root);
    for asset in generated.keys() {
        let record = store.reference("chatgpt", asset)?;
        media.push(crate::media::MediaLink {
            alias: format!("asessions-media:chatgpt/{asset}"),
            media: record
                .filter(|r| store.object(&r.media).is_file())
                .map(|r| r.media),
        });
    }
    Ok(RenderMaterial {
        media,
        messages,
        assets: artifacts,
        references,
    })
}
