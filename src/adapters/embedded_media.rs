//! Source-native image blocks; streamed record-wise, decoded into shared media.
use super::Capture;
use crate::{
    media::{MediaLink, MediaStore},
    model::{Conversation, MessagePresentation, RenderMaterial, Role},
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::path::Path;
struct ImageMessage {
    role: Role,
    timestamp: Option<i64>,
    plain: String,
    shown: String,
    media: Vec<crate::media::MediaId>,
    /// False for records canonical extraction drops as noise; their images
    /// are archived as evidence but have no Message to present them in.
    dialog: bool,
}
fn prepare(capture: &Capture, root: &Path, persist: bool) -> Result<Vec<ImageMessage>> {
    let store = MediaStore::new(root);
    let mut messages = Vec::new();
    let mut reader = super::jsonl::open_capture(capture)?;
    let mut ordinal = 0usize;
    while let Some(row) = reader.next_record()? {
        ordinal += 1;
        let native = match capture.meta.source.as_str() {
            "claude" if matches!(row["type"].as_str(), Some("user" | "assistant")) => {
                &row["message"]
            }
            "codex" if row["type"] == "response_item" && row["payload"]["type"] == "message" => {
                &row["payload"]
            }
            _ => continue,
        };
        let role = match native["role"].as_str().or_else(|| row["type"].as_str()) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        let Some(blocks) = native["content"].as_array() else {
            continue;
        };
        let dialog = match (capture.meta.source.as_str(), role) {
            ("claude", Role::User) => !super::claude::is_noise_user(&row),
            ("claude", Role::Assistant) => !super::claude::is_noise_assistant(&row),
            _ => true,
        };
        let timestamp = row["timestamp"]
            .as_str()
            .map(crate::timefmt::parse_iso_ms)
            .transpose()?;
        let mut plain = Vec::new();
        let mut shown = Vec::new();
        let mut has_image = false;
        let mut image_media = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            match block["type"].as_str() {
                Some("text" | "input_text" | "output_text") => {
                    if let Some(text) = block["text"].as_str() {
                        plain.push(text.to_string());
                        shown.push(text.to_string());
                    }
                }
                Some("image" | "input_image") => {
                    has_image = true;
                    plain.push("[image]".into());
                    let data = if block["type"] == "image" {
                        block["source"]["data"].as_str().map(|data| {
                            (block["source"]["media_type"].as_str().unwrap_or(""), data)
                        })
                    } else {
                        block["image_url"]
                            .as_str()
                            .and_then(|url| url.strip_prefix("data:"))
                            .and_then(|url| url.split_once(";base64,"))
                    };
                    let Some((mime, data)) = data else {
                        shown.push("[Missing image: bytes absent from Capture]".into());
                        continue;
                    };
                    ensure!(!mime.is_empty(), "embedded image MIME missing");
                    let decoder = base64::read::DecoderReader::new(
                        data.as_bytes(),
                        &base64::engine::general_purpose::STANDARD,
                    );
                    let key = format!(
                        "{}/{}/{index}/{:x}",
                        capture.meta.native_id,
                        row["uuid"]
                            .as_str()
                            .or_else(|| native["id"].as_str())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("record-{ordinal}")),
                        Sha256::digest(data.as_bytes())
                    );
                    let media = if persist {
                        let media = store.put(decoder, mime)?;
                        store.record(&capture.meta.source,&key,media.clone(),2,serde_json::json!({"sessionId":capture.meta.native_id,"role":role.as_str(),"timestamp":timestamp,"block":index}))?;
                        Some(media)
                    } else {
                        store
                            .reference(&capture.meta.source, &key)?
                            .filter(|r| store.object(&r.media).is_file())
                            .map(|r| r.media)
                    };
                    if let Some(id) = &media {
                        image_media.push(id.clone());
                    }
                    shown.push(media.map_or_else(
                        || "[Missing image: not archived]".into(),
                        |id| {
                            format!(
                                "![Image](asessions-media:sha256/{})",
                                id.view_path().file_name().unwrap().to_string_lossy()
                            )
                        },
                    ));
                }
                _ => {}
            }
        }
        if has_image {
            let normalize = |parts: &[String]| {
                if capture.meta.source == "codex" && role == Role::User {
                    super::codex::media_user_text(parts)
                } else {
                    Some(parts.join("\n\n"))
                }
            };
            if let (Some(plain), Some(shown)) = (normalize(&plain), normalize(&shown)) {
                messages.push(ImageMessage {
                    role,
                    timestamp,
                    plain,
                    shown,
                    media: image_media,
                    dialog,
                });
            }
        }
    }
    Ok(messages)
}
pub(super) fn archive(capture: &Capture, root: &Path) -> Result<()> {
    prepare(capture, root, true)?;
    Ok(())
}
pub(super) fn material(
    capture: &Capture,
    root: &Path,
    conversation: Option<Conversation>,
) -> Result<RenderMaterial> {
    let mut material = RenderMaterial::default();
    let Some(conversation) = conversation else {
        return Ok(material);
    };
    let mut matched = std::collections::BTreeSet::new();
    for image in prepare(capture, root, false)?
        .into_iter()
        .filter(|image| image.dialog)
    {
        for id in &image.media {
            material.media.push(MediaLink {
                alias: format!(
                    "asessions-media:sha256/{}",
                    id.view_path().file_name().unwrap().to_string_lossy()
                ),
                media: Some(id.clone()),
            });
        }
        let (index, original) = conversation
            .messages
            .iter()
            .enumerate()
            .find(|(index, m)| {
                !matched.contains(index)
                    && m.role == image.role
                    && m.timestamp_ms == image.timestamp
                    && m.text.contains(&image.plain)
            })
            .context("image block cannot be matched to canonical Message")?;
        matched.insert(index);
        let text = original.text.replacen(&image.plain, &image.shown, 1);
        material.messages.push(MessagePresentation {
            original: original.clone(),
            text,
        });
    }
    Ok(material)
}
