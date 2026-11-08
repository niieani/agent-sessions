//! View filename components that every target filesystem and sync tool accepts.
//!
//! Views land in Obsidian vaults synced across macOS, Linux, Windows, and
//! mobile. A name is safe only when Windows accepts it (Obsidian Sync rejects
//! Windows-illegal names on every platform) and Obsidian wikilinks can address
//! it. Unsafe characters become their fullwidth lookalikes so names stay
//! readable; the mapping is idempotent, so already-safe names never change.

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, ensure};

/// Characters a component may not contain, with their fullwidth lookalikes.
/// `/` and `\` are separators; `: * ? " < > |` are illegal on Windows;
/// `# ^ [ ]` break Obsidian wikilinks and block references.
const LOOKALIKES: [(char, char); 13] = [
    ('/', '／'),
    ('\\', '＼'),
    (':', '：'),
    ('*', '＊'),
    ('?', '？'),
    ('"', '＂'),
    ('<', '＜'),
    ('>', '＞'),
    ('|', '｜'),
    ('#', '＃'),
    ('^', '＾'),
    ('[', '［'),
    (']', '］'),
];

/// Windows device names, reserved with or without an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A text fact interpolated into a layout: it can never introduce a
/// directory separator, a dot segment, a control byte, or an unsafe
/// character. Position-dependent rules (trailing dots and spaces, device
/// names) apply to the complete component in [`component`].
pub(crate) fn scalar(value: &str) -> String {
    let mapped = value.chars().map(lookalike).collect::<String>();
    if matches!(mapped.as_str(), "." | "..") {
        mapped.replace('.', "．")
    } else {
        mapped
    }
}

/// One complete path component: [`scalar`], then trailing spaces trimmed,
/// trailing dots made fullwidth, and Windows device names made fullwidth.
/// Returns an empty string for an all-space value; callers decide whether
/// that is an error or has a fallback.
pub(crate) fn component(value: &str) -> String {
    let mapped = scalar(value);
    let trimmed = mapped.trim_end_matches(' ');
    let kept = trimmed.trim_end_matches('.');
    let mut result = String::with_capacity(trimmed.len() + 2);
    result.push_str(kept);
    result.extend(std::iter::repeat_n('．', trimmed.len() - kept.len()));
    let base_end = result.find('.').unwrap_or(result.len());
    let base = result[..base_end].trim_end_matches(' ');
    if RESERVED.iter().any(|name| name.eq_ignore_ascii_case(base)) {
        let fullwidth = base.chars().map(fullwidth_ascii).collect::<String>();
        result.replace_range(..base.len(), &fullwidth);
    }
    result
}

/// A relative View path with every component made safe by [`component`].
pub(crate) fn path(path: &Path) -> Result<PathBuf> {
    path.components()
        .map(|part| match part {
            Component::Normal(name) => {
                let safe = component(&name.to_string_lossy());
                ensure!(
                    !safe.is_empty(),
                    "View path {} has a blank component",
                    path.display()
                );
                Ok(safe)
            }
            _ => anyhow::bail!("View path {} is not a plain relative path", path.display()),
        })
        .collect()
}

fn lookalike(character: char) -> char {
    if character.is_control() {
        return ' ';
    }
    LOOKALIKES
        .iter()
        .find(|(unsafe_character, _)| *unsafe_character == character)
        .map_or(character, |(_, safe)| *safe)
}

fn fullwidth_ascii(character: char) -> char {
    match character {
        '!'..='~' => char::from_u32(u32::from(character) + 0xFEE0).expect("fullwidth ASCII"),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_cannot_introduce_separators_control_bytes_or_dot_segments() {
        assert_eq!(scalar("../spec\\PRD\nreview\0"), "..／spec＼PRD review ");
        assert_eq!(scalar("."), "．");
        assert_eq!(scalar(".."), "．．");
        assert_eq!(scalar(".codex - Żółć C++"), ".codex - Żółć C++");
    }

    #[test]
    fn windows_illegal_and_wikilink_breaking_characters_become_fullwidth_lookalikes() {
        assert_eq!(
            scalar(r#"Refinement: Daily Log, Inbox Review, & Silent Sweep"#),
            "Refinement： Daily Log, Inbox Review, & Silent Sweep"
        );
        assert_eq!(
            scalar(r#"a*b?c"d<e>f|g#h^i[j]k"#),
            "a＊b？c＂d＜e＞f｜g＃h＾i［j］k"
        );
        assert_eq!(
            component("optimizing for #1.subagents"),
            "optimizing for ＃1.subagents"
        );
    }

    #[test]
    fn components_end_without_dots_or_spaces() {
        assert_eq!(component("Release v1."), "Release v1．");
        assert_eq!(component("Wait... "), "Wait．．．");
        assert_eq!(component("Trailing   "), "Trailing");
        assert_eq!(component("a . b . "), "a . b ．");
        assert_eq!(component(" leading"), " leading");
        assert_eq!(component("   "), "");
        assert_eq!(component("Name.md"), "Name.md");
    }

    #[test]
    fn windows_device_names_become_fullwidth_with_or_without_extension() {
        for (raw, safe) in [
            ("CON", "ＣＯＮ"),
            ("con", "ｃｏｎ"),
            ("Aux.md", "Ａｕｘ.md"),
            ("nul.tar.gz", "ｎｕｌ.tar.gz"),
            ("COM1", "ＣＯＭ１"),
            ("lpt9.json", "ｌｐｔ９.json"),
            ("PRN .md", "ＰＲＮ .md"),
        ] {
            assert_eq!(component(raw), safe, "{raw}");
        }
        for ordinary in [
            "CONSOLE.md",
            "COM10",
            "LPT0",
            "COM",
            "my CON.md",
            "CON (2).md",
        ] {
            assert_eq!(component(ordinary), ordinary);
        }
    }

    #[test]
    fn safe_names_are_fixed_points() {
        for raw in [
            "Vault - ChatGPT: Sync?",
            "CON.md",
            "v1. ",
            "..",
            "a/b\\c#d",
            "2026-10-26 Vault - x (branch 2).md",
        ] {
            let once = component(raw);
            assert_eq!(component(&once), once, "{raw}");
        }
    }

    #[test]
    fn paths_make_every_component_safe_and_reject_blank_components() {
        assert_eq!(
            path(Path::new("projects/a: b./CON/x?.md")).unwrap(),
            PathBuf::from("projects/a： b．/ＣＯＮ/x？.md")
        );
        assert!(path(Path::new("projects/  /x.md")).is_err());
        assert!(path(Path::new("../x.md")).is_err());
    }
}
