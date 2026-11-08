use super::tokenizer::{TokenCounter, fit_messages};
use super::{CompletionKind, ModelRequest};
use crate::model::{Message, Role};
use anyhow::Result;
struct Characters;
impl TokenCounter for Characters {
    fn count_text(&self, text: &str) -> Result<usize> {
        Ok(text.chars().count())
    }
    fn count(&self, request: &ModelRequest) -> Result<usize> {
        Ok(request.system.chars().count() + request.user.chars().count() + 12)
    }
}
fn build(messages: &[Message]) -> ModelRequest {
    ModelRequest {
        kind: CompletionKind::FirstSessionDay,
        system: "system".into(),
        user: messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        corrections: vec![],
    }
}
fn message(text: String) -> Message {
    Message {
        role: Role::User,
        kind: Default::default(),
        text,
        timestamp_ms: None,
        actions_before: 0,
    }
}
#[test]
fn dense_unicode_paste_keeps_both_ends_and_whole_short_reply() {
    let messages = [
        message(format!("BEGIN{}END", "123,猫,".repeat(1000))),
        message("short reply".into()),
    ];
    let (req, truncated, tokens) = fit_messages(&Characters, &messages, 150, build).unwrap();
    assert!(truncated);
    assert!(req.user.starts_with("BEGIN"));
    assert!(req.user.contains("[message middle truncated]"));
    assert!(req.user.ends_with("END\nshort reply"));
    assert_eq!(tokens, Characters.count(&req).unwrap());
    assert!(tokens <= 150);
}
#[test]
fn many_messages_shrink_largest_first_and_keep_short_messages() {
    let mut messages = vec![message("tiny".into())];
    messages.extend((0..10).map(|_| message("abc猫".repeat(100))));
    let (req, truncated, tokens) = fit_messages(&Characters, &messages, 2000, build).unwrap();
    assert!(truncated && tokens <= 2000);
    assert!(req.user.starts_with("tiny\n"));
    assert_eq!(req.user.matches("[message middle truncated]").count(), 10);
}
#[test]
fn fixed_context_cannot_be_silently_clipped() {
    assert!(fit_messages(&Characters, &[message("body".into())], 10, build).is_err());
}
#[test]
fn fitting_messages_are_unchanged() {
    let messages = [message("hello".into())];
    let (req, truncated, _) = fit_messages(&Characters, &messages, 100, build).unwrap();
    assert!(!truncated);
    assert_eq!(req, build(&messages));
}

#[test]
#[ignore = "requires an explicit cloned archive and local model"]
fn measure_local_prompt() -> anyhow::Result<()> {
    use super::*;
    let root = std::env::var("DIGEST_CHECK_ARCHIVE")?;
    let archive = Archive::open(std::path::Path::new(&root))?;
    let fleet = FleetConfig::load(archive.root())?.unwrap();
    let config = fleet.digest.as_ref().unwrap();
    let counter = tokenizer::LocalTokenizer::load(config)?;
    let workday = WorkdayConfig::new(&fleet.views.workday)?;
    let options = DigestOptions {
        session: Some(SessionKey::parse(
            "chatgpt:683903c9-08f4-8003-aba3-86d563bcd80d",
        )?),
        day: Some("2025-05-29".into()),
        from: None,
        through: None,
        force: true,
        dry_run: true,
        now_ms: crate::adapters::now_ms(),
    };
    let db = Db::open_read_only(&archive.db_path())?;
    let selection = select_plans(&archive, &db, &fleet, &workday, &options, false)?;
    let plan = &selection.plans[0];
    let day = plan
        .days
        .iter()
        .find(|d| d.workday == "2025-05-29")
        .unwrap();
    let prepare = |limit| {
        prompt::day_request(
            &plan.session,
            &day.workday,
            &day.messages,
            config,
            prompt::DayRequestContext {
                project: plan.project.as_deref(),
                topic: None,
                intent: None,
                opening: &[],
                previous: &[],
                workday_config: &workday,
                max_input_tokens: limit,
                counter: &counter,
            },
        )
    };
    let full = prepare(usize::MAX)?;
    let start = std::time::Instant::now();
    for _ in 0..10 {
        tokenizer::TokenCounter::count(&counter, &full.request)?;
    }
    eprintln!(
        "COUNT full={} average={:?}",
        full.input_tokens,
        start.elapsed() / 10
    );
    let start = std::time::Instant::now();
    let fitted = prepare(60_000)?;
    eprintln!(
        "FITTED tokens={} truncated={} elapsed={:?} messages={} chars={:?}",
        fitted.input_tokens,
        fitted.truncated,
        start.elapsed(),
        day.messages.len(),
        day.messages
            .iter()
            .map(|m| m.text.chars().count())
            .collect::<Vec<_>>()
    );
    assert!(fitted.truncated && fitted.input_tokens <= 60_000);
    for message in &day.messages {
        assert!(
            fitted
                .request
                .user
                .contains(&message.text.chars().take(80).collect::<String>())
        );
        let tail: String = message
            .text
            .chars()
            .rev()
            .take(80)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert!(fitted.request.user.contains(&tail));
        if message.text.len() < 5000 {
            assert!(fitted.request.user.contains(&message.text));
        }
    }
    Ok(())
}

