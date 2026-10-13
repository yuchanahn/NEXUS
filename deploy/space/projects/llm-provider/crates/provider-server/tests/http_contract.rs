use futures_util::StreamExt;
use llm_contract::*;
use llm_mock::MockProvider;
use llm_provider::{CallContext, EventStream, LlmProvider, ProviderFuture, collect_response};
use llm_wire::*;
use provider_client::RemoteProvider;
use provider_server::{ServerConfig, ServerState, router};
use serde_json::json;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

const TOKEN: &str = "test-token-not-a-real-secret";

struct Releasable(AtomicBool);
impl LlmProvider for Releasable {
    fn id(&self) -> &str {
        "release-fixture"
    }
    fn capabilities(&self, _: &ModelSpec) -> Capabilities {
        Capabilities::default()
    }
    fn stream<'a>(&'a self, _: GenerateRequest, _: CallContext) -> ProviderFuture<'a, EventStream> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::UnsupportedFeature, "fixture")) })
    }
    fn release_state<'a>(
        &'a self,
        _: ProviderState,
        context: CallContext,
    ) -> ProviderFuture<'a, bool> {
        Box::pin(async move {
            context.check()?;
            Ok(self.0.swap(false, Ordering::SeqCst))
        })
    }
}

#[tokio::test]
async fn remote_release_is_authenticated_idempotent_and_provider_bound() {
    let server = start(
        vec![Arc::new(Releasable(AtomicBool::new(true)))],
        ServerConfig::new(TOKEN),
    )
    .await;
    let client = RemoteProvider::new(&server.url, TOKEN, "release-fixture").unwrap();
    let state = ProviderState {
        provider: "release-fixture".into(),
        data: json!({"job_id":"fixture"}),
    };
    assert!(
        client
            .release_state(state.clone(), CallContext::default())
            .await
            .unwrap()
    );
    assert!(
        !client
            .release_state(state.clone(), CallContext::default())
            .await
            .unwrap()
    );
    let mut foreign = state.clone();
    foreign.provider = "other".into();
    assert_eq!(
        client
            .release_state(foreign, CallContext::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRequest
    );
    let denied = reqwest::Client::new()
        .post(format!("{}/v1/release", server.url))
        .json(&ReleaseRequest {
            version: 1,
            provider: "release-fixture".into(),
            state,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
}

struct TestServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(providers: Vec<Arc<dyn LlmProvider>>, config: ServerConfig) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(ServerState::new(config, providers).unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer { url, task }
}

fn request() -> GenerateRequest {
    GenerateRequest::text("mock-model", "hello")
}

fn envelope(id: &str, provider: &str) -> CallRequest {
    CallRequest {
        version: PROTOCOL_VERSION,
        request_id: id.into(),
        provider: provider.into(),
        request: request(),
        timeout_ms: None,
    }
}

async fn post(
    server: &TestServer,
    route: &str,
    request: &impl serde::Serialize,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}{route}", server.url))
        .bearer_auth(TOKEN)
        .json(request)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn remote_trait_supports_json_and_sse_without_agent_dependencies() {
    let server = start(
        vec![Arc::new(MockProvider::text("안녕"))],
        ServerConfig::new(TOKEN),
    )
    .await;
    let provider: Box<dyn LlmProvider> =
        Box::new(RemoteProvider::new(&server.url, TOKEN, "mock").unwrap());
    let response = provider
        .generate(request(), CallContext::default())
        .await
        .unwrap();
    let streamed = collect_response(
        provider
            .stream(request(), CallContext::default())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response, streamed);
    assert_eq!(
        response.message.content,
        vec![ContentPart::Text {
            text: "안녕".into()
        }]
    );
    let client = RemoteProvider::new(&server.url, TOKEN, "mock").unwrap();
    assert_eq!(
        client.capabilities(&request().model).tools,
        Support::Unknown
    );
    let capabilities = client
        .fetch_capabilities(request().model, CallContext::default())
        .await
        .unwrap();
    assert_eq!(capabilities.tools, Support::Supported);
}

#[tokio::test]
async fn unauthorized_version_and_unknown_provider_are_explicit() {
    let server = start(
        vec![Arc::new(MockProvider::text("ok"))],
        ServerConfig::new(TOKEN),
    )
    .await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/generate", server.url))
        .json(&envelope("no-auth", "mock"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.json::<ErrorReply>().await.unwrap().error.kind,
        ErrorKind::Authentication
    );
    let mut bad = envelope("wrong-version", "mock");
    bad.version = 2;
    assert_eq!(post(&server, "/v1/generate", &bad).await.status(), 400);
    assert_eq!(
        post(
            &server,
            "/v1/generate",
            &envelope("missing-provider", "missing")
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        reqwest::get(format!("{}/health/ready", server.url))
            .await
            .unwrap()
            .status(),
        200
    );
}

struct CountingProvider {
    calls: Arc<AtomicUsize>,
}

impl LlmProvider for CountingProvider {
    fn id(&self) -> &str {
        "counting"
    }
    fn capabilities(&self, model: &ModelSpec) -> Capabilities {
        MockProvider::text("").capabilities(model)
    }
    fn stream<'a>(
        &'a self,
        request: GenerateRequest,
        context: CallContext,
    ) -> ProviderFuture<'a, EventStream> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            MockProvider::text("ok").stream(request, context).await
        })
    }
}

#[tokio::test]
async fn duplicate_ids_never_trigger_a_second_model_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = start(
        vec![Arc::new(CountingProvider {
            calls: calls.clone(),
        })],
        ServerConfig::new(TOKEN),
    )
    .await;
    let request = envelope("same-id", "counting");
    let first = post(&server, "/v1/generate", &request).await;
    assert_eq!(first.status(), 200);
    let _: Reply<GenerateResponse> = first.json().await.unwrap();
    assert_eq!(post(&server, "/v1/generate", &request).await.status(), 409);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct PendingProvider {
    dropped: Arc<AtomicBool>,
}
struct PendingStream {
    started: bool,
    dropped: Arc<AtomicBool>,
}

impl Drop for PendingStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl futures_core::Stream for PendingStream {
    type Item = GenerationEvent;
    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if !self.started {
            self.started = true;
            Poll::Ready(Some(GenerationEvent::Started { request_id: None }))
        } else {
            Poll::Pending
        }
    }
}

