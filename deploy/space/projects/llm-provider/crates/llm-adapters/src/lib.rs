//! One-call HTTP adapters. No browser control, agent loop, or tool execution.
mod codec;
mod emulated;
mod streaming;
pub use codec::{chat_request, chat_response, responses_request, responses_response};
use futures_util::StreamExt;
use llm_contract::*;
use llm_provider::{
    CallContext, EventStream, LlmProvider, ProviderFuture, validate_request, validate_response,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, future::Future};

const BODY_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Chat,
    AntigravityChat,
    Responses,
    GrokSpace,
    Yqcloud,
    NexusHarness,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub upstream: String,
    pub status: String,
    #[serde(default)]
    pub tool_protocol: Option<ToolProtocol>,
    #[serde(default)]
    pub tool_status: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterConfig {
    pub id: String,
    pub protocol: Protocol,
    pub base_url: String,
    /// Explicit owner opt-in for a configured HTTP service on a private Docker network.
    /// Public/remote endpoints remain HTTPS by default.
    #[serde(default)]
    pub allow_http: bool,
    #[serde(default)]
    pub auth_env: Option<String>,
    #[serde(default)]
    pub auth_file: Option<String>,
    #[serde(default)]
    pub text_only: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default = "max_tokens")]
    pub max_tokens_field: String,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub tools: Option<Support>,
    #[serde(default)]
    pub usage_estimated: bool,
}
fn max_tokens() -> String {
    "max_tokens".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub providers: Vec<AdapterConfig>,
}
pub struct HttpProvider {
    config: AdapterConfig,
    client: reqwest::Client,
}
pub(crate) fn invalid(msg: &str) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse, msg)
}
fn unsupported(msg: &str) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedFeature, msg)
}
pub(crate) fn validate_calls(
    request: &GenerateRequest,
    calls: &[ToolCall],
) -> Result<(), ProviderError> {
    let mut ids = HashSet::new();
    for call in calls {
        if !ids.insert(&call.id)
            || request.messages.iter().flat_map(|message| &message.content).any(|part| matches!(part, ContentPart::ToolCall(previous) if previous.id == call.id))
            || !request.tools.iter().any(|tool| tool.name == call.name)
            || request.tool_choice == ToolChoice::None
            || matches!(&request.tool_choice, ToolChoice::Named(name) if *name != call.name)
        {
            return Err(invalid("model tool calls violate declared tool policy"));
        }
    }
    if calls.is_empty()
        && matches!(
            request.tool_choice,
            ToolChoice::Required | ToolChoice::Named(_)
        )
    {
        return Err(invalid("model did not honor required tool choice"));
    }
    Ok(())
}
pub(crate) fn http_error(e: reqwest::Error) -> ProviderError {
    ProviderError::new(
        if e.is_timeout() {
            ErrorKind::Timeout
        } else {
            ErrorKind::Connection
        },
        "upstream HTTP transport failed",
    )
}
pub(crate) fn status_error(status: u16) -> ProviderError {
    let kind = match status {
        400 | 409 | 422 => ErrorKind::InvalidRequest,
        401 => ErrorKind::Authentication,
        403 => ErrorKind::AccessDenied,
        404 => ErrorKind::InvalidRequest,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        _ => ErrorKind::ProviderInternal,
    };
    let mut e = ProviderError::new(kind, "upstream rejected the request");
    e.provider_code = Some(status.to_string());
    e
}
pub(crate) async fn control(c: &CallContext) -> ProviderError {
    let deadline = async {
        if let Some(at) = c.deadline {
            tokio::time::sleep_until(at.into()).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {biased;()=c.cancellation.cancelled()=>ProviderError::new(ErrorKind::Cancelled,"call cancelled locally"),()=deadline=>ProviderError::new(ErrorKind::Timeout,"call deadline exceeded")}
}
pub(crate) async fn guarded<T>(
    c: &CallContext,
    f: impl Future<Output = Result<T, ProviderError>>,
) -> Result<T, ProviderError> {
    c.check()?;
    tokio::select! {biased;e=control(c)=>Err(e),v=f=>v}
}
pub(crate) async fn body(r: reqwest::Response, c: &CallContext) -> Result<Vec<u8>, ProviderError> {
    guarded(c, async move {
        let mut stream = r.bytes_stream();
        let mut out = Vec::new();
        while let Some(b) = stream.next().await {
            let b = b.map_err(http_error)?;
            if out.len().saturating_add(b.len()) > BODY_LIMIT {
                return Err(invalid("upstream response byte limit exceeded"));
            }
            out.extend_from_slice(&b);
        }
        Ok(out)
    })
    .await
}
impl HttpProvider {
    pub fn new(config: AdapterConfig) -> Result<Self, ProviderError> {
        let url = reqwest::Url::parse(&config.base_url).map_err(|_| {
            ProviderError::new(ErrorKind::InvalidRequest, "invalid configured endpoint")
        })?;
        let local = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "::1" | "host.docker.internal")
        );
        if config.id.trim().is_empty()
            || (!local && !config.allow_http && url.scheme() != "https")
            || !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "unsafe configured endpoint",
            ));
        }
        if !matches!(
            config.max_tokens_field.as_str(),
            "max_tokens" | "max_completion_tokens"
        ) {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "invalid max tokens field",
            ));
        }
        let mut ids = HashSet::new();
        for m in &config.models {
            if m.id.trim().is_empty() || m.upstream.trim().is_empty() || !ids.insert(&m.id) {
                return Err(ProviderError::new(
                    ErrorKind::InvalidRequest,
                    "invalid configured model catalog",
                ));
            }
        }
        if config.streaming && !matches!(config.protocol, Protocol::Chat) {
            return Err(unsupported(
                "this protocol currently supports nonstreaming calls only",
            ));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .map_err(http_error)?;
        Ok(Self { config, client })
    }
    pub fn config(&self) -> &AdapterConfig {
        &self.config
    }
    fn model<'a>(&'a self, id: &'a str) -> Result<&'a str, ProviderError> {
        self.config
            .models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.upstream.as_str())
            .ok_or_else(|| {
                ProviderError::new(
                    ErrorKind::InvalidRequest,
                    "model is not registered for this provider",
                )
            })
    }
    fn tool_protocol(&self, model: &ModelSpec) -> ToolProtocol {
        self.config
            .models
            .iter()
            .find(|m| m.id == model.id)
            .and_then(|m| m.tool_protocol)
            .unwrap_or_else(|| {
                if matches!(self.config.protocol, Protocol::NexusHarness) {
                    ToolProtocol::McpRelay
                } else if self.config.text_only || self.config.tools == Some(Support::Unsupported) {
                    ToolProtocol::Unsupported
                } else {
                    ToolProtocol::Unknown
                }
            })
    }
    fn needs_emulation(&self, request: &GenerateRequest) -> bool {
        self.tool_protocol(&request.model) == ToolProtocol::Emulated
            && (!request.tools.is_empty()
                || request.messages.iter().any(|m| {
                    m.content
                        .iter()
                        .any(|p| !matches!(p, ContentPart::Text { .. }))
                })
                || (self.config.text_only
                    && (request.instructions.is_some()
                        || request.messages.len() != 1
                        || request.messages[0].role != Role::User)))
    }
    fn token(&self) -> Result<Option<String>, ProviderError> {
        if let Some(k) = &self.config.auth_env {
            return std::env::var(k)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(Some)
                .ok_or_else(|| {
                    ProviderError::new(
                        ErrorKind::Authentication,
                        "configured credential is missing",
                    )
                });
        }
        if let Some(p) = &self.config.auth_file {
            let s = std::fs::read_to_string(p).map_err(|_| {
                ProviderError::new(
                    ErrorKind::Authentication,
                    "configured credential file is unavailable",
                )
            })?;
            if s.len() > 16384 || s.trim().is_empty() {
                return Err(ProviderError::new(
                    ErrorKind::Authentication,
                    "invalid credential file",
                ));
            }
            return Ok(Some(s.trim().into()));
        }
        Ok(None)
    }
    fn prepare(&self, mut r: GenerateRequest, stream: bool) -> Result<Value, ProviderError> {
        validate_request(&r, self.id(), &self.capabilities(&r.model))?;
        if !matches!(
            self.config.protocol,
            Protocol::NexusHarness | Protocol::AntigravityChat
        ) && r.messages.iter().any(|m| m.provider_state.is_some())
        {
            return Err(unsupported(
                "continuation state is not supported by this adapter",
            ));
        }
        if (self.config.text_only || matches!(self.config.protocol, Protocol::Yqcloud))
            && (r.instructions.is_some()
                || r.messages.len() != 1
                || r.messages[0].role != Role::User
                || r.messages[0]
                    .content
                    .iter()
                    .any(|p| !matches!(p, ContentPart::Text { .. })))
        {
            return Err(unsupported("this bridge accepts one plain user message"));
        }
        r.model.id = self.model(&r.model.id)?.into();
        let mut value = match self.config.protocol {
            Protocol::Chat | Protocol::AntigravityChat => {
                codec::chat_request(&r, stream, &self.config.max_tokens_field)?
            }
            Protocol::Responses => codec::responses_request(&r, stream)?,
            Protocol::GrokSpace => {
                if !r.tools.is_empty()
                    || !matches!(r.tool_choice, ToolChoice::Auto | ToolChoice::None)
                    || r.options.max_output_tokens.is_some()
                    || r.options.temperature.is_some()
                {
                    return Err(unsupported(
                        "Grok Space accepts text messages without tool or sampling options",
                    ));
                }
                let messages = codec::plain_messages(&r)?;
                json!({"model":r.model.id,"messages":messages})
            }
            Protocol::Yqcloud => {
                let messages = codec::plain_messages(&r)?;
                let prompt = messages[0]["content"]
                    .as_str()
                    .ok_or_else(|| invalid("missing prompt"))?;
                json!({"prompt":prompt,"userId":format!("nexus-{}",uuid::Uuid::new_v4()),"network":false,"system":"","withoutContext":true,"stream":false})
            }
            Protocol::NexusHarness => {
                serde_json::to_value(&r).map_err(|_| invalid("could not encode harness request"))?
            }
        };
        if matches!(self.config.protocol, Protocol::AntigravityChat) {
            let states = r
                .messages
                .iter()
                .filter(|m| m.role == Role::Assistant)
                .map(|m| m.provider_state.as_ref())
                .collect::<Vec<_>>();
            let assistants = value["messages"]
                .as_array_mut()
                .ok_or_else(|| invalid("missing encoded messages"))?
                .iter_mut()
                .filter(|m| m["role"] == "assistant");
            for (message, state) in assistants.zip(states) {
                if let Some(state) = state {
                    message["nexus_state"] = state.data.clone();
                }
            }
        }
        if self.config.text_only {
            value.as_object_mut().unwrap().remove("tool_choice");
        }
        Ok(value)
    }
    async fn send(
        &self,
        v: Value,
        stream: bool,
        c: &CallContext,
    ) -> Result<reqwest::Response, ProviderError> {
        let suffix = match self.config.protocol {
            Protocol::Chat | Protocol::AntigravityChat => "/chat/completions",
            Protocol::Responses => "/responses",
            Protocol::GrokSpace => {
                if stream {
                    "/api/chat/stream"
                } else {
                    "/api/chat"
                }
            }
            Protocol::Yqcloud => "/api/generateStream",
            Protocol::NexusHarness => "/generate",
        };
        self.send_path(v, suffix, c).await
    }
    async fn send_path(
        &self,
        v: Value,
        suffix: &str,
        c: &CallContext,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut req = self
            .client
            .post(format!(
                "{}{suffix}",
                self.config.base_url.trim_end_matches('/')
            ))
            .json(&v);
        if let Some(token) = self.token()? {
            req = req.bearer_auth(token);
        }
        let r = guarded(c, async { req.send().await.map_err(http_error) }).await?;
        if !r.status().is_success() {
            return Err(status_error(r.status().as_u16()));
        }
        Ok(r)
    }
}
impl LlmProvider for HttpProvider {
    fn id(&self) -> &str {
        &self.config.id
    }
    fn release_state<'a>(
        &'a self,
        state: ProviderState,
        context: CallContext,
    ) -> ProviderFuture<'a, bool> {
        Box::pin(async move {
            if !matches!(self.config.protocol, Protocol::NexusHarness) {
                return Err(unsupported("this adapter has no releasable continuation"));
            }
            if state.provider != self.id() {
                return Err(ProviderError::new(
                    ErrorKind::InvalidRequest,
                    "continuation belongs to another adapter",
                ));
            }
            let response = self
                .send_path(
                    serde_json::to_value(state)
                        .map_err(|_| invalid("invalid continuation state"))?,
                    "/release",
                    &context,
                )
                .await?;
            let value: Value = serde_json::from_slice(&body(response, &context).await?)
                .map_err(|_| invalid("invalid release response"))?;
            value
                .get("released")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid("invalid release response contract"))
        })
    }
    fn capabilities(&self, model: &ModelSpec) -> Capabilities {
        let mode = self.tool_protocol(model);
        let tool_status = self
            .config
            .models
            .iter()
            .find(|entry| entry.id == model.id)
            .and_then(|entry| entry.tool_status.as_deref());
        let verified = tool_status == Some("provider_round_trip_verified");
        if matches!(self.config.protocol, Protocol::NexusHarness) {
            return Capabilities {
                tool_protocol: ToolProtocol::McpRelay,
                streaming: Support::Unsupported,
                tool_streaming: Support::Unsupported,
                tools: Support::Supported,
                required_tool_choice: Support::Unsupported,
                named_tool_choice: Support::Unsupported,
                max_output_tokens: Support::Unsupported,
                temperature: Support::Unsupported,
            };
        }
        let plain = self.config.text_only
            || matches!(
                self.config.protocol,
                Protocol::GrokSpace | Protocol::Yqcloud
            );
        Capabilities {
            tool_protocol: mode,
            streaming: if self.config.streaming {
                Support::Supported
            } else {
                Support::Unsupported
            },
            tool_streaming: if !self.config.streaming
                || mode == ToolProtocol::Emulated
                || mode == ToolProtocol::Unsupported
            {
                Support::Unsupported
            } else if mode == ToolProtocol::Native && verified {
                Support::Supported
            } else {
                Support::Unknown
            },
            tools: if matches!(
                mode,
                ToolProtocol::Native | ToolProtocol::Emulated | ToolProtocol::McpRelay
            ) {
                if verified {
                    Support::Supported
                } else if tool_status == Some("model_protocol_failed") {
                    Support::Unsupported
                } else {
                    Support::Unknown
                }
            } else if plain || mode == ToolProtocol::Unsupported {
                Support::Unsupported
            } else {
                self.config.tools.unwrap_or(Support::Unknown)
            },
            required_tool_choice: if mode == ToolProtocol::Emulated {
                Support::Supported
            } else if plain {
                Support::Unsupported
            } else {
                Support::Unknown
            },
            named_tool_choice: if mode == ToolProtocol::Emulated {
                Support::Supported
            } else if plain {
                Support::Unsupported
            } else {
                Support::Unknown
            },
            max_output_tokens: if plain {
                Support::Unsupported
            } else {
                Support::Unknown
            },
            temperature: if plain {
                Support::Unsupported
            } else {
                Support::Unknown
            },
        }
    }
    fn generate<'a>(
        &'a self,
        r: GenerateRequest,
        c: CallContext,
    ) -> ProviderFuture<'a, GenerateResponse> {
        Box::pin(async move {
            validate_request(&r, self.id(), &self.capabilities(&r.model))?;
            if self.tool_protocol(&r.model) == ToolProtocol::Emulated
                && r.messages
                    .iter()
                    .any(|message| message.provider_state.is_some())
            {
                return Err(unsupported(
                    "native continuation state cannot be replayed through emulation",
                ));
            }
            let original = r.clone();
            let (r, exchange) = if self.needs_emulation(&r) {
                let (request, exchange) = emulated::Exchange::prepare(&r);
                (request, Some(exchange))
            } else {
                (r, None)
            };
            let v = self.prepare(r, false)?;
            let response = self.send(v, false, &c).await?;
            if matches!(self.config.protocol, Protocol::Yqcloud) {
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                if !matches!(
                    content_type.split(';').next(),
                    Some("text/plain" | "application/text")
                ) {
                    return Err(invalid("unexpected prompt endpoint content type"));
                }
                let text = String::from_utf8(body(response, &c).await?)
                    .map_err(|_| invalid("invalid prompt endpoint UTF-8"))?;
                if text.trim().is_empty() {
                    return Err(invalid("prompt endpoint returned empty text"));
                }
                if text.starts_with("很抱歉地告诉您") && text.contains("盗用接口") {
                    return Err(ProviderError::new(
                        ErrorKind::AccessDenied,
                        "prompt endpoint returned an access restriction notice",
                    ));
                }
                let out = GenerateResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![ContentPart::Text { text }],
                        provider_state: None,
                    },
                    finish_reason: FinishReason::Unknown,
                    usage: Usage::default(),
                    request_id: None,
                };
                return if let Some(exchange) = exchange {
                    exchange.decode(out)
                } else {
                    Ok(out)
                };
            }
            let value: Value = serde_json::from_slice(&body(response, &c).await?)
                .map_err(|_| invalid("invalid upstream JSON"))?;
            let state = if matches!(self.config.protocol, Protocol::AntigravityChat) {
                value["choices"][0]["message"].get("nexus_state").cloned()
            } else {
                None
            };
            let mut out = match self.config.protocol {
                Protocol::Chat | Protocol::AntigravityChat => codec::chat_response(value)?,
                Protocol::Responses => codec::responses_response(value)?,
                Protocol::GrokSpace => codec::grok_response(value)?,
                Protocol::Yqcloud => unreachable!("handled plaintext above"),
                Protocol::NexusHarness => serde_json::from_value(value)
                    .map_err(|_| invalid("invalid harness response contract"))?,
            };
            if let Some(data) = state {
                out.message.provider_state = Some(ProviderState {
                    provider: self.id().into(),
                    data,
                });
            }
            if self.config.usage_estimated {
                out.usage = Usage::default();
            }
            if let Some(exchange) = exchange {
                out = exchange.decode(out)?;
            }
            let calls = out
                .message
                .content
                .iter()
                .filter_map(|part| {
                    if let ContentPart::ToolCall(call) = part {
                        Some(call.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            validate_calls(&original, &calls)?;
            validate_response(&out, self.id())?;
            Ok(out)
        })
    }
    fn stream<'a>(&'a self, r: GenerateRequest, c: CallContext) -> ProviderFuture<'a, EventStream> {
        Box::pin(async move {
            if self.needs_emulation(&r) {
                return Err(unsupported(
                    "emulated tool envelopes require buffered generate",
                ));
            }
            if self.capabilities(&r.model).streaming != Support::Supported {
                return Err(unsupported(
                    "real upstream streaming is unavailable for this bridge",
                ));
            }
            let v = self.prepare(r.clone(), true)?;
            let response = self.send(v, true, &c).await?;
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            if !content_type.starts_with("text/event-stream") {
                return Err(invalid("upstream did not return SSE"));
            }
            Ok(streaming::events(
                response,
                self.config.protocol.clone(),
                c,
                self.config.usage_estimated,
                r,
            ))
        })
    }
}
