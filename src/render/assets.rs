//! Elected Adapter artifacts are disposable View assets, rebased per Transcript.
use crate::adapters::{Capture, CaptureMeta, CaptureOutcome, adapter_for};
use crate::archive::{Archive, db::SessionRow};
use crate::model::{Message, RenderAsset, RenderReference};
use anyhow::{Context, Result, ensure};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[derive(Default)]
pub(super) struct Assets {
    presentations: BTreeMap<i64, Vec<crate::model::MessagePresentation>>,
    pub semantic_inputs: BTreeMap<PathBuf, String>,
    pub media_paths: BTreeSet<PathBuf>,
    pub errors: Vec<String>,
    pub writes: BTreeMap<PathBuf, Vec<u8>>,
    references: BTreeMap<i64, BTreeMap<String, Option<(PathBuf, bool)>>>,
    pub generated: BTreeMap<i64, BTreeMap<String, RenderReference>>,
}
impl Assets {
    pub fn collect(
        archive: &Archive,
        sessions: &[SessionRow],
        rendered: &BTreeSet<i64>,
    ) -> Result<Self> {
        let db = crate::archive::Db::open_read_only(&archive.db_path())?;
        let mut wanted: BTreeMap<(String, String, String), Vec<i64>> = BTreeMap::new();
        for session in sessions.iter().filter(|s| rendered.contains(&s.id)) {
            let Some(host) = session.winner_host.as_ref() else {
                continue;
            };
            let Some(adapter) = adapter_for(&session.source) else {
                continue;
            };
            if !db.has_capture_presence(
                &session.source,
                adapter.capture_native_id(&session.native_id),
                host,
            )? {
                continue;
            }
            wanted
                .entry((
                    session.source.clone(),
                    adapter.capture_native_id(&session.native_id).to_string(),
                    host.clone(),
                ))
                .or_default()
                .push(session.id);
        }
        let mut assets = Self::default();
        let mut contexts = BTreeMap::new();
        for ((source, native_id, host), ids) in wanted {
            let stem = if matches!(source.as_str(), "cursor" | "t3") {
                crate::adapters::sqlite::capture_stem(&native_id)
            } else {
                native_id
            };
            let meta_path = archive
                .captures_dir()
                .join(&host)
                .join(&source)
                .join(format!("{stem}.meta.json"));
            if !contexts.contains_key(&source) {
                contexts.insert(
                    source.clone(),
                    adapter_for(&source)
                        .context("Adapter missing")?
                        .prepare_extraction(archive.root())?,
                );
            }
            let result = (|| -> Result<crate::model::RenderMaterial> {
                let meta: CaptureMeta = serde_json::from_reader(std::fs::File::open(&meta_path)?)?;
                let capture = Capture {
                    body_path: crate::archive::body_for_sidecar(&meta_path)?,
                    meta_path: meta_path.clone(),
                    meta,
                    written: false,
                    outcome: CaptureOutcome::Unchanged,
                };
                adapter_for(&source)
                    .context("Adapter missing")?
                    .render_material(&capture, archive.root(), &contexts[&source])
            })();
            let material = match result {
                Ok(material) => material,
                Err(error) => {
                    let message = format!("{source}/{stem}: render preparation failed: {error:#}");
                    tracing::warn!("{message}");
                    assets.errors.push(message);
                    continue;
                }
            };
            for session in ids {
                assets
                    .presentations
                    .insert(session, material.messages.clone());
                let semantic = super::changes::semantic(
                    &serde_json::json!({"source":source,"session":session,"dialog":super::changes::dialog_input(&db.messages(session)?)}),
                );
                for asset in &material.assets {
                    assets
                        .semantic_inputs
                        .insert(asset.relative_path.clone(), semantic.clone());
                    assets.register(session, asset.clone())?;
                }
                for media in &material.media {
                    let target = media
                        .media
                        .as_ref()
                        .map(|id| (id.view_path(), id.media_type.starts_with("image/")));
                    if let Some((path, _)) = &target {
                        assets.media_paths.insert(path.clone());
                    }
                    assets
                        .references
                        .entry(session)
                        .or_default()
                        .insert(media.alias.clone(), target);
                }
                for reference in &material.references {
                    assets
                        .generated
                        .entry(session)
                        .or_default()
                        .insert(reference.alias.clone(), reference.clone());
                }
            }
        }
        Ok(assets)
    }
    fn register(&mut self, session: i64, asset: RenderAsset) -> Result<()> {
        use std::path::Component;
        ensure!(
            !asset.relative_path.as_os_str().is_empty()
                && asset
                    .relative_path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "unsafe RenderAsset path"
        );
        if let Some(old) = self.writes.get(&asset.relative_path) {
            ensure!(*old == asset.bytes, "conflicting RenderAsset bytes");
        } else {
            self.writes.insert(asset.relative_path.clone(), asset.bytes);
        }
        let image = asset
            .media_type
            .as_ref()
            .is_some_and(|mime| mime.starts_with("image/"));
        let references = self.references.entry(session).or_default();
        for reference in std::iter::once(asset.relative_path.to_string_lossy().into_owned())
            .chain(asset.references)
        {
            let value = Some((asset.relative_path.clone(), image));
            if let Some(old) = references.insert(reference, value.clone()) {
                ensure!(old == value, "ambiguous RenderAsset reference");
            }
        }
        Ok(())
    }
    pub fn media_identity(&self, session: i64) -> Vec<(String, Option<PathBuf>)> {
        self.references
            .get(&session)
            .into_iter()
            .flat_map(|refs| refs.iter())
            .filter(|(alias, _)| alias.starts_with("asessions-media:"))
            .map(|(alias, target)| (alias.clone(), target.as_ref().map(|(path, _)| path.clone())))
            .collect()
    }
    pub fn messages(&self, session: i64, view: &Path, messages: &[Message]) -> Vec<Message> {
        let empty = BTreeMap::new();
        let references = self.references.get(&session).unwrap_or(&empty);
        let link =
            Regex::new(r"(!?)\[([^\]\n]*)\]\(([^)\n]+)\)").expect("literal asset-link regex");
        let mut occurrences = BTreeMap::<String, usize>::new();
        messages
            .iter()
            .map(|message| {
                let key = serde_json::to_string(&(
                    message.role.as_str(),
                    message.timestamp_ms,
                    &message.text,
                ))
                .expect("Message identity serializes");
                let occurrence = occurrences.entry(key).or_default();
                let n = *occurrence;
                *occurrence += 1;
                let mut prepared = message.clone();
                let text = self
                    .presentations
                    .get(&session)
                    .and_then(|entries| {
                        entries
                            .iter()
                            .filter(|entry| {
                                entry.original.role == message.role
                                    && entry.original.timestamp_ms == message.timestamp_ms
                                    && entry.original.text == message.text
                            })
                            .nth(n)
                    })
                    .map_or(message.text.as_str(), |entry| entry.text.as_str());
                prepared.text = crate::markdown::map_prose(text, |prose| {
                    rewrite(prose, &link, references, view)
                });
                prepared
            })
            .collect()
    }
}
fn rewrite(
    text: &str,
    pattern: &Regex,
    references: &BTreeMap<String, Option<(PathBuf, bool)>>,
    view: &Path,
) -> String {
    pattern
        .replace_all(text, |capture: &regex::Captures<'_>| {
            let Some(target) = references.get(&capture[3]) else {
                return if capture[3].starts_with("asessions-media:") {
                    format!("[Missing image: {}]", &capture[2])
                } else {
                    capture[0].to_string()
                };
            };
            let Some((target, image)) = target else {
                return format!(
                    "[Missing image: {} ({}; not captured)]",
                    &capture[2],
                    capture[3].rsplit('/').next().unwrap_or("unknown")
                );
            };
            let bang = if *image || !capture[1].is_empty() {
                "!"
            } else {
                ""
            };
            format!(
                "{bang}[{}]({})",
                &capture[2],
                super::relative_href(view.parent().unwrap_or_else(|| Path::new("")), target)
            )
        })
        .into_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MessageKind, Role};
    #[test]
    fn rebase_registered_links_images_and_aliases_but_leave_code_and_unregistered_urls() {
        let mut assets = Assets::default();
        assets
            .register(
                1,
                RenderAsset {
                    relative_path: "assets/image.png".into(),
                    bytes: vec![1],
                    references: vec!["asset:one".into()],
                    media_type: Some("image/png".into()),
                },
            )
            .unwrap();
        let text = "[Image](asset:one) [external](https://example.com)\n`[literal](asset:one)`\n```md\n[code](asset:one)\n```\n";
        let message = Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: text.into(),
            timestamp_ms: Some(1),
            actions_before: 0,
        };
        let result = assets.messages(1, Path::new("loose/day/file.md"), &[message]);
        assert_eq!(
            result[0].text,
            text.replacen("[Image](asset:one)", "![Image](../../assets/image.png)", 1)
        );
    }
}