impl LlmProvider for PendingProvider {
    fn id(&self) -> &str {
        "pending"
    }
    fn capabilities(&self, model: &ModelSpec) -> Capabilities {
        MockProvider::text("").capabilities(model)
    }
    fn stream<'a>(
        &'a self,
        _request: GenerateRequest,
        _context: CallContext,
    ) -> ProviderFuture<'a, EventStream> {
        Box::pin(async move {
            Ok(Box::pin(PendingStream {
                started: false,
                dropped: self.dropped.clone(),
            }) as EventStream)
        })
    }
}

#[tokio::test]
async fn server_deadline_wakes_pending_upstream_and_releases_it() {
    let dropped = Arc::new(AtomicBool::new(false));
    let server = start(
        vec![Arc::new(PendingProvider {
            dropped: dropped.clone(),
        })],
        ServerConfig::new(TOKEN),
    )
    .await;
    let mut request = envelope("timeout", "pending");
    request.timeout_ms = Some(30);
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        post(&server, "/v1/generate", &request),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 504);
    assert_eq!(
        response.json::<ErrorReply>().await.unwrap().error.kind,
        ErrorKind::Timeout
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn explicit_cancel_terminates_sse_and_frees_concurrency_slot() {
    let dropped = Arc::new(AtomicBool::new(false));
    let mut config = ServerConfig::new(TOKEN);
    config.max_concurrent_calls = 1;
    let server = start(
        vec![
            Arc::new(PendingProvider {
                dropped: dropped.clone(),
            }),
            Arc::new(MockProvider::text("ok")),
        ],
        config,
    )
    .await;
    let response = post(&server, "/v1/stream", &envelope("active", "pending")).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        post(&server, "/v1/generate", &envelope("busy", "mock"))
            .await
            .status(),
        429
    );
    let cancel = CancelRequest {
        version: PROTOCOL_VERSION,
        request_id: "active".into(),
    };
    let reply: Reply<CancelResponse> = post(&server, "/v1/cancel", &cancel)
        .await
        .json()
        .await
        .unwrap();
    assert!(reply.value.signalled);
    let text = tokio::time::timeout(Duration::from_secs(2), response.text())
        .await
        .unwrap()
        .unwrap();
    assert!(text.contains("cancelled"));
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        post(&server, "/v1/generate", &envelope("after-cancel", "mock"))
            .await
            .status(),
        200
    );
    let reply: Reply<CancelResponse> = post(&server, "/v1/cancel", &cancel)
        .await
        .json()
        .await
        .unwrap();
    assert!(!reply.value.signalled);
}

