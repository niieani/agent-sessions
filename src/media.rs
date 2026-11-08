//! Durable source-neutral Session Media; no View paths depend on storage location.
pub(crate) mod bundle;
mod materialize;
use anyhow::{Context, Result, ensure};
pub use materialize::{MaterializationMethod, materialize};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaId {
    pub sha256: String,
    pub bytes: u64,
    pub media_type: String,
}
impl MediaId {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.sha256.len() == 64
                && self
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid Session Media identity"
        );
        ensure!(
            !self.media_type.trim().is_empty(),
            "Session Media MIME missing"
        );
        Ok(())
    }
    pub fn view_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "media/sha256/{}.{}",
            self.sha256,
            extension(&self.media_type)
        ))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaRecord {
    pub media: MediaId,
    pub quality: u8,
    pub metadata: Value,
}
#[derive(Debug, Clone)]
pub struct MediaLink {
    pub alias: String,
    pub media: Option<MediaId>,
}
pub struct MediaStore {
    root: PathBuf,
}
impl MediaStore {
    pub fn new(archive: &Path) -> Self {
        Self {
            root: archive.join("media"),
        }
    }
    pub fn object(&self, id: &MediaId) -> PathBuf {
        self.root.join("objects").join(&id.sha256)
    }
    pub fn put(&self, mut input: impl Read, media_type: &str) -> Result<MediaId> {
        ensure!(!media_type.trim().is_empty(), "Session Media MIME missing");
        let dir = self.root.join("objects");
        std::fs::create_dir_all(&dir)?;
        let mut stage = tempfile::NamedTempFile::new_in(&dir)?;
        let mut digest = Sha256::new();
        let mut bytes = 0;
        let mut buffer = [0u8; 65536];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            stage.write_all(&buffer[..n])?;
            digest.update(&buffer[..n]);
            bytes += n as u64;
        }
        stage.as_file().sync_all()?;
        let id = MediaId {
            sha256: format!("{:x}", digest.finalize()),
            bytes,
            media_type: media_type.into(),
        };
        let object = self.object(&id);
        if object.exists() {
            ensure!(
                hash_file(&object)? == (id.sha256.clone(), bytes),
                "Session Media object corrupted"
            );
        } else {
            stage.persist(&object)?;
        }
        let metadata = dir.join(format!("{}.meta.json", id.sha256));
        if metadata.exists() {
            let old: MediaId = serde_json::from_reader(File::open(&metadata)?)?;
            ensure!(old == id, "conflicting Session Media MIME");
        } else {
            atomic_json(&metadata, &id)?;
        }
        Ok(id)
    }
    fn reference_path(&self, source: &str, key: &str) -> Result<PathBuf> {
        ensure!(
            !source.is_empty()
                && source
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "unsafe media Source"
        );
        Ok(self
            .root
            .join("references")
            .join(source)
            .join(format!("{:x}.json", Sha256::digest(key.as_bytes()))))
    }
    pub fn reference(&self, source: &str, key: &str) -> Result<Option<MediaRecord>> {
        let path = self.reference_path(source, key)?;
        match File::open(&path) {
            Ok(file) => {
                let record: MediaRecord = serde_json::from_reader(file)?;
                record.media.validate()?;
                Ok(Some(record))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading Session Media reference"),
        }
    }
    pub fn record(
        &self,
        source: &str,
        key: &str,
        media: MediaId,
        quality: u8,
        mut metadata: Value,
    ) -> Result<()> {
        media.validate()?;
        sanitize_metadata(&mut metadata);
        if self
            .reference(source, key)?
            .is_some_and(|old| (old.quality, &old.media.sha256) > (quality, &media.sha256))
        {
            return Ok(());
        }
        let path = self.reference_path(source, key)?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        atomic_json(
            &path,
            &MediaRecord {
                media,
                quality,
                metadata,
            },
        )
    }
    pub fn objects(&self) -> Result<Vec<MediaId>> {
        let dir = self.root.join("objects");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut result: Vec<MediaId> = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let p = entry?.path();
            if p.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".meta.json")
            {
                let id: MediaId = serde_json::from_reader(File::open(&p)?)?;
                id.validate()?;
                ensure!(
                    p.file_name().unwrap().to_string_lossy() == format!("{}.meta.json", id.sha256),
                    "media metadata identity mismatch"
                );
                result.push(id);
            }
        }
        result.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        Ok(result)
    }
}
pub fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        bytes += n as u64;
    }
    Ok((format!("{:x}", hash.finalize()), bytes))
}
fn extension(mime: &str) -> &str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut stage =
        tempfile::NamedTempFile::new_in(path.parent().context("metadata parent absent")?)?;
    serde_json::to_writer_pretty(&mut stage, value)?;
    stage.as_file().sync_all()?;
    stage.persist(path)?;
    Ok(())
}
pub fn sanitize_metadata(value: &mut Value) {
    match value {
        Value::String(text) if text.starts_with("https://") || text.starts_with("http://") => {
            if let Some((base, query)) = text.split_once('?') {
                let params = query
                    .split('&')
                    .filter(|p| {
                        !matches!(
                            p.split('=')
                                .next()
                                .unwrap_or("")
                                .to_ascii_lowercase()
                                .as_str(),
                            "sig"
                                | "signature"
                                | "token"
                                | "access_token"
                                | "se"
                                | "sp"
                                | "sv"
                                | "sr"
                                | "st"
                                | "skt"
                                | "ske"
                                | "sks"
                                | "skv"
                                | "skoid"
                                | "sktid"
                        ) && !p.to_ascii_lowercase().starts_with("x-amz-")
                            && !p.to_ascii_lowercase().starts_with("x-goog-")
                    })
                    .collect::<Vec<_>>();
                *text = if params.is_empty() {
                    base.into()
                } else {
                    format!("{base}?{}", params.join("&"))
                };
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_metadata(item)
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                sanitize_metadata(item)
            }
        }
        _ => {}
    }
}
