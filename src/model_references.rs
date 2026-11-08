//! Generated references are presentation facts, independent of native formats.
use super::RenderAsset;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Citation,
    Label,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceTarget {
    Url(String),
    Asset(PathBuf),
}
#[derive(Debug, Clone)]
pub struct RenderReference {
    pub alias: String,
    pub target: ReferenceTarget,
    pub label: String,
    pub kind: ReferenceKind,
}
impl RenderReference {
    pub fn escape_label(label: &str) -> String {
        let mut escaped = String::new();
        for ch in label.chars() {
            match ch {
                '\\' | '[' | ']' | '*' | '_' | '|' | '~' => {
                    escaped.push('\\');
                    escaped.push(ch);
                }
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '&' => escaped.push_str("&amp;"),
                '`' => escaped.push_str("&#96;"),
                '\u{e200}' | '\u{e201}' | '\u{e202}' | '\u{e205}' => {
                    escaped.push_str(&format!("&#x{:X};", ch as u32));
                }
                '\n' | '\r' => escaped.push(' '),
                _ => escaped.push(ch),
            }
        }
        escaped
    }
    pub fn new(target: ReferenceTarget, label: String, kind: ReferenceKind) -> Self {
        let key = format!("{target:?}:{kind:?}");
        Self {
            alias: format!("asessions-reference:{:x}", Sha256::digest(key.as_bytes())),
            target,
            label,
            kind,
        }
    }
    pub fn markdown(&self, label: &str) -> String {
        format!("[{}]({})", Self::escape_label(label), self.alias)
    }
}
#[derive(Debug, Clone)]
pub struct MessagePresentation {
    pub original: super::Message,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct RenderMaterial {
    pub media: Vec<crate::media::MediaLink>,
    pub messages: Vec<MessagePresentation>,
    pub assets: Vec<RenderAsset>,
    pub references: Vec<RenderReference>,
}
