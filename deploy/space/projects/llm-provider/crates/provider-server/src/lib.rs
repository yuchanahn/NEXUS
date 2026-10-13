//! HTTP/SSE boundary around one model call. Tool execution stays with callers.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State, rejection::JsonRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::StreamExt;
use llm_contract::*;
use llm_provider::{
    CallContext, Cancellation, EventStream, LlmProvider, collect_response, validate_request,
    validate_response,
};
use llm_wire::*;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub struct ServerConfig {
    pub models: Vec<serde_json::Value>,
    pub bearer_token: String,
    pub max_concurrent_calls: usize,
    pub default_timeout: Duration,
    pub max_timeout: Duration,
    pub dedup_ttl: Duration,
    pub max_remembered_requests: usize,
    pub max_response_bytes: usize,
}

impl ServerConfig {
    pub fn new(bearer_token: impl Into<String>) -> Self {
        Self {
            models: vec![],
            bearer_token: bearer_token.into(),
            max_concurrent_calls: 32,
            default_timeout: Duration::from_secs(60),
            max_timeout: Duration::from_secs(120),
            dedup_ttl: Duration::from_secs(300),
            max_remembered_requests: 2048,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }
}

struct Entry {
    cancellation: Option<Cancellation>,
    completed_at: Option<Instant>,
}

struct Inner {
    config: ServerConfig,
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    requests: Mutex<HashMap<String, Entry>>,
    semaphore: Arc<Semaphore>,
}

#[derive(Clone)]
pub struct ServerState(Arc<Inner>);

impl ServerState {
    pub fn new(
        config: ServerConfig,
        providers: Vec<Arc<dyn LlmProvider>>,
    ) -> Result<Self, ProviderError> {
        if config.bearer_token.len() < 16
            || !config.bearer_token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "bearer token must have at least 16 printable ASCII characters",
            ));
        }
        if config.max_concurrent_calls == 0
            || config.max_remembered_requests < config.max_concurrent_calls
            || config.default_timeout.is_zero()
            || config.default_timeout > config.max_timeout
            || config.max_response_bytes == 0
            || config.max_response_bytes > MAX_RESPONSE_BYTES
            || config.max_timeout > Duration::from_secs(3600)
            || config.dedup_ttl.is_zero()
        {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "invalid server resource limits",
            ));
        }
        let mut registry = HashMap::new();
        for provider in providers {
            if provider.id().trim().is_empty()
                || registry
                    .insert(provider.id().to_owned(), provider)
                    .is_some()
            {
                return Err(ProviderError::new(
                    ErrorKind::InvalidRequest,
                    "provider IDs must be nonempty and unique",
                ));
            }
        }
        if registry.is_empty() {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "at least one provider is required",
            ));
        }
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_calls));
        Ok(Self(Arc::new(Inner {
            config,
            providers: registry,
            requests: Mutex::new(HashMap::new()),
            semaphore,
        })))
    }

    fn provider(&self, id: &str) -> Result<Arc<dyn LlmProvider>, ApiError> {
        self.0.providers.get(id).cloned().ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                ErrorKind::InvalidRequest,
                "unknown provider",
                None,
            )
        })
    }

    fn begin(&self, request: &CallRequest) -> Result<CallGuard, ApiError> {
        let id = Some(request.request_id.clone());
        check_version(request.version)?;
        if request.request_id.is_empty()
            || request.request_id.len() > 128
            || !request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorKind::InvalidRequest,
                "invalid request_id",
                id,
            ));
        }
        if request.timeout_ms == Some(0) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorKind::InvalidRequest,
                "timeout_ms must be positive",
                id,
            ));
        }
        let permit = self.0.semaphore.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                ErrorKind::RateLimited,
                "concurrent call limit reached",
                id.clone(),
            )
        })?;
        let cancellation = Cancellation::default();
        let mut requests = self.0.requests.lock().expect("request registry poisoned");
        requests.retain(|_, entry| {
            entry
                .completed_at
                .is_none_or(|at| at.elapsed() < self.0.config.dedup_ttl)
        });
        if requests.contains_key(&request.request_id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                ErrorKind::InvalidRequest,
                "request_id already used; automatic replay is not supported",
                id,
            ));
        }
        if requests.len() >= self.0.config.max_remembered_requests {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                ErrorKind::RateLimited,
                "request registry limit reached",
                id,
            ));
        }
        requests.insert(
            request.request_id.clone(),
            Entry {
                cancellation: Some(cancellation.clone()),
                completed_at: None,
            },
        );
        let timeout = request
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(self.0.config.default_timeout)
            .min(self.0.config.max_timeout);
        Ok(CallGuard {
            state: self.clone(),
            id: request.request_id.clone(),
            context: CallContext {
                cancellation,
                deadline: Some(Instant::now() + timeout),
                trace_id: Some(request.request_id.clone()),
            },
            _permit: permit,
        })
    }
}

