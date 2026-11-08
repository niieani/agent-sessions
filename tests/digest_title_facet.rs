//! Session Day titles never repeat their Topic: a facet that equals or
//! begins with the Topic is corrected through the retry loop, and once
//! retries are exhausted the leading Topic is stripped (a day left with no
//! facet is titled by its Topic alone).

use std::collections::VecDeque;
use std::fs;
use std::sync::Mutex;

use agent_sessions::archive::Archive;
use agent_sessions::digest::{CompletionKind, DigestOptions, ModelClient, ModelRequest};
use agent_sessions::model::{Conversation, Message, MessageKind, Provenance, Role};

fn ms(value: &str) -> i64 {
    agent_sessions::timefmt::parse_iso_ms(value).unwrap()
}

fn message(text: &str, timestamp: &str) -> Message {
    Message {
        role: Role::User,
        kind: MessageKind::Dialog,
        text: text.into(),
        timestamp_ms: Some(ms(timestamp)),
        actions_before: 0,
    }
}

fn conversation(messages: Vec<Message>) -> Conversation {
    Conversation {
        provenance: Provenance {
            source: "codex".into(),
            host: "test".into(),
            native_id: "facet".into(),
            native_path: "/store/facet".into(),
        },
        title: None,
        project_path: Some("/work/project".into()),
        native_project: None,
        created_at_ms: messages.first().and_then(|message| message.timestamp_ms),
        updated_at_ms: messages.last().and_then(|message| message.timestamp_ms),
        messages,
        transcript_facts: Default::default(),
        subagent_parent: None,
        inherited_context: None,
        subagent_references: Vec::new(),
        branches: Vec::new(),
        subagent_links: Vec::new(),
        trailing_actions: 0,
    }
}

/// Answers Session Day requests from a script (the last entry repeats) and
/// records every request.
struct ScriptedClient {
    first_day: Mutex<VecDeque<&'static str>>,
    later_day: Mutex<VecDeque<&'static str>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedClient {
    fn new(first_day: &[&'static str], later_day: &[&'static str]) -> Self {
        Self {
            first_day: Mutex::new(first_day.iter().copied().collect()),
            later_day: Mutex::new(later_day.iter().copied().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn day_requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.kind != CompletionKind::SessionTitle)
            .cloned()
            .collect()
    }
}

fn next(script: &Mutex<VecDeque<&'static str>>) -> String {
    let mut script = script.lock().unwrap();
    if script.len() > 1 {
        script.pop_front().unwrap().into()
    } else {
        script.front().unwrap().to_string()
    }
}

impl ModelClient for ScriptedClient {
    fn complete(&self, request: &ModelRequest) -> anyhow::Result<String> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(match request.kind {
            CompletionKind::FirstSessionDay => next(&self.first_day),
            CompletionKind::ContinuationSessionDay
            | CompletionKind::ContinuationSessionDayWithoutIntent => next(&self.later_day),
            CompletionKind::SessionTitle => r#"{"title":"Archive Work"}"#.into(),
        })
    }
}

fn first(title: &str) -> String {
    format!(
        r#"{{"topic":"Archive","title":{title:?},"intent":"Build it","tldr":null,"summary":"Worked.","classification":["build"]}}"#
    )
}

fn later(title: &str) -> String {
    format!(
        r#"{{"title":{title:?},"intent":null,"tldr":null,"summary":"Worked more.","classification":["build"]}}"#
    )
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

struct Run {
    _root: tempfile::TempDir,
    dir: std::path::PathBuf,
    stats: agent_sessions::digest::DigestStats,
}

impl Run {
    fn day(&self, workday: &str) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.dir.join(format!("{workday}.json"))).unwrap())
            .unwrap()
    }
}

