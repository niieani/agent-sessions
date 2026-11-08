//! One path authority for Capture writers and identity-based readers.
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

pub struct CapturePaths {
    pub body: PathBuf,
    pub meta: PathBuf,
}

pub fn capture_paths(
    root: &Path,
    host: &str,
    source: &str,
    native_id: &str,
) -> Result<CapturePaths> {
    let adapter = super::adapter_for(source)
        .with_context(|| format!("no Capture path policy for Source {source:?}"))?;
    for value in [host, source] {
        ensure!(
            !value.is_empty() && !value.contains(['/', '\\', '\0']) && !matches!(value, "." | ".."),
            "unsafe path component in Capture namespace {value:?}"
        );
    }
    let stem = adapter.capture_stem(native_id);
    ensure!(
        !stem.is_empty()
            && !stem.contains(['\\', '\0'])
            && !Path::new(&stem).is_absolute()
            && stem
                .split('/')
                .all(|part| !part.is_empty() && !matches!(part, "." | "..")),
        "unsafe path component in Capture identity path {native_id:?}"
    );
    let base = root.join(host).join(source);
    Ok(CapturePaths {
        body: base.join(format!("{stem}.{}.zst", adapter.capture_extension())),
        meta: base.join(format!("{stem}.meta.json")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policies_preserve_every_source_layout_and_branch_capture_identity() {
        for source in ["claude", "codex", "pi", "cursor", "t3", "chatgpt"] {
            let uuid = "11111111-1111-4111-8111-111111111111";
            assert_eq!(
                capture_paths(Path::new("captures"), "host", source, uuid)
                    .unwrap()
                    .meta,
                Path::new("captures")
                    .join("host")
                    .join(source)
                    .join(format!("{uuid}.meta.json"))
            );
        }
        for source in ["cursor", "t3"] {
            for id in ["draft/opaque".to_owned(), "x".repeat(1000)] {
                let paths = capture_paths(Path::new("captures"), "host", source, &id).unwrap();
                assert_eq!(
                    paths.meta.file_name().unwrap(),
                    format!("{}.meta.json", crate::path_key::identity_component(&id)).as_str()
                );
            }
        }
        let child = "11111111-1111-4111-8111-111111111111/subagents/agent-abc";
        assert!(
            capture_paths(Path::new("captures"), "host", "claude", child)
                .unwrap()
                .meta
                .ends_with(format!("{child}.meta.json"))
        );
        assert_eq!(
            capture_paths(
                Path::new("captures"),
                "host",
                "chatgpt",
                "conversation~opening"
            )
            .unwrap()
            .meta,
            Path::new("captures/host/chatgpt/conversation.meta.json")
        );
    }
}