#[tokio::test]
async fn local_client_cancellation_interrupts_a_pending_read() {
    let server = start(
        vec![Arc::new(PendingProvider {
            dropped: Arc::new(AtomicBool::new(false)),
        })],
        ServerConfig::new(TOKEN),
    )
    .await;
    let client = RemoteProvider::new(&server.url, TOKEN, "pending").unwrap();
    let context = CallContext::default();
    let cancellation = context.cancellation.clone();
    let mut stream = client.stream(request(), context).await.unwrap();
    assert!(matches!(
        stream.next().await,
        Some(GenerationEvent::Started { .. })
    ));
    let cancel = tokio::spawn(async move {
        tokio::task::yield_now().await;
        cancellation.cancel();
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap(),
        Some(GenerationEvent::Cancelled)
    );
    assert_eq!(stream.next().await, None);
    cancel.await.unwrap();
}

#[tokio::test]
async fn disconnect_drops_upstream_and_releases_slot() {
    let dropped = Arc::new(AtomicBool::new(false));
    let mut config = ServerConfig::new(TOKEN);
    config.max_concurrent_calls = 1;
    let server = start(
        vec![
            Arc::new(PendingProvider {
                dropped: dropped.clone(),
            }),
            Arc::new(MockProvider::text("ok")),
        ],
        config,
    )
    .await;
    let client = RemoteProvider::new(&server.url, TOKEN, "pending").unwrap();
    let mut stream = client
        .stream(request(), CallContext::default())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await,
        Some(GenerationEvent::Started { .. })
    ));
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        post(
            &server,
            "/v1/generate",
            &envelope("after-disconnect", "mock")
        )
        .await
        .status(),
        200
    );
}

#[tokio::test]
async fn request_and_response_byte_limits_are_enforced() {
    let mut config = ServerConfig::new(TOKEN);
    config.max_response_bytes = 512;
    let server = start(vec![Arc::new(MockProvider::text("x".repeat(2048)))], config).await;
    let response = post(&server, "/v1/generate", &envelope("large-response", "mock")).await;
    assert_eq!(response.status(), 502);
    assert_eq!(
        response.json::<ErrorReply>().await.unwrap().error.kind,
        ErrorKind::InvalidResponse
    );
    let mut huge = envelope("large-request", "mock");
    huge.request.messages = vec![Message::user("x".repeat(MAX_REQUEST_BYTES))];
    assert_eq!(post(&server, "/v1/generate", &huge).await.status(), 413);
}

#[tokio::test]
async fn tools_round_trip_over_http_without_executing_them() {
    let provider = MockProvider::scripted(vec![
        GenerationEvent::Started { request_id: None },
        GenerationEvent::ToolCallStarted {
            index: 0,
            id: "tool-1".into(),
            name: "read_file".into(),
        },
        GenerationEvent::ToolArgumentsDelta {
            index: 0,
            fragment: "{\"path\":\"a.rs\"}".into(),
        },
        GenerationEvent::BlockEnded { index: 0 },
        GenerationEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            provider_state: None,
        },
    ]);
    let server = start(vec![Arc::new(provider)], ServerConfig::new(TOKEN)).await;
    let client = RemoteProvider::new(&server.url, TOKEN, "mock").unwrap();
    let mut request = request();
    request.tools.push(ToolDefinition {
        name: "read_file".into(),
        description: "read file".into(),
        input_schema: json!({"type": "object"}),
    });
    let response = collect_response(
        client
            .stream(request.clone(), CallContext::default())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        response.message.content,
        vec![ContentPart::ToolCall(ToolCall {
            id: "tool-1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "a.rs"})
        })]
    );
    request.messages.push(response.message);
    request.messages.push(Message::tool(ToolResult {
        call_id: "tool-1".into(),
        outcome: ToolOutcome::Success(json!("externally executed")),
    }));
    assert!(
        client
            .generate(request, CallContext::default())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn local_deadline_and_auth_failure_preserve_error_classification() {
    let server = start(
        vec![Arc::new(PendingProvider {
            dropped: Arc::new(AtomicBool::new(false)),
        })],
        ServerConfig::new(TOKEN),
    )
    .await;
    let wrong = RemoteProvider::new(&server.url, "wrong-token-long-enough", "pending").unwrap();
    assert_eq!(
        wrong
            .generate(request(), CallContext::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Authentication
    );
    let client = RemoteProvider::new(&server.url, TOKEN, "pending").unwrap();
    let context = CallContext {
        deadline: Some(Instant::now() + Duration::from_millis(40)),
        ..CallContext::default()
    };
    assert_eq!(
        client.generate(request(), context).await.unwrap_err().kind,
        ErrorKind::Timeout
    );
}

struct BufferedProvider {
    invalid_role: bool,
    text_streaming: bool,
}

impl LlmProvider for BufferedProvider {
    fn id(&self) -> &str {
        "buffered"
    }
    fn capabilities(&self, model: &ModelSpec) -> Capabilities {
        let mut caps = MockProvider::text("").capabilities(model);
        caps.streaming = if self.text_streaming {
            Support::Supported
        } else {
            Support::Unsupported
        };
        caps.tool_streaming = Support::Unsupported;
        caps
    }
    fn stream<'a>(
        &'a self,
        _request: GenerateRequest,
        _context: CallContext,
    ) -> ProviderFuture<'a, EventStream> {
        Box::pin(async {
            Err(ProviderError::new(
                ErrorKind::UnsupportedFeature,
                "streaming unsupported",
            ))
        })
    }
    fn generate<'a>(
        &'a self,
        _request: GenerateRequest,
        _context: CallContext,
    ) -> ProviderFuture<'a, GenerateResponse> {
        Box::pin(async move {
            Ok(GenerateResponse {
                message: Message {
                    role: if self.invalid_role {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    content: vec![ContentPart::Text {
                        text: "buffered".into(),
                    }],
                    provider_state: None,
                },
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
                request_id: None,
            })
        })
    }
}

