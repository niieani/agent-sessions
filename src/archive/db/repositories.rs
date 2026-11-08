//! Rebuildable repository evidence; no native Store reads.
use super::*;
use crate::repository_resolution::{Fact, Lookup, Resolver, T3Evidence};

pub(crate) struct RepositoryCandidate {
    pub source: String,
    pub native_id: String,
    pub host: String,
    pub path: String,
    pub branch_recorded: bool,
    pub revision: (i64, u64),
    pub fact: Option<Fact>,
}
impl Db {
    pub(crate) fn repository_candidates(
        &self,
        host: Option<&str>,
    ) -> Result<Vec<RepositoryCandidate>> {
        let mut stmt = self.conn.prepare("SELECT p.source,p.native_id,p.host,p.project_path,p.git_branch,r.source_mtime_ms,r.source_size,f.fact
            FROM session_projections p JOIN session_revisions r USING(source,native_id,host)
            LEFT JOIN repository_resolutions f USING(source,native_id,host)
            WHERE (?1 IS NULL OR p.host=?1) AND p.project_path IS NOT NULL AND p.git_repository_url IS NULL
            AND NOT EXISTS(SELECT 1 FROM conversation_native_projects n WHERE n.source=p.source AND n.native_id=p.native_id AND n.host=p.host)
            ORDER BY p.source,p.native_id,p.host")?;
        stmt.query_map([host], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, u64>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?
        .map(|row| {
            let (source, native_id, host, path, branch, mtime, size, fact) = row?;
            Ok(RepositoryCandidate {
                source,
                native_id,
                host,
                path,
                branch_recorded: branch.is_some(),
                revision: (mtime, size),
                fact: fact.map(|s| serde_json::from_str(&s)).transpose()?,
            })
        })
        .collect()
    }
    pub(crate) fn repository_sticky(
        &self,
        host: Option<&str>,
    ) -> Result<BTreeMap<(String, String), Lookup>> {
        let mut stmt = self.conn.prepare("SELECT host,fact FROM repository_resolutions WHERE ?1 IS NULL OR host=?1 ORDER BY source,native_id")?;
        let mut result = BTreeMap::new();
        for row in stmt.query_map([host], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (host, fact) = row?;
            let fact: Fact = serde_json::from_str(&fact)?;
            if matches!(fact.lookup, Lookup::Resolved(_)) {
                result.insert((host, fact.path), fact.lookup);
            }
        }
        Ok(result)
    }
    pub(crate) fn index_t3_repository(&self, p: &OrchestratorProjection) -> Result<()> {
        let evidence = T3Evidence {
            repository_url: p.repository_url.clone(),
            workspace_root: p.workspace_root.clone(),
            links: p.link_facts.clone(),
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO t3_repository_evidence VALUES (?1,?2,?3,?4)",
            params![
                p.source,
                p.native_id,
                p.host,
                serde_json::to_string(&evidence)?
            ],
        )?;
        Ok(())
    }
    pub(crate) fn prepare_repository_t3(
        &self,
        archive: &crate::archive::Archive,
        host: Option<&str>,
        resolver: &dyn Resolver,
        failures: &mut usize,
    ) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT p.source,p.native_id,p.host FROM orchestrator_thread_projections p
            LEFT JOIN t3_repository_evidence e USING(source,native_id,host)
            WHERE (?1 IS NULL OR p.host=?1) AND e.evidence IS NULL",
        )?;
        let rows = stmt
            .query_map([host], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (source, id, host) in rows {
            let path = match crate::adapters::capture_paths(
                &archive.captures_dir(),
                &host,
                &source,
                &id,
            ) {
                Ok(paths) => paths.meta,
                Err(error) => {
                    tracing::warn!(%error,host,source,id,"invalid repository Capture path");
                    *failures += 1;
                    continue;
                }
            };
            let loaded = (|| -> Result<_> {
                let meta = serde_json::from_reader(std::fs::File::open(&path)?)?;
                let capture = crate::adapters::Capture {
                    meta,
                    meta_path: path.clone(),
                    body_path: crate::archive::body_for_sidecar(&path)?,
                    written: false,
                    outcome: crate::adapters::CaptureOutcome::Unchanged,
                };
                resolver.extracted();
                crate::adapters::t3::extract_projection(&capture)
            })();
            match loaded {
                Ok(p) => self.index_t3_repository(&p)?,
                Err(error) => {
                    crate::repository_resolution::warn_bad_unit(&error, &path)?;
                    *failures += 1;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn repository_workspace_candidates(
        &self,
        host: Option<&str>,
    ) -> Result<Vec<RepositoryCandidate>> {
        let mut stmt=self.conn.prepare("SELECT p.source,p.native_id,p.host,p.logical_revision_ms,p.discriminator,e.evidence,f.fact
            FROM orchestrator_thread_projections p JOIN t3_repository_evidence e USING(source,native_id,host)
            LEFT JOIN repository_resolutions f USING(source,native_id,host) WHERE ?1 IS NULL OR p.host=?1")?;
        let mut result = Vec::new();
        for row in stmt.query_map([host], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })? {
            let (source, native_id, host, mtime, size, evidence, fact) = row?;
            let evidence: T3Evidence = serde_json::from_str(&evidence)?;
            if evidence.repository_url.is_some() {
                continue;
            }
            if let Some(path) = evidence.workspace_root {
                result.push(RepositoryCandidate {
                    source,
                    native_id,
                    host,
                    path,
                    branch_recorded: false,
                    revision: (mtime, size),
                    fact: fact.map(|s| serde_json::from_str(&s)).transpose()?,
                });
            }
        }
        Ok(result)
    }
    pub(crate) fn repository_t3_fallbacks(&self) -> Result<BTreeMap<(String, String), String>> {
        let mut stmt=self.conn.prepare("SELECT e.evidence,f.fact FROM t3_repository_evidence e
            JOIN orchestrator_threads t ON t.source=e.source AND t.native_id=e.native_id AND t.winner_host=e.host
            LEFT JOIN repository_resolutions f ON f.source=e.source AND f.native_id=e.native_id AND f.host=e.host")?;
        let mut answers: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        for row in stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            let (evidence, fact) = row?;
            let evidence: T3Evidence = serde_json::from_str(&evidence)?;
            let root = fact
                .map(|s| serde_json::from_str::<Fact>(&s))
                .transpose()?
                .and_then(|f| match f.lookup {
                    Lookup::Resolved(url) => crate::repository_key::repository_key(&url),
                    _ => None,
                });
            let Some(url) = evidence.repository_url.or(root) else {
                continue;
            };
            let links = evidence
                .links
                .into_iter()
                .filter(|f| f.status == crate::model::OrchestratorLinkStatus::Candidate)
                .filter_map(|f| Some((f.provider_source?, f.provider_native_id?)))
                .collect::<BTreeSet<_>>();
            if links.len() == 1 {
                answers
                    .entry(links.into_iter().next().unwrap())
                    .or_default()
                    .insert(url);
            }
        }
        Ok(answers
            .into_iter()
            .filter_map(|(key, urls)| {
                (urls.len() == 1).then(|| (key, urls.into_iter().next().unwrap()))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bulk_repository_facts_keep_host_qualified_identity_and_support_old_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("archive.db")).unwrap();
        let fact = Fact {
            source_revision: (1, 2),
            branch_recorded: true,
            path: "/work".into(),
            lookup: Lookup::Missing,
            unresolved: true,
        };
        for host in ["a", "b"] {
            db.conn
                .execute(
                    "INSERT INTO repository_resolutions VALUES (?1,'claude','session',?2)",
                    params![host, serde_json::to_string(&fact).unwrap()],
                )
                .unwrap();
        }
        let facts = db.repository_facts().unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(
            facts[&("a".into(), "claude".into(), "session".into())],
            fact
        );
        db.conn
            .execute("DROP TABLE repository_resolutions", [])
            .unwrap();
        assert!(db.repository_facts().unwrap().is_empty());
    }
}
