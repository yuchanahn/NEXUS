//! Versioned transport envelopes. No HTTP client, runtime, or agent dependency.

use llm_contract::{GenerateRequest, GenerationEvent, ModelSpec, ProviderError};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRequest {
    pub version: u32,
    pub request_id: String,
    pub provider: String,
    pub request: GenerateRequest,
    /// Relative server-side budget, never a serialized process-local Instant.
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequest {
    pub version: u32,
    pub provider: String,
    pub model: ModelSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub version: u32,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResponse {
    /// True means an active local call received the cancellation signal.
    pub signalled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequest {
    pub version: u32,
    pub provider: String,
    pub state: llm_contract::ProviderState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseResponse {
    pub released: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply<T> {
    pub version: u32,
    pub request_id: Option<String>,
    pub value: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorReply {
    pub version: u32,
    pub request_id: Option<String>,
    pub error: ProviderError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamReply {
    pub version: u32,
    pub request_id: String,
    /// Starts at zero and increases once per event, including the terminal event.
    pub sequence: u64,
    pub event: GenerationEvent,
}