struct CallGuard {
    state: ServerState,
    id: String,
    context: CallContext,
    _permit: OwnedSemaphorePermit,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.context.cancellation.cancel();
        if let Ok(mut requests) = self.state.0.requests.lock()
            && let Some(entry) = requests.get_mut(&self.id)
        {
            entry.cancellation = None;
            entry.completed_at = Some(Instant::now());
        }
    }
}

async fn providers(State(state): State<ServerState>) -> Json<serde_json::Value> {
    let mut ids: Vec<_> = state.0.providers.keys().cloned().collect();
    ids.sort();
    Json(serde_json::json!({"version": 1, "providers": ids}))
}
async fn models(State(state): State<ServerState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"version": 1, "models": state.0.config.models}))
}
pub fn router(state: ServerState) -> Router {
    let api = Router::new()
        .route("/v1/providers", get(providers))
        .route("/v1/models", get(models))
        .route("/v1/capabilities", post(capabilities))
        .route("/v1/generate", post(generate))
        .route("/v1/stream", post(stream))
        .route("/v1/cancel", post(cancel))
        .route("/v1/release", post(release))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/health/live", get(|| async { "live" }))
        .route("/health/ready", get(|| async { "ready" }))
        .merge(api)
        .with_state(state)
}

async fn authenticate(State(state): State<ServerState>, request: Request, next: Next) -> Response {
    let expected = state.0.config.bearer_token.as_bytes();
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .as_bytes();
    if expected.ct_eq(supplied).unwrap_u8() != 1 {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            ErrorKind::Authentication,
            "authentication required",
            None,
        )
        .into_response();
    }
    next.run(request).await
}

fn check_version(version: u32) -> Result<(), ApiError> {
    if version != PROTOCOL_VERSION {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            ErrorKind::InvalidRequest,
            "unsupported protocol version",
            None,
        ));
    }
    Ok(())
}

fn body<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|error| {
        ApiError::new(
            error.status(),
            ErrorKind::InvalidRequest,
            "invalid JSON request or body too large",
            None,
        )
    })
}

async fn capabilities(
    State(state): State<ServerState>,
    payload: Result<Json<CapabilityRequest>, JsonRejection>,
) -> Result<Json<Reply<Capabilities>>, ApiError> {
    let request = body(payload)?;
    check_version(request.version)?;
    if request.model.id.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            ErrorKind::InvalidRequest,
            "empty model ID",
            None,
        ));
    }
    Ok(Json(Reply {
        version: PROTOCOL_VERSION,
        request_id: None,
        value: state
            .provider(&request.provider)?
            .capabilities(&request.model),
    }))
}

async fn release(
    State(state): State<ServerState>,
    payload: Result<Json<ReleaseRequest>, JsonRejection>,
) -> Result<Json<Reply<ReleaseResponse>>, ApiError> {
    let request = body(payload)?;
    check_version(request.version)?;
    if request.state.provider != request.provider {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            ErrorKind::InvalidRequest,
            "continuation belongs to another provider",
            None,
        ));
    }
    let provider = state.provider(&request.provider)?;
    let _permit = state.0.semaphore.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorKind::RateLimited,
            "concurrent call limit reached",
            None,
        )
    })?;
    let context = CallContext {
        deadline: Some(Instant::now() + state.0.config.default_timeout),
        ..CallContext::default()
    };
    let operation = provider.release_state(request.state, context.clone());
    let released = tokio::select! {
        biased;
        error = wait_control(&context) => return Err(ApiError::from_provider(error, None)),
        result = operation => result.map_err(|error| ApiError::from_provider(error, None))?,
    };
    Ok(Json(Reply {
        version: PROTOCOL_VERSION,
        request_id: None,
        value: ReleaseResponse { released },
    }))
}

