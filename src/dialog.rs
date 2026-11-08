//! Current whole-Conversation facts for configurable Kind predicates.
use crate::model::{Message, Role};
use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NumericConditions {
    pub eq: Option<u64>,
    pub lt: Option<u64>,
    pub lte: Option<u64>,
    pub gt: Option<u64>,
    pub gte: Option<u64>,
}

impl NumericConditions {
    fn matches(&self, value: u64) -> bool {
        self.eq.is_none_or(|n| value == n)
            && self.lt.is_none_or(|n| value < n)
            && self.lte.is_none_or(|n| value <= n)
            && self.gt.is_none_or(|n| value > n)
            && self.gte.is_none_or(|n| value >= n)
    }

    fn validate(&self) -> Result<()> {
        if self.eq.is_none()
            && self.lt.is_none()
            && self.lte.is_none()
            && self.gt.is_none()
            && self.gte.is_none()
        {
            bail!("empty numeric conditions");
        }
        let lower = self.gte.unwrap_or(0).max(match self.gt {
            Some(n) => n
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("contradictory numeric bounds"))?,
            None => 0,
        });
        let upper = self.lte.unwrap_or(u64::MAX).min(match self.lt {
            Some(n) => n
                .checked_sub(1)
                .ok_or_else(|| anyhow::anyhow!("contradictory numeric bounds"))?,
            None => u64::MAX,
        });
        if lower > upper || self.eq.is_some_and(|n| n < lower || n > upper) {
            bail!("contradictory numeric bounds");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DialogConditions {
    pub user_messages: Option<NumericConditions>,
    pub agent_messages: Option<NumericConditions>,
    pub assistant_messages: Option<NumericConditions>,
    pub user_chars: Option<NumericConditions>,
    pub agent_chars: Option<NumericConditions>,
    pub assistant_chars: Option<NumericConditions>,
    pub total_chars: Option<NumericConditions>,
}

impl DialogConditions {
    fn fields(&self) -> [(&str, Option<&NumericConditions>); 7] {
        [
            ("user-messages", self.user_messages.as_ref()),
            ("agent-messages", self.agent_messages.as_ref()),
            ("assistant-messages", self.assistant_messages.as_ref()),
            ("user-chars", self.user_chars.as_ref()),
            ("agent-chars", self.agent_chars.as_ref()),
            ("assistant-chars", self.assistant_chars.as_ref()),
            ("total-chars", self.total_chars.as_ref()),
        ]
    }

    pub(crate) fn validate(&self) -> Result<()> {
        use anyhow::Context;
        let fields = self.fields();
        if fields.iter().all(|(_, field)| field.is_none()) {
            bail!("empty Dialog Statistics conditions");
        }
        for (name, condition) in fields {
            if let Some(condition) = condition {
                condition
                    .validate()
                    .with_context(|| format!("match.dialog.{name}"))?;
            }
        }
        Ok(())
    }

    pub(crate) fn matches(&self, stats: &DialogStatistics) -> bool {
        self.fields()
            .into_iter()
            .zip([
                stats.user_messages,
                stats.agent_messages,
                stats.assistant_messages,
                stats.user_chars,
                stats.agent_chars,
                stats.assistant_chars,
                stats.user_chars + stats.agent_chars + stats.assistant_chars,
            ])
            .all(|((_, condition), value)| {
                condition.is_none_or(|condition| condition.matches(value))
            })
    }
}

#[derive(Debug, Default)]
pub(crate) struct DialogStatistics {
    pub user_messages: u64,
    pub agent_messages: u64,
    pub assistant_messages: u64,
    pub user_chars: u64,
    pub agent_chars: u64,
    pub assistant_chars: u64,
}

impl DialogStatistics {
    pub(crate) fn from_messages(messages: &[Message]) -> Self {
        let mut stats = Self::default();
        for message in messages.iter().filter(|message| !message.text.is_empty()) {
            let (count, chars) = match message.role {
                Role::User => (&mut stats.user_messages, &mut stats.user_chars),
                Role::Agent => (&mut stats.agent_messages, &mut stats.agent_chars),
                Role::Assistant => (&mut stats.assistant_messages, &mut stats.assistant_chars),
            };
            *count += 1;
            *chars += message.text.chars().count() as u64;
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MessageKind;

    #[test]
    fn agent_statistics_are_distinct_from_user_statistics() {
        let messages = [
            Message {
                role: Role::User,
                kind: MessageKind::Dialog,
                text: "user".into(),
                timestamp_ms: None,
                actions_before: 0,
            },
            Message {
                role: Role::Agent,
                kind: MessageKind::Dialog,
                text: "agent".into(),
                timestamp_ms: None,
                actions_before: 0,
            },
        ];
        let stats = DialogStatistics::from_messages(&messages);
        assert_eq!(stats.user_messages, 1);
        assert_eq!(stats.agent_messages, 1);
        assert_eq!(stats.user_chars, 4);
        assert_eq!(stats.agent_chars, 5);
    }
}
