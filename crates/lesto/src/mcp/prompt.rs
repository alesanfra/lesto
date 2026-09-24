//! `Prompt`: what a route marked `mcp = "prompt"` returns.

use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::operation::{OperationBuilder, OperationOutput};

/// The messages of an MCP prompt: the return type of a route marked `mcp = "prompt"`.
///
/// Over MCP it is the result of `prompts/get`; over plain HTTP the route answers the same
/// document as JSON.
///
/// ```
/// use lesto::mcp::Prompt;
///
/// let prompt = Prompt::new()
///     .description("Review a note")
///     .user("Review the note below for clarity.")
///     .assistant("Paste it and I will point out what is unclear.");
/// assert_eq!(prompt.messages().len(), 2);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Prompt {
    /// What the prompt is for, when it depends on the arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// The conversation the prompt starts.
    messages: Vec<PromptMessage>,
}

impl Prompt {
    /// A prompt with no messages yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Describe this prompt (the route's doc comment already describes it in `prompts/list`).
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add a message from the user.
    pub fn user(self, text: impl Into<String>) -> Self {
        self.message(Role::User, text)
    }

    /// Add a message from the assistant.
    pub fn assistant(self, text: impl Into<String>) -> Self {
        self.message(Role::Assistant, text)
    }

    /// Add a text message from `role`.
    pub fn message(mut self, role: Role, text: impl Into<String>) -> Self {
        self.messages.push(PromptMessage {
            role,
            content: PromptContent::Text { text: text.into() },
        });
        self
    }

    /// The messages, in order.
    pub fn messages(&self) -> &[PromptMessage] {
        &self.messages
    }
}

/// One message of a [`Prompt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PromptMessage {
    /// Who says it.
    pub role: Role,
    /// What is said.
    pub content: PromptContent,
}

/// Who says a [`PromptMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The user.
    User,
    /// The model.
    Assistant,
}

/// The content of a [`PromptMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
#[non_exhaustive]
pub enum PromptContent {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
}

impl IntoResponse for Prompt {
    fn into_response(self) -> Response {
        axum::Json(self).into_response()
    }
}

impl OperationOutput for Prompt {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        <axum::Json<Prompt> as OperationOutput>::describe(builder, status);
    }
}
