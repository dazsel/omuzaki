//! [`AssistantEvent`] and the small types it carries (D1, D10). The panel drains these once per frame.

use omuzaki_ops_api::{ApprovalId, Budget, Spend};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::HostError;

/// Identifies one `ask_person` question within a conversation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct QuestionId(pub u64);

/// The four per-message caps (D2.7). `Limit(cap)` names the one that was hit.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Cap {
    /// LLM requests.
    Turns,
    /// Tool calls.
    ToolCalls,
    /// Wall time, excluding time waiting for a person.
    WallTime,
    /// Tokens, all four counters.
    Tokens,
}

/// How far along a plan item is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanStatus {
    /// Not started.
    Todo,
    /// Being worked on.
    Doing,
    /// Finished.
    Done,
}

/// One line of the answer's checklist (the `update_plan` tool, D9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct PlanItem {
    /// What the step is.
    pub text: String,
    /// Where it stands.
    pub status: PlanStatus,
}

impl PlanItem {
    /// A plan item.
    pub fn new(text: impl Into<String>, status: PlanStatus) -> Self {
        Self { text: text.into(), status }
    }
}

/// What the host tells the panel. The panel renders each as D10 says; the receiver drains them with
/// [`try_recv`](crate::hub::EventReceiver::try_recv).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum AssistantEvent {
    /// More text for the current bubble. The host coalesces them to at most one per 16 ms.
    TextDelta(String),
    /// A compact row for a tool call, ticked or crossed by the [`ToolResult`](AssistantEvent::ToolResult) of the same id.
    #[non_exhaustive]
    ToolCall {
        /// The call id.
        id: String,
        /// The tool's name.
        tool: String,
        /// The tool's title plus its main argument ("Face profile: egg-oval, 50 %").
        summary: String,
    },
    /// The call `id` finished: `ok` ticks the row, otherwise it is crossed.
    #[non_exhaustive]
    ToolResult {
        /// The call id.
        id: String,
        /// Whether the call succeeded.
        ok: bool,
    },
    /// The answer waits on an approval; the panel shows the approval card inline.
    #[non_exhaustive]
    Waiting {
        /// Which approval.
        approval: ApprovalId,
    },
    /// The session is paused (by Stop, the Agent panel or the budget); the panel shows a banner.
    Paused,
    /// The session resumed.
    Resumed,
    /// A question card with its choices and a text box.
    #[non_exhaustive]
    Question {
        /// Answer it with [`Assistant::answer`](crate::Assistant::answer).
        id: QuestionId,
        /// The question.
        text: String,
        /// Suggested answers; empty for a free-text question.
        choices: Vec<String>,
    },
    /// The answer's checklist, whole each time.
    Plan(Vec<PlanItem>),
    /// A per-message cap was hit; the host makes one last turn to say what is left.
    Limit(Cap),
    /// What the session has spent against its budget, for the meter line.
    #[non_exhaustive]
    Budget {
        /// Spent so far.
        spent: Spend,
        /// The session's caps.
        budget: Budget,
    },
    /// Something failed; the panel shows the error with its one action.
    Error(HostError),
    /// The answer is over; the panel re-enables its box.
    Done,
}

impl AssistantEvent {
    /// A tool call row.
    pub fn tool_call(id: impl Into<String>, tool: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::ToolCall { id: id.into(), tool: tool.into(), summary: summary.into() }
    }

    /// The end of the call `id`.
    pub fn tool_result(id: impl Into<String>, ok: bool) -> Self {
        Self::ToolResult { id: id.into(), ok }
    }

    /// The answer waits on `approval`.
    pub fn waiting(approval: ApprovalId) -> Self {
        Self::Waiting { approval }
    }

    /// A question card.
    pub fn question(id: QuestionId, text: impl Into<String>, choices: Vec<String>) -> Self {
        Self::Question { id, text: text.into(), choices }
    }

    /// The meter line's data.
    pub fn budget(spent: Spend, budget: Budget) -> Self {
        Self::Budget { spent, budget }
    }

    /// Whether Stop withdraws this event while it is still unread (D4): the output the person would otherwise keep
    /// seeing after the click.
    pub fn is_withdrawn_by_stop(&self) -> bool {
        matches!(self, Self::TextDelta(_) | Self::ToolCall { .. } | Self::Question { .. } | Self::Plan(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn every_event() -> Vec<AssistantEvent> {
        vec![
            AssistantEvent::TextDelta("hi".into()),
            AssistantEvent::tool_call("c1", "face_set_profile", "Face profile: egg-oval, 50 %"),
            AssistantEvent::tool_result("c1", true),
            AssistantEvent::waiting(ApprovalId(7)),
            AssistantEvent::Paused,
            AssistantEvent::Resumed,
            AssistantEvent::question(QuestionId(1), "Which?", vec!["a".into(), "b".into()]),
            AssistantEvent::Plan(vec![
                PlanItem::new("look", PlanStatus::Done),
                PlanItem::new("edit", PlanStatus::Doing),
            ]),
            AssistantEvent::Limit(Cap::ToolCalls),
            AssistantEvent::budget(Spend { usd_micros: 814, tokens: 6_327, seconds: 0 }, Budget::default()),
            AssistantEvent::Error(HostError::NeedsKey),
            AssistantEvent::Done,
        ]
    }

    #[test]
    fn every_variant_survives_json() {
        for e in every_event() {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<AssistantEvent>(&json).unwrap(), e, "{json}");
        }
    }

    #[test]
    fn stop_withdraws_exactly_the_four_output_kinds() {
        let withdrawn: Vec<bool> = every_event().iter().map(AssistantEvent::is_withdrawn_by_stop).collect();
        assert_eq!(withdrawn, [true, true, false, false, false, false, true, true, false, false, false, false]);
    }
}