async fn cancel(
    State(state): State<ServerState>,
    payload: Result<Json<CancelRequest>, JsonRejection>,
) -> Result<Json<Reply<CancelResponse>>, ApiError> {
    let request = body(payload)?;
    check_version(request.version)?;
    let requests = state.0.requests.lock().expect("request registry poisoned");
    let cancellation = requests
        .get(&request.request_id)
        .and_then(|entry| entry.cancellation.as_ref());
    if let Some(cancellation) = cancellation {
        cancellation.cancel();
    }
    Ok(Json(Reply {
        version: PROTOCOL_VERSION,
        request_id: Some(request.request_id),
        value: CancelResponse {
            signalled: cancellation.is_some(),
        },
    }))
}

async fn wait_control(context: &CallContext) -> ProviderError {
    let deadline = context.deadline.expect("server calls have a deadline");
    tokio::select! {
        biased;
        () = context.cancellation.cancelled() => ProviderError::new(ErrorKind::Cancelled, "call cancelled"),
        () = tokio::time::sleep_until(deadline.into()) => ProviderError::new(ErrorKind::Timeout, "server call deadline exceeded"),
    }
}

async fn open_call(
    state: &ServerState,
    request: &CallRequest,
    guard: &CallGuard,
) -> Result<EventStream, ApiError> {
    let provider = state.provider(&request.provider)?;
    validate_request(
        &request.request,
        provider.id(),
        &provider.capabilities(&request.request.model),
    )
    .map_err(|error| ApiError::from_provider(error, Some(guard.id.clone())))?;
    tokio::select! {
        biased;
        error = wait_control(&guard.context) => Err(ApiError::from_provider(error, Some(guard.id.clone()))),
        result = provider.stream(request.request.clone(), guard.context.clone()) => result.map_err(|error| ApiError::from_provider(error, Some(guard.id.clone()))),
    }
}

/// Owns the call guard so disconnect/drop cancels upstream and frees the slot.
fn controlled(mut upstream: EventStream, guard: CallGuard) -> EventStream {
    Box::pin(async_stream::stream! {
        let mut started = false;
        let mut bytes = 0usize;
        loop {
            let event = tokio::select! {
                biased;
                error = wait_control(&guard.context) => {
                    if error.kind == ErrorKind::Cancelled { GenerationEvent::Cancelled }
                    else { GenerationEvent::Failed { error } }
                }
                event = upstream.next() => {
                    event.unwrap_or_else(|| GenerationEvent::Failed { error: ProviderError::new(ErrorKind::InvalidResponse, "upstream stream ended without a terminal event") })
                }
            };
            // Ensure transport-level failures are representable even before upstream starts.
            if !started {
                started = true;
                if !matches!(event, GenerationEvent::Started { .. }) {
                    yield GenerationEvent::Started { request_id: None };
                    if !matches!(event, GenerationEvent::Failed { .. } | GenerationEvent::Cancelled) {
                        yield GenerationEvent::Failed { error: ProviderError::new(ErrorKind::InvalidResponse, "upstream did not start correctly") };
                        break;
                    }
                }
            } else if matches!(event, GenerationEvent::Started { .. }) {
                yield GenerationEvent::Failed { error: ProviderError::new(ErrorKind::InvalidResponse, "duplicate upstream start") };
                break;
            }
            let size = serde_json::to_vec(&event).map(|bytes| bytes.len()).unwrap_or(usize::MAX);
            bytes = bytes.saturating_add(size);
            if size > MAX_EVENT_BYTES || bytes > guard.state.0.config.max_response_bytes {
                yield GenerationEvent::Failed { error: ProviderError::new(ErrorKind::InvalidResponse, "response byte limit exceeded") };
                break;
            }
            let terminal = event.is_terminal();
            yield event;
            if terminal { break; }
        }
        drop(upstream);
        drop(guard);
    })
}

