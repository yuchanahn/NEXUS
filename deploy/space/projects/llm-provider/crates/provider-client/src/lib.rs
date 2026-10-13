//! HTTP client implementing the same provider trait as an in-process adapter.
//! Requires Tokio only at the transport edge; common contracts remain independent.

mod sse;

use futures_util::StreamExt;
use llm_contract::*;
use llm_provider::{
    CallContext, EventStream, LlmProvider, ProviderFuture, validate_request, validate_response,
};
use llm_wire::*;
use reqwest::{Client, Response, Url};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashMap, future::Future, time::Duration};

#[derive(Clone)]
pub struct RemoteProvider {
    client: Client,
    base: Url,
    token: String,
    provider: String,
    capabilities: HashMap<String, Capabilities>,
}

impl RemoteProvider {
    pub fn new(
        base: &str,
        bearer_token: impl Into<String>,
        provider: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let base = Url::parse(base).map_err(|_| invalid_request("invalid server URL"))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(invalid_request(
                "server URL must be an HTTP(S) origin without credentials, path, query, or fragment",
            ));
        }
        let token = bearer_token.into();
        if token.len() < 16 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(invalid_request(
                "bearer token must have at least 16 printable ASCII characters",
            ));
        }
        let provider = provider.into();
        if provider.trim().is_empty() {
            return Err(invalid_request("empty provider ID"));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| {
                ProviderError::new(ErrorKind::Connection, "cannot construct HTTP client")
            })?;
        Ok(Self {
            client,
            base,
            token,
            provider,
            capabilities: HashMap::new(),
        })
    }

    /// Configured model-specific knowledge. Trait capabilities() never does I/O.
    pub fn with_capabilities(mut self, model: ModelSpec, capabilities: Capabilities) -> Self {
        self.capabilities.insert(model.id, capabilities);
        self
    }

    pub async fn fetch_capabilities(
        &self,
        model: ModelSpec,
        context: CallContext,
    ) -> Result<Capabilities, ProviderError> {
        let request = CapabilityRequest {
            version: PROTOCOL_VERSION,
            provider: self.provider.clone(),
            model,
        };
        let response = guarded(&context, self.post("v1/capabilities", &request)).await?;
        read_reply(response, &context, None).await
    }

    /// Explicit remote cancellation for callers managing wire request IDs.
    pub async fn cancel_request(
        &self,
        request_id: String,
        context: CallContext,
    ) -> Result<bool, ProviderError> {
        let request = CancelRequest {
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
        };
        let response = guarded(&context, self.post("v1/cancel", &request)).await?;
        let result: CancelResponse = read_reply(response, &context, Some(&request_id)).await?;
        Ok(result.signalled)
    }

    async fn post<T: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<Response, ProviderError> {
        let bytes = serde_json::to_vec(body)
            .map_err(|_| invalid_request("request cannot be serialized"))?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(invalid_request("request byte limit exceeded"));
        }
        self.client
            .post(self.base.join(path).expect("constant relative route"))
            .bearer_auth(&self.token)
            .header("content-type", "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(http_error)
    }

    fn envelope(
        &self,
        request: GenerateRequest,
        context: &CallContext,
    ) -> Result<CallRequest, ProviderError> {
        context.check()?;
        validate_request(&request, &self.provider, &self.capabilities(&request.model))?;
        let timeout_ms = context.deadline.map(|deadline| {
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis()
                .max(1)
                .min(u64::MAX as u128) as u64
        });
        Ok(CallRequest {
            version: PROTOCOL_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            provider: self.provider.clone(),
            request,
            timeout_ms,
        })
    }
}

fn invalid_request(message: &str) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidRequest, message)
}
fn invalid_response(message: &str) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse, message)
}

fn http_error(error: reqwest::Error) -> ProviderError {
    // Do not propagate reqwest's URL-bearing display string or response bodies.
    ProviderError::new(
        if error.is_timeout() {
            ErrorKind::Timeout
        } else {
            ErrorKind::Connection
        },
        "HTTP transport failed",
    )
}

