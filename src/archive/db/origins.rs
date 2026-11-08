use super::*;
use crate::origins::{Attributions, ClaimKind, HostAttribution, OriginClaim};
use std::collections::BTreeMap;

pub type OriginSession = (String, String, String, Vec<String>);
impl Db {
    pub fn origin_hosts(&self, source: &str, native_id: &str) -> Result<Vec<String>> {
        Ok(self.conn.prepare("SELECT h.host FROM session_hosts h JOIN sessions s ON s.id=h.session_id WHERE s.source=?1 AND s.native_id=?2 ORDER BY h.host")?
            .query_map(params![source, native_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn origin_sessions(&self) -> Result<Vec<OriginSession>> {
        let rows = self
            .conn
            .prepare("SELECT source,native_id,winner_host FROM sessions ORDER BY source,native_id")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(s, n, w)| {
                let hosts = self.origin_hosts(&s, &n)?;
                Ok((s, n, w, hosts))
            })
            .collect()
    }
    pub fn origin_claims(&self) -> Result<BTreeMap<(String, String), Vec<OriginClaim>>> {
        // All Host projections, never the elected one alone. An ambiguous
        // Thread binding is not proof of execution of either Session.
        let rows = self.conn.prepare("SELECT DISTINCT f.provider_source,f.provider_native_id,f.host,f.native_id
            FROM orchestrator_projection_link_facts f
            WHERE f.source='t3' AND f.status='candidate' AND f.provider_source IS NOT NULL AND f.provider_native_id IS NOT NULL
              AND 1=(SELECT count(DISTINCT g.provider_source || ':' || g.provider_native_id)
                     FROM orchestrator_projection_link_facts g WHERE g.source=f.source AND g.native_id=f.native_id AND g.host=f.host AND g.status='candidate')
            ORDER BY f.provider_source,f.provider_native_id,f.host,f.native_id")?
            .query_map([], |r| Ok(OriginClaim { source:r.get(0)?, native_id:r.get(1)?, host:r.get(2)?, kind:ClaimKind::T3Execution, reference:format!("t3:{}", r.get::<_, String>(3)?) }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut evidence: BTreeMap<(String, String), Vec<OriginClaim>> = BTreeMap::new();
        for claim in rows {
            evidence
                .entry((claim.source.clone(), claim.native_id.clone()))
                .or_default()
                .push(claim);
        }
        // Claude child identity includes its exact parent Session; no cwd guessing.
        for (source, native_id, _, _) in self.origin_sessions()? {
            if source == "claude"
                && let Some((parent, _)) = native_id.split_once("/subagents/")
            {
                let claims = evidence
                    .get(&(source.clone(), parent.to_owned()))
                    .cloned()
                    .unwrap_or_default();
                for mut claim in claims {
                    claim.native_id = native_id.clone();
                    evidence
                        .entry((source.clone(), native_id.clone()))
                        .or_default()
                        .push(claim);
                }
            }
        }
        Ok(evidence)
    }
    pub fn replace_host_attributions(&mut self, records: &Attributions) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM session_origins", [])?;
        for record in records.values() {
            tx.execute(
                "INSERT INTO session_origins(source,native_id,record_json) VALUES (?1,?2,?3)",
                params![
                    record.source,
                    record.native_id,
                    serde_json::to_string(record)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn origin_start_times(&self) -> Result<BTreeMap<(String, String), Option<i64>>> {
        self.conn
            .prepare("SELECT source,native_id,created_at_ms FROM sessions")?
            .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()
            .context("reading Session start times")
    }
    pub fn previous_origin_host(&self, source: &str, native_id: &str) -> Result<Option<String>> {
        self.conn.query_row(
            "SELECT json_extract(record_json, '$.host') FROM session_origins WHERE source=?1 AND native_id=?2",
            params![source,native_id], |r| r.get(0),
        ).optional().context("reading previous origin Host")
    }
    /// Logging needs only the last decision, never obsolete evidence schemas.
    pub fn previous_origin_decision(
        &self,
        source: &str,
        native_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        self.conn.query_row(
            "SELECT json_extract(record_json, '$.host'), json_extract(record_json, '$.status') FROM session_origins WHERE source=?1 AND native_id=?2",
            params![source, native_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional().context("reading previous origin decision")
    }
    pub(crate) fn origin_decisions(&self) -> Result<crate::origins::Decisions> {
        self.conn.prepare("SELECT source,native_id,json_extract(record_json, '$.host'),json_extract(record_json, '$.status') FROM session_origins")?
            .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))))?
            .collect::<rusqlite::Result<_>>().context("reading origin decisions")
    }
    pub fn host_attribution(
        &self,
        source: &str,
        native_id: &str,
    ) -> Result<Option<HostAttribution>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT record_json FROM session_origins WHERE source=?1 AND native_id=?2",
                params![source, native_id],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|raw| serde_json::from_str(&raw).context("decoding host attribution"))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{conversation, orchestrator};
    use super::*;
    use crate::{
        archive::Archive,
        origins::{self, AttributionStatus},
    };

    fn archive() -> (tempfile::TempDir, Archive) {
        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::open(dir.path()).unwrap();
        (dir, archive)
    }
    fn session(db: &mut Db, id: &str, host: &str, rich: bool) {
        let mut c = conversation(id);
        c.provenance.host = host.into();
        c.messages.push(Message {
            role: Role::User,
            kind: MessageKind::Dialog,
            text: if rich { "more dialog" } else { "dialog" }.into(),
            timestamp_ms: Some(1),
            actions_before: 0,
        });
        db.upsert_conversation(&c).unwrap();
    }
    #[test]
    fn exact_t3_execution_beats_richest_copy_in_either_ingest_order_and_children_inherit() {
        for reverse in [false, true] {
            let (_dir, a) = archive();
            let mut db = a.db().unwrap();
            let hosts = if reverse {
                ["laptop", "studio"]
            } else {
                ["studio", "laptop"]
            };
            for host in hosts {
                session(&mut db, "provider-1", host, host == "laptop");
                session(
                    &mut db,
                    "provider-1/subagents/agent-child",
                    host,
                    host == "laptop",
                );
            }
            db.upsert_orchestrator_projection(&orchestrator("thread", "studio", 1))
                .unwrap();
            origins::reconcile(&a, &mut db).unwrap();
            for id in ["provider-1", "provider-1/subagents/agent-child"] {
                let r = db.host_attribution("claude", id).unwrap().unwrap();
                assert_eq!(r.host, "studio");
                assert_eq!(r.status, AttributionStatus::Confirmed);
            }
            assert_eq!(
                db.sessions_with_dialog().unwrap()[0].winner_host.as_deref(),
                Some("laptop")
            );
        }
    }
    #[test]
    fn ambiguous_execution_keeps_previous_host_even_when_projection_winner_changes() {
        let (_dir, a) = archive();
        let mut db = a.db().unwrap();
        session(&mut db, "provider-1", "studio", false);
        origins::reconcile(&a, &mut db).unwrap();
        session(&mut db, "provider-1", "laptop", true);
        for host in ["studio", "laptop"] {
            db.upsert_orchestrator_projection(&orchestrator("thread", host, 1))
                .unwrap();
        }
        origins::reconcile(&a, &mut db).unwrap();
        let r = db
            .host_attribution("claude", "provider-1")
            .unwrap()
            .unwrap();
        assert_eq!(r.host, "studio");
        assert_eq!(r.status, AttributionStatus::Ambiguous);
        origins::reconcile(&a, &mut db).unwrap();
        assert_eq!(
            r,
            db.host_attribution("claude", "provider-1")
                .unwrap()
                .unwrap()
        );
    }
    #[test]
    fn t3_claims_are_rederived_and_never_saved_as_durable_evidence() {
        let (_dir, a) = archive();
        let mut db = a.db().unwrap();
        session(&mut db, "provider-1", "studio", false);
        session(&mut db, "provider-1", "laptop", true);
        db.upsert_orchestrator_projection(&orchestrator("thread", "studio", 1))
            .unwrap();
        origins::reconcile(&a, &mut db).unwrap();
        assert_eq!(
            db.host_attribution("claude", "provider-1")
                .unwrap()
                .unwrap()
                .status,
            AttributionStatus::Confirmed
        );
        let mut changed = orchestrator("thread", "studio", 2);
        changed.link_facts[0].provider_native_id = Some("other-provider".into());
        db.upsert_orchestrator_projection(&changed).unwrap();
        origins::reconcile(&a, &mut db).unwrap();
        let r = db
            .host_attribution("claude", "provider-1")
            .unwrap()
            .unwrap();
        assert_eq!(r.host, "studio");
        assert_eq!(r.status, AttributionStatus::Ambiguous);
        assert!(r.claims.is_empty());
        let preferences = origins::load(a.root()).unwrap();
        let preference =
            serde_json::to_string(&preferences[&("claude".into(), "provider-1".into())]).unwrap();
        assert!(!preference.contains("claims"));
        assert!(!preference.contains("t3"));
    }
    #[test]
    fn preserving_current_host_does_not_read_the_discarded_evidence_report() {
        let (_dir, a) = archive();
        let mut db = a.db().unwrap();
        session(&mut db, "provider-1", "studio", false);
        session(&mut db, "provider-1", "laptop", true);
        // The last choice is sufficient; report fields are regenerable.
        db.conn.execute("INSERT INTO session_origins(source,native_id,record_json) VALUES ('claude','provider-1',?1)",
            [r#"{"host":"studio","claims":["discarded report schema"]}"#]).unwrap();
        origins::preserve_current(&a, &db).unwrap();
        let preferences = origins::load(a.root()).unwrap();
        assert_eq!(
            preferences[&("claude".into(), "provider-1".into())].host,
            "studio"
        );
        origins::reconcile(&a, &mut db).unwrap();
        let r = db
            .host_attribution("claude", "provider-1")
            .unwrap()
            .unwrap();
        assert_eq!(r.host, "studio");
        assert_eq!(r.status, AttributionStatus::Ambiguous);
        assert!(r.claims.is_empty());
    }
}