fn digest(client: &ScriptedClient, messages: Vec<Message>) -> Run {
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open(root.path()).unwrap();
    fs::write(
        root.path().join("config.toml"),
        r#"[views]
timezone = "UTC"
[digest]
tokenizer = 'tests/fixtures/tokenizer'
base-url = "http://127.0.0.1:1/v1"
model = "fixture"
api-key-env = "TEST_API_KEY"
max-input-tokens = 60000
request-timeout = "1s"
retries = 2
"#,
    )
    .unwrap();
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation(messages))
        .unwrap();
    let stats = agent_sessions::digest::run_with_client(
        &archive,
        client,
        &DigestOptions {
            session: None,
            force: false,
            day: None,
            from: None,
            through: None,
            dry_run: false,
            now_ms: ms("2026-09-03T10:00:00Z"),
        },
    )
    .unwrap();
    let dir = agent_sessions::digest::digest_dir(root.path(), "codex", "facet").unwrap();
    Run {
        _root: root,
        dir,
        stats,
    }
}

fn one_day() -> Vec<Message> {
    vec![message("build it", "2026-08-30T10:00:00Z")]
}

fn two_days() -> Vec<Message> {
    vec![
        message("build it", "2026-08-30T10:00:00Z"),
        message("deploy it", "2026-08-31T10:00:00Z"),
    ]
}

#[test]
fn a_first_day_title_repeating_its_topic_is_corrected_by_retry() {
    let client = ScriptedClient::new(
        &[leak(first("Archive: setup")), leak(first("Setup"))],
        &[leak(later("unused"))],
    );
    let run = digest(&client, one_day());
    assert_eq!(run.stats.failed, 0);
    assert_eq!(run.day("2026-08-30")["title"], "Archive - Setup");
    let requests = client.day_requests();
    assert_eq!(requests.len(), 2, "one correction round");
    let correction = &requests[1].corrections[0].error;
    assert!(correction.contains("Archive"), "correction names the Topic");
    assert!(run.dir.join("2026-08-30.rejected.jsonl").is_file());
}

#[test]
fn exhausted_retries_strip_the_leading_topic_instead_of_failing() {
    let client = ScriptedClient::new(&[leak(first("archive — setup"))], &[leak(later("x"))]);
    let run = digest(&client, one_day());
    assert_eq!(run.stats.failed, 0);
    assert_eq!(client.day_requests().len(), 3, "every retry was spent");
    let day = run.day("2026-08-30");
    assert_eq!(day["title"], "Archive - setup");
    assert_eq!(day["lockedTitle"], "Archive - setup");
}

#[test]
fn a_facet_that_is_only_the_topic_leaves_a_topic_only_title() {
    let client = ScriptedClient::new(&[leak(first("Archive"))], &[leak(later("x"))]);
    let run = digest(&client, one_day());
    assert_eq!(run.stats.failed, 0);
    let day = run.day("2026-08-30");
    assert_eq!(day["title"], "Archive");
    assert_eq!(day["lockedTitle"], "Archive");
}

#[test]
fn a_facet_merely_sharing_a_word_prefix_is_accepted() {
    let client = ScriptedClient::new(&[leak(first("Archives overview"))], &[leak(later("x"))]);
    let run = digest(&client, one_day());
    assert_eq!(client.day_requests().len(), 1);
    assert_eq!(
        run.day("2026-08-30")["title"],
        "Archive - Archives overview"
    );
}

#[test]
fn later_days_name_the_topic_and_see_prior_titles_as_facets_only() {
    let client = ScriptedClient::new(
        &[leak(first("Setup"))],
        &[leak(later("Archive - Deploy")), leak(later("Deploy"))],
    );
    let run = digest(&client, two_days());
    assert_eq!(run.stats.failed, 0);
    assert_eq!(run.day("2026-08-31")["title"], "Archive - Deploy");

    let later = client
        .day_requests()
        .into_iter()
        .find(|request| request.kind != CompletionKind::FirstSessionDay)
        .unwrap();
    let context: serde_json::Value = {
        let start = later.user.find('{').unwrap();
        let end = later.user.find("\n\nSession Day body:").unwrap();
        serde_json::from_str(&later.user[start..end]).unwrap()
    };
    assert_eq!(context["topic"], "Archive");
    assert_eq!(context["priorDigests"][0]["facet"], "Setup");
    assert!(context["priorDigests"][0].get("title").is_none());
}