#[tokio::test]
async fn buffered_only_adapters_work_without_faking_stream_support() {
    let server = start(
        vec![Arc::new(BufferedProvider {
            invalid_role: false,
            text_streaming: false,
        })],
        ServerConfig::new(TOKEN),
    )
    .await;
    let client = RemoteProvider::new(&server.url, TOKEN, "buffered").unwrap();
    assert!(
        client
            .generate(request(), CallContext::default())
            .await
            .is_ok()
    );
    let error = match client.stream(request(), CallContext::default()).await {
        Err(error) => error,
        Ok(_) => panic!("streaming should be rejected"),
    };
    assert_eq!(error.kind, ErrorKind::UnsupportedFeature);
    let caps = client
        .fetch_capabilities(request().model, CallContext::default())
        .await
        .unwrap();
    let client = client.with_capabilities(request().model, caps);
    assert_eq!(
        client.capabilities(&request().model).streaming,
        Support::Unsupported
    );
    assert_eq!(
        client
            .capabilities(&ModelSpec {
                id: "another-model".into()
            })
            .streaming,
        Support::Unknown
    );
}

#[tokio::test]
async fn malformed_buffered_response_is_not_a_success() {
    let server = start(
        vec![Arc::new(BufferedProvider {
            invalid_role: true,
            text_streaming: false,
        })],
        ServerConfig::new(TOKEN),
    )
    .await;
    let client = RemoteProvider::new(&server.url, TOKEN, "buffered").unwrap();
    assert_eq!(
        client
            .generate(request(), CallContext::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidResponse
    );
}
#[tokio::test]
async fn tool_generate_uses_buffered_path_when_only_text_streaming_is_supported() {
    let server = start(
        vec![Arc::new(BufferedProvider {
            invalid_role: false,
            text_streaming: true,
        })],
        ServerConfig::new(TOKEN),
    )
    .await;
    let client = RemoteProvider::new(&server.url, TOKEN, "buffered").unwrap();
    let mut tool_request = request();
    tool_request.tools.push(ToolDefinition {
        name: "echo".into(),
        description: "echo".into(),
        input_schema: json!({"type":"object"}),
    });
    assert_eq!(
        client
            .generate(tool_request.clone(), CallContext::default())
            .await
            .unwrap()
            .finish_reason,
        FinishReason::Stop
    );
    assert!(
        client
            .stream(tool_request, CallContext::default())
            .await
            .is_err()
    );
    let caps = client
        .fetch_capabilities(request().model, CallContext::default())
        .await
        .unwrap();
    assert_eq!(caps.streaming, Support::Supported);
    assert_eq!(caps.tool_streaming, Support::Unsupported);
}
