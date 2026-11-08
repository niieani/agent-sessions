use super::*;
use crate::{archive::Archive, fleet::FleetConfig};
use std::{collections::BTreeSet, time::Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    version: u32,
    leads: Vec<serde_json::Value>,
    #[serde(deserialize_with = "nullable_cursor")]
    next_cursor: Option<String>,
    more: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Bookmark {
    version: u32,
    url: String,
    cursor: String,
}

/// Exclusive archive writer; commits each validated page before its Bookmark.
pub fn sync(archive: &Archive, selected: Option<&str>) -> Result<usize> {
    let _lock = archive.acquire_writer_lock()?;
    let config =
        FleetConfig::load(archive.root())?.context("lead sync requires archive config.toml")?;
    let mut names = BTreeSet::new();
    for feed in &config.lead_feeds {
        anyhow::ensure!(
            !feed.name.trim().is_empty() && names.insert(&feed.name),
            "empty or duplicate lead feed name {:?}",
            feed.name
        );
        anyhow::ensure!(
            (feed.url.starts_with("https://") || feed.url.starts_with("http://"))
                && !feed.url.contains(['?', '#']),
            "lead feed URL must be HTTP(S), without query or fragment (feed {:?})",
            feed.name
        );
    }
    let feeds = config
        .lead_feeds
        .iter()
        .filter(|f| selected.is_none_or(|name| name == f.name))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !feeds.is_empty(),
        "no configured lead feeds match {:?}",
        selected
    );
    let mut db = archive.db()?;
    let mut count = 0;
    for feed in feeds {
        count += sync_feed(archive, &mut db, feed)
            .with_context(|| format!("syncing lead feed {:?}", feed.name))?;
    }
    Ok(count)
}
fn sync_feed(archive: &Archive, db: &mut crate::archive::Db, feed: &FeedConfig) -> Result<usize> {
    let root = archive.root();
    let base = root.join("lead-feeds");
    directory(&base, false)?;
    let path = base.join(format!(
        "{}.json",
        crate::path_key::identity_component(&feed.name)
    ));
    let mut cursor = if regular_file(&path)? {
        let saved: Bookmark = serde_json::from_reader(fs::File::open(&path)?)?;
        anyhow::ensure!(
            saved.version == 1 && saved.url == feed.url && !saved.cursor.is_empty(),
            "invalid lead Bookmark or feed URL changed; use a new feed name"
        );
        Some(saved.cursor)
    } else {
        None
    };
    let token = feed
        .token_file
        .as_ref()
        .map(|path| -> Result<String> {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            };
            let token = fs::read_to_string(&path).context("reading lead feed token-file")?;
            let token = token.trim();
            anyhow::ensure!(
                !token.is_empty() && !token.chars().any(char::is_whitespace),
                "invalid lead feed bearer token"
            );
            Ok(token.to_owned())
        })
        .transpose()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut seen = BTreeSet::new();
    if let Some(c) = &cursor {
        seen.insert(c.clone());
    }
    let mut count = 0;
    loop {
        let mut request = agent.get(&feed.url).query("limit", "200");
        if let Some(cursor) = &cursor {
            request = request.query("after", cursor);
        }
        if let Some(token) = &token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let mut response = request.call().context("requesting Session Lead Feed")?;
        anyhow::ensure!(
            response.status().as_u16() == 200,
            "Session Lead Feed returned HTTP {}: {}",
            response.status(),
            response
                .body_mut()
                .read_to_string()
                .context("reading error response")?
        );
        let page: Page = response
            .body_mut()
            .read_json()
            .context("decoding Session Lead Feed v1")?;
        anyhow::ensure!(
            page.version == 1,
            "unsupported Session Lead Feed version {}",
            page.version
        );
        anyhow::ensure!(page.leads.len() <= 200, "lead page exceeds requested limit");
        let leads = page
            .leads
            .into_iter()
            .map(SessionLead::from_wire)
            .collect::<Result<Vec<_>>>()?;
        for lead in &leads {
            lead.validate()?;
            anyhow::ensure!(
                seen.insert(lead.cursor.clone()),
                "lead cursor repeated; feed must advance strictly in feed order"
            );
        }
        if let Some(last) = leads.last() {
            anyhow::ensure!(
                page.next_cursor.as_ref() == Some(&last.cursor),
                "nextCursor must equal last lead cursor"
            );
        } else {
            anyhow::ensure!(
                !page.more && page.next_cursor == cursor,
                "empty page must finish and preserve the requested cursor"
            );
        }
        // Opaque cursors cannot be compared lexically: only the producer knows their order.
        for lead in &leads {
            store(root, lead)?;
            count += 1;
        }
        db.upsert_session_leads(&leads)
            .context("leads are durable; index refresh failed; rerun sync or rebuild")?;
        if let Some(next) = &page.next_cursor {
            directory(&base, true)?;
            publish(
                &path,
                &Bookmark {
                    version: 1,
                    url: feed.url.clone(),
                    cursor: next.clone(),
                },
            )?;
        }
        cursor = page.next_cursor;
        if !page.more {
            break;
        }
    }
    Ok(count)
}

fn nullable_cursor<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}
