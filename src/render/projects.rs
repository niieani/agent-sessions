//! Repository and recorded-path Project identity, independent of Kind matching.
use crate::archive::db::SessionRow;
use crate::repository_key::repository_key;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Identity {
    Native(String),
    Repository(String),
    Path(String),
}
impl Identity {
    pub(super) fn for_session(session: &SessionRow) -> Option<Self> {
        session
            .native_project
            .as_ref()
            .map(|project| Self::Native(project.key()))
            .or_else(|| {
                session
                    .transcript_facts
                    .git
                    .as_ref()
                    .and_then(|git| git.repository_url.as_deref())
                    .and_then(repository_key)
                    .map(Self::Repository)
                    .or_else(|| {
                        session
                            .repository_resolution
                            .as_ref()
                            .and_then(|fact| match &fact.lookup {
                                crate::repository_resolution::Lookup::Resolved(url) => {
                                    repository_key(url).map(Self::Repository)
                                }
                                _ => None,
                            })
                    })
                    .or_else(|| {
                        (!session
                            .repository_resolution
                            .as_ref()
                            .is_some_and(|f| f.unresolved))
                        .then(|| session.project_path.clone().map(Self::Path))
                        .flatten()
                    })
            })
    }
    fn key(&self) -> &str {
        match self {
            Self::Native(key) | Self::Repository(key) | Self::Path(key) => key,
        }
    }
}

pub(super) struct ProjectName {
    pub(super) directory: String,
    pub(super) display: String,
}

/// Names are resolved per Session because an exact path mapping takes precedence
/// even when several recorded paths have the same repository identity.
///
/// Every directory name is a filesystem-safe component (`filename::component`);
/// the display name keeps the raw text.
///
/// Project names become directory names on filesystems that may ignore case
/// (APFS by default), so derived names are reconciled case-insensitively after
/// the raw-name collision rule: a path-only name adopts the single repository
/// spelling it matches ignoring case, and any other case-only variants among
/// derived names get hash suffixes. Explicit names — Name Table values and the
/// `reserved` Project Assignment and Kind group directories — are never
/// respelled for case; two explicit spellings that differ only by case fail manifest
/// validation.
pub(super) fn names(
    owners: &BTreeMap<i64, &SessionRow>,
    table: &indexmap::IndexMap<String, String>,
    reserved: &BTreeSet<String>,
) -> BTreeMap<i64, ProjectName> {
    let identities = owners
        .iter()
        .filter_map(|(id, owner)| {
            Identity::for_session(owner)
                .or_else(|| {
                    owner
                        .project_path
                        .as_ref()
                        .filter(|path| table.contains_key(*path))
                        .cloned()
                        .map(Identity::Path)
                })
                .map(|key| (*id, key))
        })
        .collect::<BTreeMap<_, _>>();
    let explicit = identities
        .iter()
        .filter_map(|(id, identity)| {
            owners[id]
                .project_path
                .as_ref()
                .and_then(|path| table.get(path))
                .or_else(|| match identity {
                    Identity::Native(key) | Identity::Repository(key) => table.get(key),
                    Identity::Path(_) => None,
                })
                .map(|name| (*id, name))
        })
        .collect::<BTreeMap<_, _>>();
    // Repository evidence is optional per Session: do not rename path-only
    // Projects merely because another Session records the same repository.
    // Explicitly named Sessions never render their raw name, so they do not
    // force a hash suffix onto the raw names that remain.
    let mut collisions: BTreeMap<(bool, String), BTreeSet<&Identity>> = BTreeMap::new();
    for (_, identity) in identities
        .iter()
        .filter(|(id, _)| !explicit.contains_key(id))
    {
        collisions
            .entry((
                matches!(identity, Identity::Repository(_)),
                super::project_basename(identity.key()),
            ))
            .or_default()
            .insert(identity);
    }
    let resolved = identities
        .iter()
        .map(|(id, identity)| {
            let name = match explicit.get(id) {
                Some(name) => Name::Explicit(directory_name(name)),
                None => {
                    if let Identity::Native(_) = identity {
                        let project = owners[id]
                            .native_project
                            .as_ref()
                            .expect("native Project identity");
                        let name = project.name.as_deref().unwrap_or(&project.native_id);
                        return (*id, Name::Derived(directory_name(name), identity));
                    }
                    let basename = super::project_basename(identity.key());
                    let collides = collisions[&(
                        matches!(identity, Identity::Repository(_)),
                        basename.clone(),
                    )]
                        .len()
                        > 1;
                    if collides {
                        Name::Hashed(hashed(identity))
                    } else {
                        Name::Derived(directory_name(&basename), identity)
                    }
                }
            };
            (*id, name)
        })
        .collect::<BTreeMap<_, _>>();
    let canonical = case_canonical_spellings(resolved.values(), reserved);
    resolved
        .into_iter()
        .map(|(id, name)| {
            let name = match name {
                Name::Explicit(name) | Name::Hashed(name) => name,
                Name::Derived(name, identity) => match &canonical[&filesystem_key(&name)] {
                    Canonical::Spelling(spelling) => spelling.clone(),
                    Canonical::Ambiguous => {
                        format!("{}@{}", name, super::short_path_hash(identity.key()))
                    }
                },
            };
            let display = if let Some(explicit) = explicit.get(&id) {
                (*explicit).clone()
            } else {
                owners[&id]
                    .native_project
                    .as_ref()
                    .and_then(|project| project.name.clone())
                    .unwrap_or_else(|| name.clone())
            };
            (
                id,
                ProjectName {
                    directory: name,
                    display,
                },
            )
        })
        .collect()
}