async fn generate(
    State(state): State<ServerState>,
    payload: Result<Json<CallRequest>, JsonRejection>,
) -> Result<Json<Reply<GenerateResponse>>, ApiError> {
    let request = body(payload)?;
    let guard = state.begin(&request)?;
    let provider = state.provider(&request.provider)?;
    let capabilities = provider.capabilities(&request.request.model);
    let has_tools = !request.request.tools.is_empty()
        || request.request.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::ToolCall(_) | ContentPart::ToolResult(_)))
        });
    let value = if capabilities.streaming == Support::Unsupported
        || (has_tools && capabilities.tool_streaming == Support::Unsupported)
    {
        validate_request(
            &request.request,
            provider.id(),
            &provider.capabilities(&request.request.model),
        )
        .map_err(|error| ApiError::from_provider(error, Some(request.request_id.clone())))?;
        tokio::select! {
            biased;
            error = wait_control(&guard.context) => return Err(ApiError::from_provider(error, Some(request.request_id.clone()))),
            result = provider.generate(request.request, guard.context.clone()) => {
                result.map_err(|error| ApiError::from_provider(error, Some(request.request_id.clone())))?
            }
        }
    } else {
        let upstream = open_call(&state, &request, &guard).await?;
        collect_response(controlled(upstream, guard))
            .await
            .map_err(|error| ApiError::from_provider(error, Some(request.request_id.clone())))?
    };
    validate_response(&value, provider.id())
        .map_err(|error| ApiError::from_provider(error, Some(request.request_id.clone())))?;
    if serde_json::to_vec(&value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
        > state.0.config.max_response_bytes
    {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            ErrorKind::InvalidResponse,
            "response byte limit exceeded",
            Some(request.request_id),
        ));
    }
    Ok(Json(Reply {
        version: PROTOCOL_VERSION,
        request_id: Some(request.request_id),
        value,
    }))
}

async fn stream(
    State(state): State<ServerState>,
    payload: Result<Json<CallRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = body(payload)?;
    let guard = state.begin(&request)?;
    let upstream = open_call(&state, &request, &guard).await?;
    let mut events = controlled(upstream, guard);
    let stream = async_stream::stream! {
        let mut sequence = 0u64;
        while let Some(event) = events.next().await {
            let terminal = event.is_terminal();
            let reply = StreamReply { version: PROTOCOL_VERSION, request_id: request.request_id.clone(), sequence, event };
            sequence += 1;
            let event = Event::default().event("generation").json_data(reply).expect("wire event is JSON serializable");
            if terminal {
                // Drop the guard before yielding completion, including on backpressure.
                drop(events);
                yield Ok::<_, Infallible>(event);
                break;
            }
            yield Ok::<_, Infallible>(event);
        }
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default().interval(Duration::from_secs(10)))
        .into_response())
}

struct ApiError {
    status: StatusCode,
    reply: ErrorReply,
}

impl ApiError {
    fn new(status: StatusCode, kind: ErrorKind, message: &str, request_id: Option<String>) -> Self {
        Self {
            status,
            reply: ErrorReply {
                version: PROTOCOL_VERSION,
                request_id,
                error: ProviderError::new(kind, message),
            },
        }
    }

    fn from_provider(error: ProviderError, request_id: Option<String>) -> Self {
        let status = match error.kind {
            ErrorKind::InvalidRequest | ErrorKind::UnsupportedFeature => StatusCode::BAD_REQUEST,
            ErrorKind::Authentication => StatusCode::UNAUTHORIZED,
            ErrorKind::AccessDenied => StatusCode::FORBIDDEN,
            ErrorKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ErrorKind::Cancelled => StatusCode::REQUEST_TIMEOUT,
            _ => StatusCode::BAD_GATEWAY,
        };
        Self {
            status,
            reply: ErrorReply {
                version: PROTOCOL_VERSION,
                request_id,
                error,
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.reply)).into_response()
    }
}
