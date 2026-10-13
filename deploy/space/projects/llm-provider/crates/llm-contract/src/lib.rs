//! Provider-neutral data. No network clients, executors, or tool implementations.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// An adapter-local model identifier, not a fixed catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub model: ModelSpec,
    pub instructions: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    pub options: GenerationOptions,
}

impl GenerateRequest {
    pub fn text(model: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            model: ModelSpec { id: model.into() },
            instructions: None,
            messages: vec![Message::user(prompt)],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            options: GenerationOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationOptions {
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
    /// Adapter-owned continuation data; callers preserve it without interpreting it.
    pub provider_state: Option<ProviderState>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentPart::Text { text: text.into() }],
            provider_state: None,
        }
    }

    pub fn tool(result: ToolResult) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::ToolResult(result)],
            provider_state: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

/// A declaration only. Never contains a callback, process, or executor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "name", rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

/// A completed, syntactically valid JSON-object call, not an authorized action.
/// Schema and business-rule validation belong to the tool runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique within the supplied conversation. Adapters synthesize IDs if needed.
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub outcome: ToolOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum ToolOutcome {
    Success(Value),
    Failure { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderState {
    /// Stable adapter identity. Cross-provider replay is not portable.
    pub provider: String,
    pub data: Value,
}

/// Unknown is distinct from unsupported; capabilities are model-specific.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub tool_protocol: ToolProtocol,
    pub streaming: Support,
    #[serde(default = "unknown_support")]
    pub tool_streaming: Support,
    pub tools: Support,
    pub required_tool_choice: Support,
    pub named_tool_choice: Support,
    pub max_output_tokens: Support,
    pub temperature: Support,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            tool_protocol: ToolProtocol::Unknown,
            streaming: Support::Unknown,
            tool_streaming: Support::Unknown,
            tools: Support::Unknown,
            required_tool_choice: Support::Unknown,
            named_tool_choice: Support::Unknown,
            max_output_tokens: Support::Unknown,
            temperature: Support::Unknown,
        }
    }
}
fn unknown_support() -> Support {
    Support::Unknown
}

/// How calls cross the model boundary; Supported alone does not imply native calls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProtocol {
    Native,
    Emulated,
    McpRelay,
    Unsupported,
    #[default]
    Unknown,
}

/// Fields are optional because providers may omit usage. Updates are snapshots,
/// not increments. Missing fields do not mean zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Other(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub message: Message,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    pub request_id: Option<String>,
}

/// Block indices start at zero and are assigned in output order. Deltas from
/// different blocks may interleave. A block must end before Completed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum GenerationEvent {
    Started {
        request_id: Option<String>,
    },
    TextStarted {
        index: u32,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ToolCallStarted {
        index: u32,
        id: String,
        name: String,
    },
    ToolArgumentsDelta {
        index: u32,
        fragment: String,
    },
    BlockEnded {
        index: u32,
    },
    UsageUpdated {
        usage: Usage,
    },
    Completed {
        finish_reason: FinishReason,
        provider_state: Option<ProviderState>,
    },
    Failed {
        error: ProviderError,
    },
    Cancelled,
}

impl GenerationEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidRequest,
    UnsupportedFeature,
    Authentication,
    AccessDenied,
    RateLimited,
    Connection,
    Timeout,
    InvalidResponse,
    ProviderInternal,
    Cancelled,
}

/// Human-readable fields must be sanitized by adapters (no credentials or bodies).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub message: String,
    pub provider_code: Option<String>,
    pub request_id: Option<String>,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            provider_code: None,
            request_id: None,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for ProviderError {}