#[test]
fn local_tokenizer_counts_template_and_correction_turns() {
    let config = crate::fleet::FleetConfig::parse("[digest]\nbase-url='http://localhost/v1'\nmodel='fixture'\napi-key-env='UNUSED'\ntokenizer='tests/fixtures/tokenizer'\n").unwrap().digest.unwrap();
    let counter = super::tokenizer::LocalTokenizer::load(&config).unwrap();
    let mut request = build(&[message("hello".into())]);
    let initial = counter.count(&request).unwrap();
    assert!(
        initial
            > counter.count_text(&request.system).unwrap()
                + counter.count_text(&request.user).unwrap()
    );
    request.corrections.push(super::ModelCorrection {
        raw: "bad output".into(),
        error: "invalid JSON".into(),
    });
    assert!(counter.count(&request).unwrap() > initial);
}

#[test]
fn missing_or_unloadable_tokenizer_is_an_explicit_error() {
    let mut config = crate::fleet::FleetConfig::parse(
        "[digest]\nbase-url='http://localhost/v1'\nmodel='fixture'\napi-key-env='UNUSED'\n",
    )
    .unwrap()
    .digest
    .unwrap();
    let error = super::tokenizer::LocalTokenizer::load(&config)
        .err()
        .unwrap();
    assert!(error.to_string().contains("digest.tokenizer"));
    config.tokenizer = Some(std::path::PathBuf::from("tests/fixtures/tokenizer/absent"));
    assert!(super::tokenizer::LocalTokenizer::load(&config).is_err());
}

#[test]
fn omlx_format_counts_the_schema_for_each_completion_kind() {
    use crate::fleet::TokenizerPromptFormat;
    let mut config = crate::fleet::FleetConfig::parse("[digest]\nbase-url='http://localhost/v1'\nmodel='fixture'\napi-key-env='UNUSED'\ntokenizer='tests/fixtures/tokenizer'\n").unwrap().digest.unwrap();
    let plain = super::tokenizer::LocalTokenizer::load(&config).unwrap();
    config.tokenizer_prompt_format = TokenizerPromptFormat::Omlx;
    let omlx = super::tokenizer::LocalTokenizer::load(&config).unwrap();
    for kind in [
        CompletionKind::FirstSessionDay,
        CompletionKind::ContinuationSessionDay,
        CompletionKind::ContinuationSessionDayWithoutIntent,
        CompletionKind::SessionTitle,
    ] {
        let mut request = build(&[message("hello".into())]);
        request.kind = kind;
        assert!(omlx.count(&request).unwrap() > plain.count(&request).unwrap());
    }
}
