//! Streaming Bundle transport for content-addressed bytes and their metadata.
use super::{MediaId, MediaRecord, hash_file};
use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Write},
    path::{Component, Path},
};
pub(crate) fn append<W: Write>(tar: &mut tar::Builder<W>, archive: &Path) -> Result<()> {
    let root = archive.join("media");
    if !root.exists() {
        return Ok(());
    }
    fn visit<W: Write>(tar: &mut tar::Builder<W>, archive: &Path, dir: &Path) -> Result<()> {
        let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if entry.file_name() == "reports" {
                continue;
            }
            ensure!(!entry.file_type()?.is_symlink(), "symlink in Archive media");
            if entry.file_type()?.is_dir() {
                visit(tar, archive, &entry.path())?;
            } else {
                let path = entry.path();
                let relative = path.strip_prefix(archive)?;
                validate_path(relative)?;
                let mut file = std::fs::File::open(&path)?;
                let mut header = tar::Header::new_gnu();
                header.set_size(file.metadata()?.len());
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, relative, &mut file)?;
            }
        }
        Ok(())
    }
    visit(tar, archive, &root)
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn validate_path(path: &Path) -> Result<()> {
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(_))),
        "unsafe media Bundle path"
    );
    let parts = path
        .iter()
        .map(|s| s.to_str().context("non-UTF8 media path"))
        .collect::<Result<Vec<_>>>()?;
    let valid = match parts.as_slice() {
        ["media", "objects", name] => hash(name.strip_suffix(".meta.json").unwrap_or(name)),
        ["media", "references", source, name] => {
            source
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && name.strip_suffix(".json").is_some_and(hash)
        }
        _ => false,
    };
    ensure!(valid, "invalid media Bundle entry {}", path.display());
    Ok(())
}
pub(crate) fn ingest(archive: &Path, path: &Path, reader: &mut impl Read) -> Result<()> {
    validate_path(path)?;
    let destination = archive.join(path);
    let parent = destination.parent().context("media parent missing")?;
    std::fs::create_dir_all(parent)?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(reader, &mut stage)?;
    stage.as_file().sync_all()?;
    let name = path.file_name().unwrap().to_str().unwrap();
    if path.starts_with("media/objects") {
        if let Some(expected) = name.strip_suffix(".meta.json") {
            let id: MediaId = serde_json::from_reader(std::fs::File::open(stage.path())?)?;
            id.validate()?;
            ensure!(id.sha256 == expected, "media metadata identity mismatch");
            ensure!(
                hash_file(&archive.join("media/objects").join(&id.sha256))?
                    == (id.sha256.clone(), id.bytes),
                "media metadata bytes mismatch"
            );
            if destination.exists() {
                let old: MediaId = serde_json::from_reader(std::fs::File::open(&destination)?)?;
                ensure!(old == id, "conflicting Session Media metadata");
            }
        } else {
            ensure!(
                hash_file(stage.path())?.0 == name,
                "bundled media hash mismatch"
            );
        }
    } else {
        let incoming: MediaRecord = serde_json::from_reader(std::fs::File::open(stage.path())?)?;
        incoming.media.validate()?;
        let object = archive.join("media/objects").join(&incoming.media.sha256);
        let canonical: MediaId = serde_json::from_reader(std::fs::File::open(
            object.with_file_name(format!("{}.meta.json", incoming.media.sha256)),
        )?)?;
        ensure!(
            canonical == incoming.media
                && hash_file(&object)? == (canonical.sha256.clone(), canonical.bytes),
            "bundled media reference mismatch"
        );
        if destination.exists() {
            let old: MediaRecord = serde_json::from_reader(std::fs::File::open(&destination)?)?;
            if (old.quality, &old.media.sha256) > (incoming.quality, &incoming.media.sha256) {
                return Ok(());
            }
        }
        let mut metadata = incoming.metadata.clone();
        super::sanitize_metadata(&mut metadata);
        ensure!(
            metadata == incoming.metadata,
            "signed URL parameters in bundled media metadata"
        );
    }
    stage.persist(&destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conflicting_metadata_cannot_poison_existing_media() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::media::MediaStore::new(temp.path());
        let id = store.put(b"image".as_slice(), "image/png").unwrap();
        let mut wrong = id.clone();
        wrong.bytes += 1;
        let path = std::path::PathBuf::from(format!("media/objects/{}.meta.json", id.sha256));
        assert!(
            ingest(
                temp.path(),
                &path,
                &mut serde_json::to_vec(&wrong).unwrap().as_slice()
            )
            .is_err()
        );
        assert_eq!(store.objects().unwrap(), vec![id]);
    }
}
