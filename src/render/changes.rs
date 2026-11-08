//! Compare semantic inputs independently of templates and renderer output.
use crate::archive::db::RenderFileState;
use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Debug, serde::Serialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub change: &'static str,
    pub form_only: bool,
    pub basis: &'static str,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
}
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn semantic(value: &Value) -> String {
    hash(
        serde_json::to_string(value)
            .expect("JSON value serializes")
            .as_bytes(),
    )
}
pub(super) fn dialog_input(messages: &[crate::model::Message]) -> Value {
    json!(
        messages
            .iter()
            .map(|m| json!({"role":m.role,"kind":m.kind,"text":m.text}))
            .collect::<Vec<_>>()
    )
}
pub(super) fn digest_input(value: Option<&Value>) -> Value {
    value.map_or(Value::Null,|v|json!({"topic":v["topic"],"title":v["title"],"intent":v["intent"],"summary":v["summary"],"tldr":v["tldr"],"classification":v["classification"]}))
}
fn disk_files(root: &Path, dir: &Path, result: &mut BTreeSet<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            disk_files(root, &entry.path(), result)?;
        } else {
            result.insert(entry.path().strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}
fn signature(text: &str) -> Option<Value> {
    let dialog = super::parse_dialog(text).ok()?;
    if dialog.messages.is_empty() {
        return None;
    }
    let body = text
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---\n").map(|(_, s)| s))
        .unwrap_or(text);
    let prefix = body
        .lines()
        .take_while(|line| {
            !matches!(
                line.split([' ', '>']).next(),
                Some("<user" | "<assistant" | "<agent" | "<parent")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut visible = String::new();
    for event in pulldown_cmark::Parser::new(&prefix) {
        if let pulldown_cmark::Event::Text(text) | pulldown_cmark::Event::Code(text) = event {
            visible.push_str(&text);
            visible.push(' ');
        }
    }
    Some(
        json!({"dialog":dialog_input(&dialog.messages),"digest":visible.split_whitespace().collect::<Vec<_>>() }),
    )
}
pub(super) struct Comparison {
    pub files: Vec<FileChange>,
    pub states: BTreeMap<String, RenderFileState>,
}
pub(super) fn compare(
    output: &Path,
    writes: &BTreeMap<PathBuf, String>,
    assets: &BTreeMap<PathBuf, Vec<u8>>,
    expected: &BTreeSet<PathBuf>,
    inputs: &BTreeMap<PathBuf, (String, String)>,
    previous: &BTreeMap<String, RenderFileState>,
) -> Result<Comparison> {
    let mut live = BTreeSet::new();
    disk_files(output, output, &mut live)?;
    let mut verified = BTreeSet::new();
    for (path, state) in previous {
        let path = output.join(path);
        if path.is_file() && crate::media::hash_file(&path)?.0 == state.output_hash {
            verified.insert(state.semantic.clone());
        }
    }
    let mut result = Comparison {
        files: Vec::new(),
        states: BTreeMap::new(),
    };
    for (path, bytes) in writes
        .iter()
        .map(|(p, t)| (p, t.as_bytes()))
        .chain(assets.iter().map(|(p, b)| (p, b.as_slice())))
    {
        let (input, kind) = &inputs[path];
        let output_hash = hash(bytes);
        let key = path.to_string_lossy().into_owned();
        result.states.insert(
            key.clone(),
            RenderFileState {
                semantic: input.clone(),
                output_hash: output_hash.clone(),
                kind: kind.clone(),
            },
        );
        let old = output.join(path);
        if old.is_file() && crate::media::hash_file(&old)?.0 == output_hash {
            continue;
        }
        let before_sha256 = if live.contains(path) && old.is_file() {
            Some(crate::media::hash_file(&old)?.0)
        } else {
            None
        };
        let externally_changed = previous
            .get(&key)
            .is_some_and(|saved| Some(&saved.output_hash) != before_sha256.as_ref());
        let (form_only, basis) = if externally_changed {
            (false, "conservative")
        } else if verified.contains(input) {
            (true, "inputs")
        } else if old.is_file() && previous.get(&key).is_none() {
            let equal = std::fs::read_to_string(&old)
                .ok()
                .zip(std::str::from_utf8(bytes).ok())
                .is_some_and(|(old, new)| {
                    signature(&old)
                        .zip(signature(new))
                        .is_some_and(|(a, b)| a == b)
                });
            (
                equal,
                if equal {
                    "parsed-dialog"
                } else {
                    "conservative"
                },
            )
        } else {
            (false, "inputs")
        };
        result.files.push(FileChange {
            path: path.clone(),
            change: if live.contains(path) {
                "updated"
            } else {
                "created"
            },
            form_only,
            basis,
            before_sha256,
            after_sha256: Some(output_hash),
        });
    }
    for path in live.difference(expected) {
        let key = path.to_string_lossy();
        let before_sha256 = if output.join(path).is_file() {
            Some(crate::media::hash_file(&output.join(path))?.0)
        } else {
            None
        };
        let form_only = previous.get(key.as_ref()).is_some_and(|old| {
            before_sha256.as_ref() == Some(&old.output_hash)
                && (old.kind == "index"
                    || result
                        .states
                        .values()
                        .any(|new| new.semantic == old.semantic))
        });
        result.files.push(FileChange {
            path: path.clone(),
            change: "removed",
            form_only,
            before_sha256,
            after_sha256: None,
            basis: if previous.contains_key(key.as_ref()) {
                "inputs"
            } else {
                "conservative"
            },
        });
    }
    result.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}
