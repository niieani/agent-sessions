//! Host Pull Health, durably published before updating the rebuildable index.
use super::Db;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS host_pulls (host TEXT PRIMARY KEY,tracked_since_ms INTEGER NOT NULL,last_success_ms INTEGER);";

/// One Host's Pull health record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostPullRecord {
    /// When this Archive first attempted to Pull the Host.
    pub tracked_since_ms: i64,
    /// End of the Host's latest Pull in which every Source succeeded.
    pub last_success_ms: Option<i64>,
}

impl Db {
    /// Start tracking `host` at `at_ms` unless it is already tracked.
    pub fn track_host(&self, host: &str, at_ms: i64) -> Result<()> {
        self.update_host_pull_health(|hosts| {
            hosts.entry(host.to_owned()).or_insert(HostPullRecord {
                tracked_since_ms: at_ms,
                last_success_ms: None,
            });
        })
    }

    /// Record a successful Pull of `host` at `at_ms`. Monotonic.
    pub fn record_host_pull_success(&self, host: &str, at_ms: i64) -> Result<()> {
        self.update_host_pull_health(|hosts| {
            let record = hosts.entry(host.to_owned()).or_insert(HostPullRecord {
                tracked_since_ms: at_ms,
                last_success_ms: None,
            });
            record.last_success_ms = Some(record.last_success_ms.unwrap_or(at_ms).max(at_ms));
        })
    }

    /// Validate durable health before Rebuild discards the index. Only a legacy
    /// Archive without a health file attempts to recover its old database.
    pub(crate) fn preserve_host_pull_health(db_path: &Path) -> Result<()> {
        let root = db_path.parent().context("archive database has no parent")?;
        load(root)?;
        if !root.join("host-pulls.json").try_exists()? && db_path.try_exists()? {
            match Self::open(db_path) {
                Ok(db) => db.sync_host_pull_health()?,
                Err(error)
                    if matches!(
                        error.downcast_ref::<rusqlite::Error>(),
                        Some(rusqlite::Error::SqliteFailure(code, _))
                            if matches!(code.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
                    ) =>
                {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "legacy database is corrupt; Host Pull Health cannot be recovered"
                    )
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Restore durable health and migrate legacy database-only records.
    /// Call under the Archive writer lock for mutations, including Rebuild.
    pub(crate) fn sync_host_pull_health(&self) -> Result<()> {
        self.update_host_pull_health(|_| {})
    }

    fn update_host_pull_health(
        &self,
        update: impl FnOnce(&mut BTreeMap<String, HostPullRecord>),
    ) -> Result<()> {
        let root = self
            .conn
            .path()
            .filter(|p| !p.is_empty())
            .and_then(|p| Path::new(p).parent());
        let mut health = match root {
            Some(root) => load(root)?,
            None => Health {
                version: 1,
                hosts: BTreeMap::new(),
            },
        };
        let mut stmt = self
            .conn
            .prepare("SELECT host,tracked_since_ms,last_success_ms FROM host_pulls")?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                HostPullRecord {
                    tracked_since_ms: r.get(1)?,
                    last_success_ms: r.get(2)?,
                },
            ))
        })? {
            let (host, old) = row?;
            let record = health.hosts.entry(host).or_insert(old);
            record.tracked_since_ms = record.tracked_since_ms.min(old.tracked_since_ms);
            record.last_success_ms = record.last_success_ms.max(old.last_success_ms);
        }
        update(&mut health.hosts);
        if let Some(root) = root {
            store(root, &health)?;
        }
        for (host, record) in health.hosts {
            self.conn.execute(
                "INSERT INTO host_pulls(host,tracked_since_ms,last_success_ms) VALUES (?1,?2,?3)
                 ON CONFLICT(host) DO UPDATE SET tracked_since_ms=excluded.tracked_since_ms,last_success_ms=excluded.last_success_ms
                 WHERE tracked_since_ms != excluded.tracked_since_ms OR last_success_ms IS NOT excluded.last_success_ms",
                params![host, record.tracked_since_ms, record.last_success_ms],
            )?;
        }
        Ok(())
    }

    pub fn host_pull_record(&self, host: &str) -> Result<Option<HostPullRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT tracked_since_ms,last_success_ms FROM host_pulls WHERE host=?1",
                [host],
                |row| {
                    Ok(HostPullRecord {
                        tracked_since_ms: row.get(0)?,
                        last_success_ms: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Health {
    version: u32,
    hosts: BTreeMap<String, HostPullRecord>,
}

fn load(root: &Path) -> Result<Health> {
    let path = root.join("host-pulls.json");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Health {
                version: 1,
                hosts: BTreeMap::new(),
            });
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let health: Health =
        serde_json::from_reader(file).with_context(|| format!("parsing {}", path.display()))?;
    ensure!(
        health.version == 1,
        "unsupported host-pulls.json version {}",
        health.version
    );
    Ok(health)
}

fn store(root: &Path, health: &Health) -> Result<()> {
    let path = root.join("host-pulls.json");
    let mut bytes = serde_json::to_vec_pretty(health)?;
    bytes.push(b'\n');
    if fs::read(&path).ok().as_ref() == Some(&bytes) {
        return Ok(());
    }
    let mut staged = tempfile::NamedTempFile::new_in(root)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist(&path)
        .with_context(|| format!("publishing {}", path.display()))?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
