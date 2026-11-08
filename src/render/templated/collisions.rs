//! Collision-safe View path naming.
//!
//! Two kinds of collision are resolved differently. Candidates spelled exactly
//! alike take ` (2)`, ` (3)` suffixes in planning order (Session start), so an
//! earlier file never changes because a later one appeared. Candidates whose
//! file names differ only by case would be one file on a case-insensitive
//! filesystem; like Project directories, every such variant takes its
//! Session's identity hash (`<stem>@<short-id>`), so which file is renamed
//! never depends on order and names without a case variant stay unchanged.
//! Manifest validation still rejects any case collision this does not resolve.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::path_key::filesystem_key;

/// Keys of candidates whose file name another candidate in the same directory
/// spells differently but that case-folds to the same name. Candidates are
/// NFC already, so the difference is letter case.
pub(super) fn case_variants<'a, K: Ord + Clone + 'a>(
    candidates: impl IntoIterator<Item = (&'a K, &'a PathBuf)>,
) -> BTreeSet<K> {
    let mut groups: BTreeMap<(&Path, String), Vec<(&K, &PathBuf)>> = BTreeMap::new();
    for (key, path) in candidates {
        let Some(name) = path.file_name() else {
            continue;
        };
        groups
            .entry((
                path.parent().unwrap_or_else(|| Path::new("")),
                filesystem_key(&name.to_string_lossy()),
            ))
            .or_default()
            .push((key, path));
    }
    groups
        .into_values()
        .filter(|members| {
            members
                .iter()
                .map(|(_, path)| path)
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .flatten()
        .map(|(key, _)| key.clone())
        .collect()
}

/// `<stem>@<identity>.<extension>`, the Project-directory collision spelling.
pub(super) fn with_identity_suffix(path: &Path, identity: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    path.with_file_name(match path.extension().and_then(|ext| ext.to_str()) {
        Some(extension) => format!("{stem}@{identity}.{extension}"),
        None => format!("{stem}@{identity}"),
    })
}

/// Hands out exact-spelling-unique paths in the order candidates arrive.
#[derive(Default)]
pub(super) struct Allocator(BTreeSet<PathBuf>);

impl Allocator {
    pub(super) fn allocate(&mut self, candidate: &Path) -> PathBuf {
        let mut path = candidate.to_path_buf();
        let mut number = 2;
        while !self.0.insert(path.clone()) {
            path = with_collision_suffix(candidate, number);
            number += 1;
        }
        path
    }
}

fn with_collision_suffix(path: &Path, number: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    path.with_file_name(match path.extension().and_then(|ext| ext.to_str()) {
        Some(extension) => format!("{stem} ({number}).{extension}"),
        None => format!("{stem} ({number})"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variants(paths: &[&str]) -> BTreeSet<usize> {
        let paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        let keys = (0..paths.len()).collect::<Vec<_>>();
        case_variants(keys.iter().zip(&paths))
    }

    #[test]
    fn only_differently_spelled_names_in_one_directory_are_case_variants() {
        assert_eq!(
            variants(&["a/Foo.md", "a/foo.md", "a/bar.md"]),
            [0, 1].into()
        );
        assert_eq!(variants(&["a/Foo.md", "a/Foo.md"]), BTreeSet::new());
        assert_eq!(variants(&["a/Foo.md", "b/foo.md"]), BTreeSet::new());
        assert_eq!(
            variants(&["a/Foo.md", "a/Foo.md", "a/FOO.md"]),
            [0, 1, 2].into()
        );
    }

    #[test]
    fn exact_duplicates_are_numbered_in_arrival_order() {
        let mut allocator = Allocator::default();
        let paths = ["x/Same.md", "x/Same.md", "x/Same (2).md", "x/plain"]
            .map(|path| allocator.allocate(Path::new(path)));
        assert_eq!(
            paths,
            ["x/Same.md", "x/Same (2).md", "x/Same (2) (2).md", "x/plain"].map(PathBuf::from)
        );
        assert_eq!(
            with_identity_suffix(Path::new("x/Same.md"), "0a1b2c3d"),
            PathBuf::from("x/Same@0a1b2c3d.md")
        );
    }
}