async fn control(context: &CallContext) -> ProviderError {
    let deadline = async {
        if let Some(deadline) = context.deadline {
            tokio::time::sleep_until(deadline.into()).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        biased;
        () = context.cancellation.cancelled() => ProviderError::new(ErrorKind::Cancelled, "call cancelled locally"),
        () = deadline => ProviderError::new(ErrorKind::Timeout, "local call deadline exceeded"),
    }
}

async fn guarded<T>(
    context: &CallContext,
    future: impl Future<Output = Result<T, ProviderError>>,
) -> Result<T, ProviderError> {
    context.check()?;
    tokio::select! { biased; error = control(context) => Err(error), result = future => result }
}

async fn limited_body(response: Response, context: &CallContext) -> Result<Vec<u8>, ProviderError> {
    guarded(context, async move {
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(http_error)?;
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(invalid_response("response byte limit exceeded"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })
    .await
}

async fn read_reply<T: DeserializeOwned>(
    response: Response,
    context: &CallContext,
    expected_id: Option<&str>,
) -> Result<T, ProviderError> {
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type.split(';').next() != Some("application/json") {
        return Err(invalid_response("expected a JSON response"));
    }
    let bytes = limited_body(response, context).await?;
    if !status.is_success() {
        let reply: ErrorReply = serde_json::from_slice(&bytes)
            .map_err(|_| invalid_response("invalid error envelope"))?;
        if reply.version != PROTOCOL_VERSION {
            return Err(invalid_response("unsupported reply version"));
        }
        return Err(reply.error);
    }
    let reply: Reply<T> = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_response("invalid response envelope"))?;
    if reply.version != PROTOCOL_VERSION || reply.request_id.as_deref() != expected_id {
        return Err(invalid_response("reply version or request ID mismatch"));
    }
    Ok(reply.value)
}

impl LlmProvider for RemoteProvider {
    fn release_state<'a>(
        &'a self,
        state: ProviderState,
        context: CallContext,
    ) -> ProviderFuture<'a, bool> {
        Box::pin(async move {
            if state.provider != self.provider {
                return Err(invalid_request("continuation belongs to another provider"));
            }
            let request = ReleaseRequest {
                version: PROTOCOL_VERSION,
                provider: self.provider.clone(),
                state,
            };
            let response = guarded(&context, self.post("v1/release", &request)).await?;
            let result: ReleaseResponse = read_reply(response, &context, None).await?;
            Ok(result.released)
        })
    }
    fn id(&self) -> &str {
        &self.provider
    }

    fn capabilities(&self, model: &ModelSpec) -> Capabilities {
        self.capabilities
            .get(&model.id)
            .cloned()
            .unwrap_or_default()
    }

    fn generate<'a>(
        &'a self,
        request: GenerateRequest,
        context: CallContext,
    ) -> ProviderFuture<'a, GenerateResponse> {
        Box::pin(async move {
            let envelope = self.envelope(request, &context)?;
            let response = guarded(&context, self.post("v1/generate", &envelope)).await?;
            let response = read_reply(response, &context, Some(&envelope.request_id)).await?;
            validate_response(&response, &self.provider)?;
            Ok(response)
        })
    }

    fn stream<'a>(
        &'a self,
        request: GenerateRequest,
        context: CallContext,
    ) -> ProviderFuture<'a, EventStream> {
        Box::pin(async move {
            let envelope = self.envelope(request, &context)?;
            let response = guarded(&context, self.post("v1/stream", &envelope)).await?;
            if !response.status().is_success() {
                return read_reply::<GenerateResponse>(
                    response,
                    &context,
                    Some(&envelope.request_id),
                )
                .await
                .map(|_| unreachable!("error status never produces a successful reply"));
            }
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if content_type.split(';').next() != Some("text/event-stream") {
                return Err(invalid_response("expected an SSE response"));
            }
            let stream = async_stream::stream! {
                let mut chunks = response.bytes_stream();
                let mut parser = sse::Parser::default();
                let mut sequence = 0;
                let mut total = 0usize;
                let mut terminal = false;
                while !terminal {
                    let chunk = guarded(&context, async {
                        chunks.next().await.ok_or_else(|| invalid_response("SSE ended without a terminal event"))?.map_err(http_error)
                    }).await;
                    let frames = match chunk {
                        Ok(chunk) => {
                            total = total.saturating_add(chunk.len());
                            if total > MAX_RESPONSE_BYTES { Err(invalid_response("SSE response byte limit exceeded")) }
                            else { parser.feed(&chunk) }
                        }
                        Err(error) => Err(error),
                    };
                    let frames = match frames {
                        Ok(frames) => frames,
                        Err(error) => {
                            if sequence == 0 { yield GenerationEvent::Started { request_id: None }; }
                            yield if error.kind == ErrorKind::Cancelled { GenerationEvent::Cancelled } else { GenerationEvent::Failed { error } };
                            break;
                        }
                    };
                    for frame in frames {
                        let reply = serde_json::from_slice::<StreamReply>(&frame)
                            .map_err(|_| invalid_response("invalid SSE envelope"))
                            .and_then(|reply| {
                                if reply.version != PROTOCOL_VERSION || reply.request_id != envelope.request_id || reply.sequence != sequence
                                    || (sequence == 0 && !matches!(reply.event, GenerationEvent::Started { .. }))
                                    || (sequence > 0 && matches!(reply.event, GenerationEvent::Started { .. })) {
                                    Err(invalid_response("SSE version, request ID, sequence, or start mismatch"))
                                } else { Ok(reply) }
                            });
                        match reply {
                            Ok(reply) => {
                                sequence += 1;
                                terminal = reply.event.is_terminal();
                                yield reply.event;
                            }
                            Err(error) => {
                                if sequence == 0 { yield GenerationEvent::Started { request_id: None }; }
                                yield GenerationEvent::Failed { error };
                                terminal = true;
                            }
                        }
                        if terminal { break; }
                    }
                }
                // Dropping the HTTP body ends local consumption. No detached cancel task.
                drop(chunks);
            };
            Ok(Box::pin(stream) as EventStream)
        })
    }
}
