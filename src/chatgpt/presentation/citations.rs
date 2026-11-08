use super::Presentation;
use crate::model::ReferenceKind;
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::LazyLock;

static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new("cite([^]+)").unwrap());
static LEGACY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"【([^【】†\n]+)†([^【】†\n]+)】").unwrap());
static IMAGE_FETCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"【\{\s*"image_fetch"\s*:\s*("(?:[^"\\]|\\.)*")\s*\}】"#).unwrap()
});
static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^turn\d+([a-zA-Z]+\d+)$").unwrap());

#[derive(Clone, Debug, PartialEq, Eq)]
struct Link {
    title: String,
    url: String,
}
pub(super) struct Citations {
    exact: BTreeMap<String, Vec<Link>>,
    rebased: BTreeMap<String, Vec<Link>>,
}
pub(super) fn url(value: &str) -> Option<String> {
    if value
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | '"'))
    {
        return None;
    }
    let absolute = if value.starts_with("https://") || value.starts_with("http://") {
        value.to_owned()
    } else if value.starts_with("/c/") {
        format!("https://chatgpt.com{value}")
    } else {
        return None;
    };
    Some(
        absolute
            .replace('\\', "%5C")
            .replace('(', "%28")
            .replace(')', "%29"),
    )
}
fn references(value: &Value) -> Vec<String> {
    if let Some(s) = value.as_str() {
        return vec![s.into()];
    }
    if let Some(items) = value.as_array() {
        return items.iter().flat_map(references).collect();
    }
    match (
        value["turn_index"].as_u64(),
        value["ref_type"].as_str(),
        value["ref_index"].as_u64(),
    ) {
        (Some(turn), Some(kind), Some(index)) => vec![format!("turn{turn}{kind}{index}")],
        _ => vec![],
    }
}
fn source_id(id: &str) -> &str {
    // Native line locators refine a source, not its identity. The original
    // marker and locator remain unchanged in the Capture/native artifact.
    id.split_once('').map_or(id, |(source, _)| source)
}
fn link(value: &Value) -> Option<Link> {
    let href = url(value.get("url")?.as_str()?)?;
    let title = value["title"]
        .as_str()
        .filter(|title| !title.trim().is_empty())
        .or_else(|| {
            value["attribution"]
                .as_str()
                .filter(|title| !title.trim().is_empty())
        })
        .unwrap_or(&href);
    Some(Link {
        title: title.into(),
        url: href,
    })
}
fn collect(value: &Value, index: &mut BTreeMap<String, Vec<Link>>) {
    match value {
        Value::Object(obj) => {
            if let Some(link) = link(value) {
                let refs = references(&value["refs"])
                    .into_iter()
                    .chain(references(&value["ref_id"]));
                for id in refs {
                    let entries = index.entry(source_id(&id).into()).or_default();
                    if !entries.contains(&link) {
                        entries.push(link.clone());
                    }
                }
            }
            for item in obj.values() {
                collect(item, index);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, index);
            }
        }
        _ => {}
    }
}
impl Citations {
    pub(super) fn new(native: &Value, text: &str) -> Self {
        let mut exact = BTreeMap::new();
        collect(&native["metadata"], &mut exact);
        // Export revisions sometimes renumber turns in metadata. Only align
        // ordered groups when every reference kind/index agrees one-to-one.
        let mut markers = Vec::new();
        // Count the same guarded prose that interpretation will transform;
        // citations in fenced/inline examples are literal user-facing code.
        crate::markdown::map_prose(text, |prose| {
            markers.extend(MARKER.captures_iter(prose).map(|c| {
                c[1].split('')
                    .map(|id| source_id(id).to_owned())
                    .collect::<Vec<_>>()
            }));
            prose.into()
        });
        let groups = native
            .pointer("/metadata/content_references")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|g| g["type"] == "grouped_webpages")
            .collect::<Vec<_>>();
        let mut rebased = BTreeMap::new();
        if markers.len() == groups.len() {
            let mut valid = true;
            for (ids, group) in markers.iter().zip(groups) {
                let items = group["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                let bindings = items
                    .iter()
                    .flat_map(|item| {
                        let target = link(item);
                        references(&item["refs"])
                            .into_iter()
                            .map(move |id| (id, target.clone()))
                    })
                    .collect::<Vec<_>>();
                if ids.len() != bindings.len()
                    || !ids.iter().zip(&bindings).all(|(a, (b, target))| {
                        REF.captures(a)
                            .zip(REF.captures(source_id(b)))
                            .is_some_and(|(a, b)| a[1] == b[1])
                            && target.is_some()
                    })
                {
                    valid = false;
                    break;
                }
                for (id, (_, link)) in ids.iter().zip(bindings) {
                    let link = link.expect("validated citation target");
                    let entries = rebased.entry(id.clone()).or_insert_with(Vec::new);
                    if !entries.contains(&link) {
                        entries.push(link);
                    }
                }
            }
            if !valid {
                rebased.clear();
            }
        }
        Self { exact, rebased }
    }
    pub(super) fn resolve(&self, ids: &[String], p: &mut Presentation) -> Option<String> {
        let mut links = Vec::new();
        for id in ids {
            let id = source_id(id);
            for link in self.rebased.get(id).or_else(|| self.exact.get(id))? {
                if !links.iter().any(|old: &Link| old.url == link.url) {
                    links.push(link.clone());
                }
            }
        }
        (!links.is_empty()).then(|| {
            links
                .into_iter()
                .map(|l| p.source_link(&l.title, &l.url, ReferenceKind::Citation))
                .collect::<Vec<_>>()
                .join(", ")
        })
    }
    pub(super) fn replace(&self, text: &str, p: &mut Presentation, artifact: &str) -> String {
        let text = LEGACY.replace_all(text, |c: &regex::Captures<'_>| {
            let image = &c[2] == "embed_image";
            self.resolve(&[c[1].into()], p).unwrap_or_else(|| {
                p.issues
                    .push(format!("unresolved-legacy-reference:{}", &c[1]));
                p.source_link(
                    if image {
                        "Image not captured"
                    } else {
                        "Unresolved citation"
                    },
                    artifact,
                    ReferenceKind::Label,
                )
            })
        });
        let text = IMAGE_FETCH.replace_all(&text, |c: &regex::Captures<'_>| {
            let label: String = serde_json::from_str(&c[1]).expect("matched JSON string");
            p.issues.push("uncaptured-legacy-image".into());
            p.source_link(
                &format!("Image not captured: {label}"),
                artifact,
                ReferenceKind::Label,
            )
        });
        MARKER
            .replace_all(&text, |c: &regex::Captures<'_>| {
                let ids = c[1].split('').map(str::to_owned).collect::<Vec<_>>();
                self.resolve(&ids, p).unwrap_or_else(|| {
                    p.issues
                        .push(format!("unresolved-citation:{}", ids.join(",")));
                    p.source_link("Unresolved citation", artifact, ReferenceKind::Label)
                })
            })
            .into_owned()
    }
}
