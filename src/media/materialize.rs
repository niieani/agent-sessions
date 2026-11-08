use super::{MediaStore, hash_file};
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::path::Path;
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MaterializationMethod {
    Copy,
    Clonefile,
    Hardlink,
    Symlink,
}
pub fn materialize(archive: &Path, output: &Path, method: MaterializationMethod) -> Result<usize> {
    for dir in [
        output.to_path_buf(),
        output.join("media"),
        output.join("media/sha256"),
    ] {
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "media directory must be a real directory: {}",
                dir.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let store = MediaStore::new(archive);
    let mut count = 0;
    for media in store.objects()? {
        let source = std::fs::canonicalize(store.object(&media))?;
        ensure!(
            hash_file(&source)? == (media.sha256.clone(), media.bytes),
            "Session Media object corrupted"
        );
        let destination = output.join(media.view_path());
        std::fs::create_dir_all(destination.parent().unwrap())?;
        let stage = tempfile::tempdir_in(destination.parent().unwrap())?;
        let staged = stage.path().join("media");
        match method {
            MaterializationMethod::Copy => {
                std::fs::copy(&source, &staged)?;
            }
            MaterializationMethod::Hardlink => std::fs::hard_link(&source, &staged)?,
            MaterializationMethod::Symlink => std::os::unix::fs::symlink(&source, &staged)?,
            MaterializationMethod::Clonefile => {
                let mut command = std::process::Command::new("cp");
                if cfg!(target_os = "macos") {
                    command.arg("-c");
                } else {
                    command.arg("--reflink=always");
                }
                let result = command.arg(&source).arg(&staged).output()?;
                ensure!(
                    result.status.success(),
                    "clonefile materialization failed: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
        }
        std::fs::rename(&staged, &destination)?;
        count += 1;
    }
    Ok(count)
}
