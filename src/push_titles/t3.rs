//! Per-Orchestrator Thread planning; provider Session identity stays unchanged.
use super::*;

pub(super) fn build_plan(
    archive: &Archive,
    db: &Db,
    host: &str,
    roots: &StoreRoots,
    titles: &indexmap::IndexMap<String, TitleConfig>,
) -> Result<Vec<PushTitlePlan>> {
    crate::pull::validate_store_root("t3", &roots.t3, roots.t3_explicit)?;
    let path = roots.t3.join(crate::adapters::t3::STORE_PATH);
    let store = path
        .is_file()
        .then(|| open_read_only(&path, "T3"))
        .transpose()?;
    if let Some(store) = &store {
        store.execute_batch("BEGIN")?;
        crate::adapters::t3::validate_title_schema(store)?;
    }
    let overwrite = titles.get("t3").is_some_and(|c| c.overwrite_external);
    let mut plans = Vec::new();
    for session in db.sessions_with_dialog()? {
        let digest_path =
            digest_dir(archive.root(), &session.source, &session.native_id)?.join("session.json");
        let Some(digest) = crate::digest::read_session(&digest_path)? else {
            continue;
        };
        for thread in db.augmentations_for_session(session.id)? {
            if thread.source != "t3" || !db.orchestrator_on_host("t3", &thread.thread_id, host)? {
                continue;
            }
            let native_id = thread.thread_id;
            let value = store
                .as_ref()
                .map(|store| current_title(store, &native_id))
                .transpose()?
                .flatten();
            let present = value.is_some();
            let current = value.flatten().filter(|title| !title.is_empty());
            let last_pushed = digest.pushed.get(&format!("t3:{native_id}")).cloned();
            let pending_path = digest_path.with_file_name("push-title.pending");
            let pending = read_pending(&pending_path)?
                .filter(|p| p.source == "t3" && p.native_id == native_id);
            let recovering = pending
                .as_ref()
                .is_some_and(|p| current.as_deref() == Some(&p.title));
            let disposition = if !present {
                PushTitleDisposition::Missing
            } else if current.as_deref() == Some(&digest.title) && current == last_pushed {
                PushTitleDisposition::Unchanged
            } else if recovering || overwrite || current.is_none() || current == last_pushed {
                PushTitleDisposition::Push
            } else {
                PushTitleDisposition::Keep
            };
            plans.push(PushTitlePlan {
                source: "t3".into(),
                native_id,
                current,
                title: digest.title.clone(),
                disposition,
                native_path: path.clone(),
                last_pushed,
                digest_path: digest_path.clone(),
                digest: digest.clone(),
                pending_path,
                pending,
            });
        }
    }
    Ok(plans)
}

fn current_title(store: &Connection, id: &str) -> Result<Option<Option<String>>> {
    Ok(store
        .query_row(
            "SELECT title FROM orchestration_v2_projection_threads WHERE thread_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn apply(roots: &StoreRoots, plan: &PushTitlePlan) -> Result<()> {
    let store = open_read_only(&plan.native_path, "T3")?;
    crate::adapters::t3::validate_title_schema(&store)?;
    let current = current_title(&store, &plan.native_id)?
        .context("T3 Orchestrator Thread disappeared after planning")?
        .filter(|s| !s.is_empty());
    anyhow::ensure!(
        current == plan.current,
        "T3 title changed after planning for {}",
        plan.native_id
    );
    if current.as_deref() != Some(&plan.title) {
        super::t3_transport::dispatch(&roots.t3, &plan.native_id, &plan.title)
            .with_context(|| format!("T3 Orchestrator Thread {}", plan.native_id))?;
    }
    Ok(())
}
