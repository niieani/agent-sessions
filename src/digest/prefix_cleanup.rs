//! Explicit exact-prefix maintenance; normal Render never changes durable titles.
use super::repair::{Publication, RepairOptions, RepairStats, publish};
use super::*;

pub(super) fn strip_project_prefix(title: &str, project: &str) -> String {
    let title_trimmed = title.trim_start();
    let count = project.chars().count();
    if count == 0 {
        return title.into();
    }
    let boundary = title_trimmed
        .char_indices()
        .nth(count)
        .map(|(i, _)| i)
        .unwrap_or(title_trimmed.len());
    let (prefix, remainder) = title_trimmed.split_at(boundary);
    if prefix.to_lowercase() != project.to_lowercase()
        || remainder.chars().next().is_some_and(char::is_alphanumeric)
    {
        return title.into();
    }
    let remainder = remainder.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '-' | '–' | '—' | ':' | '|')
    });
    if remainder.is_empty() {
        title.into()
    } else {
        remainder.into()
    }
}

pub(super) fn run(archive: &Archive, options: &RepairOptions) -> Result<RepairStats> {
    let mut stats = RepairStats::default();
    let projects = crate::render::resolved_projects(archive)?;
    let db = Db::open_read_only(&archive.db_path())?;
    let cache = files::cache_rows(archive.root())?;
    for row in db.sessions_with_dialog()? {
        let Some(Some(project)) = projects.get(&row.id) else {
            continue;
        };
        let dir = files::digest_dir(archive.root(), &row.source, &row.native_id)?;
        let Some(mut session) = files::read_session(&dir.join("session.json"))? else {
            continue;
        };
        let mut days = BTreeMap::new();
        let mut changed = false;
        for entry in cache.iter().filter(|e| {
            e.source == row.source && e.native_id == row.native_id && !e.workday.is_empty()
        }) {
            let mut day = files::read_day(&archive.root().join(&entry.path))?
                .context("missing cached Digest")?;
            let original = day.clone();
            day.title = strip_project_prefix(&day.title, project);
            day.locked_title = strip_project_prefix(&day.locked_title, project);
            day.topic = day.topic.map(|topic| strip_project_prefix(&topic, project));
            changed |= day != original;
            days.insert(
                archive
                    .root()
                    .join(&entry.path)
                    .strip_prefix(&dir)?
                    .to_path_buf(),
                day,
            );
        }
        let old = session.clone();
        session.topic = strip_project_prefix(&session.topic, project);
        session.title = strip_project_prefix(&session.title, project);
        changed |= old != session;
        if !changed {
            stats.skipped += 1;
            continue;
        }
        anyhow::ensure!(!days.is_empty(), "Session Digest has no Day Digests");
        stats.selected += 1;
        if !options.dry_run {
            publish(
                archive,
                &dir,
                &Publication {
                    schema_version: 2,
                    fingerprint: "project-prefix-cleanup-v1".into(),
                    session,
                    days,
                },
            )?;
            stats.repaired += 1;
        }
    }
    if !options.dry_run {
        refresh_cache(archive)?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::strip_project_prefix as clean;
    #[test]
    fn exact_prefix_cleanup_preserves_word_boundaries_and_meaningful_punctuation() {
        assert_eq!(
            clean(
                "Example Assistant Service Config - Fixed reasoningEffort max rejection",
                "Example Assistant Service"
            ),
            "Config - Fixed reasoningEffort max rejection"
        );
        assert_eq!(
            clean(
                "example assistant SERVICE — Config",
                "Example Assistant Service"
            ),
            "Config"
        );
        assert_eq!(
            clean(
                "Example Assistant Services Config",
                "Example Assistant Service"
            ),
            "Example Assistant Services Config"
        );
        assert_eq!(
            clean("Example Assistant Service - ", "Example Assistant Service"),
            "Example Assistant Service - "
        );
        assert_eq!(clean("C++ - .NET bridge", "C++"), ".NET bridge");
        assert_eq!(clean("C++Config", "C++"), "C++Config");
        assert_eq!(
            clean(
                "Config for Example Assistant Service",
                "Example Assistant Service"
            ),
            "Config for Example Assistant Service"
        );
        assert_eq!(clean("ŻÓŁĆ: [draft]", "żółć"), "[draft]");
    }
}