/// The Project directory for a raw name; display names keep the raw text.
pub(super) fn directory_name(value: &str) -> String {
    match super::filename::component(value.trim()) {
        name if name.is_empty() => "Project".into(),
        name => name,
    }
}

enum Name<'a> {
    Explicit(String),
    Hashed(String),
    Derived(String, &'a Identity),
}

enum Canonical {
    Spelling(String),
    Ambiguous,
}

fn hashed(identity: &Identity) -> String {
    format!(
        "{}@{}",
        directory_name(&super::project_basename(identity.key())),
        super::short_path_hash(identity.key())
    )
}

fn filesystem_key(name: &str) -> String {
    crate::path_key::filesystem_key(name)
}

/// For every case-folded derived name: the one spelling all its derived
/// identities share, or `Ambiguous` when they cannot share one directory.
fn case_canonical_spellings<'a>(
    names: impl Iterator<Item = &'a Name<'a>> + Clone,
    reserved: &'a BTreeSet<String>,
) -> BTreeMap<String, Canonical> {
    let mut explicit: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for spelling in reserved {
        explicit
            .entry(filesystem_key(spelling))
            .or_default()
            .insert(spelling);
    }
    for name in names.clone() {
        if let Name::Explicit(spelling) = name {
            explicit
                .entry(filesystem_key(spelling))
                .or_default()
                .insert(spelling);
        }
    }
    let mut native: BTreeMap<String, BTreeSet<&Identity>> = BTreeMap::new();
    let mut groups: BTreeMap<String, (BTreeSet<&str>, BTreeSet<&str>)> = BTreeMap::new();
    for name in names {
        if let Name::Derived(spelling, identity) = name {
            if matches!(identity, Identity::Native(_)) {
                native
                    .entry(filesystem_key(spelling))
                    .or_default()
                    .insert(identity);
            }
            let (repositories, paths) = groups.entry(filesystem_key(spelling)).or_default();
            match identity {
                Identity::Repository(_) => repositories.insert(spelling),
                Identity::Native(_) | Identity::Path(_) => paths.insert(spelling),
            };
        }
    }
    groups
        .into_iter()
        .map(|(key, (repositories, paths))| {
            let spellings = repositories.union(&paths).collect::<BTreeSet<_>>();
            let canonical = if native.get(&key).is_some_and(|ids| ids.len() > 1) {
                Canonical::Ambiguous
            } else if let Some(owners) = explicit.get(&key) {
                // An explicit name owns this directory; a derived name joins
                // it only when spelled exactly the same.
                if owners.len() == 1 && spellings.iter().all(|spelling| owners.contains(**spelling))
                {
                    Canonical::Spelling((*owners.first().expect("one explicit name")).to_string())
                } else {
                    Canonical::Ambiguous
                }
            } else if spellings.len() == 1 {
                Canonical::Spelling((*spellings.first().expect("nonempty group")).to_string())
            } else if repositories.len() == 1 {
                Canonical::Spelling((*repositories.first().expect("one repository")).to_string())
            } else {
                Canonical::Ambiguous
            };
            (key, canonical)
        })
        .collect()
}
