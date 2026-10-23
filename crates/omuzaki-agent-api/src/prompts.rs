//! The `/` menu's types (§7): what the server's MCP prompts look like to the panel.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::HostError;

/// One argument of a prompt. All arguments are text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct PromptArgInfo {
    /// The argument's name; the panel shows it with underscores as spaces.
    pub name: String,
    /// Helper text; empty when the server sent none.
    pub description: String,
    /// Whether "Insert" waits for it.
    pub required: bool,
}

impl PromptArgInfo {
    /// An argument.
    pub fn new(name: impl Into<String>, description: impl Into<String>, required: bool) -> Self {
        Self { name: name.into(), description: description.into(), required }
    }
}

/// One prompt the server offers this session ("Set up my first stream").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct PromptInfo {
    /// What `prompt` is called with.
    pub name: String,
    /// What the menu shows; the name when the server sent no title.
    pub title: String,
    /// One line of help; empty when the server sent none.
    pub description: String,
    /// Its arguments, in the server's order.
    pub arguments: Vec<PromptArgInfo>,
}

impl PromptInfo {
    /// A prompt with no arguments; the title is the name.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self { title: name.clone(), name, description: String::new(), arguments: Vec::new() }
    }

    /// Sets the title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Sets the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the arguments.
    #[must_use]
    pub fn with_arguments(mut self, arguments: Vec<PromptArgInfo>) -> Self {
        self.arguments = arguments;
        self
    }
}

/// The host's cached prompt list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum PromptsState {
    /// A fetch is under way (or the first one has not started).
    Loading,
    /// The list, sorted by name. Empty is not an error.
    Ready(Vec<PromptInfo>),
    /// The last fetch failed; `refresh_prompts` retries.
    Failed(HostError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_survives_json() {
        let prompt = PromptInfo::new("set_up_first_stream")
            .with_title("Set up my first stream")
            .with_description("A guided start")
            .with_arguments(vec![PromptArgInfo::new("character", "Which character goes live", true)]);
        for state in
            [PromptsState::Loading, PromptsState::Ready(vec![prompt]), PromptsState::Failed(HostError::SessionEnded)]
        {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(serde_json::from_str::<PromptsState>(&json).unwrap(), state);
        }
    }

    #[test]
    fn a_prompt_without_a_title_shows_its_name() {
        assert_eq!(PromptInfo::new("x").title, "x");
    }
}
