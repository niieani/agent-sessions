use serde_json::json;

use super::client::{CompletionKind, ModelRequest};
use super::files::{DayDigest, PromptSource};
use super::tokenizer::{TokenCounter, fit_messages};
use crate::archive::db::SessionRow;
use crate::fleet::DigestConfig;
use crate::model::{Message, MessageKind, Role};
use crate::workday::WorkdayConfig;

pub const PROMPT_VERSION: u32 = 4;
/// At most this many opening user turns stand in for a missing Intent.
const OPENING_TURNS: usize = 3;
/// Each opening turn is clipped to this many characters.
const OPENING_TURN_CHARS: usize = 1_000;
const BUILTIN_DAY_PROMPT: &str = include_str!("prompts/day.txt");
const BUILTIN_SESSION_TITLE_PROMPT: &str = include_str!("prompts/session-title.txt");

pub struct PreparedDay {
    pub request: ModelRequest,
    pub truncated: bool,
    pub input_tokens: usize,
    pub prompt_source: PromptSource,
}

pub struct DayRequestContext<'a> {
    pub project: Option<&'a str>,
    pub topic: Option<&'a str>,
    /// The Session Intent carried from earlier Session Days, if any.
    pub intent: Option<&'a str>,
    /// The Session's opening user turns, sent only when `intent` is unknown
    /// and this Session Day's body does not contain them.
    pub opening: &'a [String],
    pub previous: &'a [DayDigest],
    pub workday_config: &'a WorkdayConfig,
    pub max_input_tokens: usize,
    pub counter: &'a dyn TokenCounter,
}

pub fn day_request(
    session: &SessionRow,
    workday: &str,
    messages: &[Message],
    config: &DigestConfig,
    context: DayRequestContext<'_>,
) -> anyhow::Result<PreparedDay> {
    let summary_budget = messages
        .iter()
        .filter(|message| matches!(message.role, Role::User | Role::Agent))
        .count()
        .saturating_mul(40)
        .clamp(60, 900);
    let (template, prompt_source) = match config.prompts.day.as_deref() {
        Some(prompt) => (prompt, PromptSource::Config),
        None => (BUILTIN_DAY_PROMPT, PromptSource::Builtin),
    };
    // The title rule names the Topic outright and holds for configured
    // prompts too; validation enforces it (`super::title`).
    let title_rule = match context.topic {
        Some(topic) => format!(
            "The Topic is {topic:?}. The title is only this day's facet: never repeat {topic:?} or begin the title with it."
        ),
        None => "Choose the Topic first. The title is only this day's facet: never repeat the Topic you chose or begin the title with it.".to_string(),
    };
    let system = format!(
        "{}\n\n{title_rule}",
        template
            .replace("{classifications}", &super::CLASSIFICATIONS.join(", "))
            .replace("{summary_budget}", &summary_budget.to_string())
            .trim_end()
    );
    let make_user = |body: String| {
        let previous = context
            .previous
            .iter()
            .map(|digest| {
                let topic = digest.topic.as_deref().or(context.topic);
                json!({
                    "workday": digest.workday,
                    "facet": topic.map_or(Some(digest.title.as_str()), |topic| {
                        super::title::facet_of(&digest.title, topic)
                    }),
                    "summary": digest.summary,
                    "classification": digest.classification,
                })
            })
            .collect::<Vec<_>>();
        let mut fixed = json!({
            "source": session.source,
            "nativeId": session.native_id,
            "project": context.project,
            "workday": workday,
            "topic": context.topic,
            "intent": context.intent,
            "priorDigests": previous,
        });
        if !context.opening.is_empty() {
            fixed["opening"] = json!(context.opening);
        }
        format!(
            "Context:\n{}\n\nSession Day body:\n{}",
            serde_json::to_string_pretty(&fixed).expect("serializing prompt context"),
            body
        )
    };
    let (request, truncated, input_tokens) = fit_messages(
        context.counter,
        messages,
        context.max_input_tokens,
        |messages| ModelRequest {
            kind: match (context.topic, context.intent) {
                (None, _) => CompletionKind::FirstSessionDay,
                (Some(_), Some(_)) => CompletionKind::ContinuationSessionDay,
                (Some(_), None) => CompletionKind::ContinuationSessionDayWithoutIntent,
            },
            system: system.clone(),
            user: make_user(crate::render::digest_body(
                session,
                messages,
                context.workday_config,
            )),
            corrections: Vec::new(),
        },
    )?;
    Ok(PreparedDay {
        request,
        truncated,
        input_tokens,
        prompt_source,
    })
}

pub fn session_title_request(
    topic: &str,
    digests: &[DayDigest],
    config: &DigestConfig,
    project: Option<&str>,
) -> (ModelRequest, PromptSource) {
    let (system, prompt_source) = match config.prompts.session_title.as_deref() {
        Some(prompt) => (prompt.to_string(), PromptSource::Config),
        None => (
            BUILTIN_SESSION_TITLE_PROMPT.to_string(),
            PromptSource::Builtin,
        ),
    };
    (
        ModelRequest {
            kind: CompletionKind::SessionTitle,
            system,
            user: serde_json::to_string_pretty(&json!({
                "project": project,
                "topic": topic,
                "days": digests.iter().map(|digest| json!({
                    "workday": digest.workday,
                    "title": digest.title,
                    "summary": digest.summary,
                })).collect::<Vec<_>>(),
            }))
            .expect("serializing Session title prompt"),
            corrections: Vec::new(),
        },
        prompt_source,
    )
}

/// The user's opening turns of a Session's first Session Day: where the
/// Intent is usually set. Agent replies are excluded; the Intent is the
/// user's direction, not the agent's plan.
pub fn opening_turns(first_day: &[Message]) -> Vec<String> {
    first_day
        .iter()
        .filter(|message| matches!(message.role, Role::User | Role::Agent))
        .filter(|message| message.kind != MessageKind::GoalContinuation)
        .map(|message| message.text.trim())
        .filter(|text| !text.is_empty())
        .take(OPENING_TURNS)
        .map(|text| match text.char_indices().nth(OPENING_TURN_CHARS) {
            Some((end, _)) => format!("{}…", &text[..end]),
            None => text.to_string(),
        })
        .collect()
}
