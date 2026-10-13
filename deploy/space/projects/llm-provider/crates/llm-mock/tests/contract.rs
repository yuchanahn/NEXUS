use futures_executor::block_on;
use futures_util::StreamExt;
use llm_contract::*;
use llm_mock::MockProvider;
use llm_provider::{CallContext, LlmProvider, collect_response};
use serde_json::json;
use std::time::Instant;

fn request() -> GenerateRequest {
    GenerateRequest::text("test-model", "hello")
}

fn started() -> GenerationEvent {
    GenerationEvent::Started {
        request_id: Some("req-1".into()),
    }
}

fn completed() -> GenerationEvent {
    GenerationEvent::Completed {
        finish_reason: FinishReason::Stop,
        provider_state: None,
    }
}

fn tool_request() -> GenerateRequest {
    let mut request = request();
    request.tools.push(ToolDefinition {
        name: "read_file".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    });
    request
}

fn error_kind(provider: &dyn LlmProvider, request: GenerateRequest) -> ErrorKind {
    block_on(provider.generate(request, CallContext::default()))
        .unwrap_err()
        .kind
}

#[test]
fn dyn_provider_collects_text_and_keeps_unknown_usage() {
    let provider: Box<dyn LlmProvider> = Box::new(MockProvider::text("hello"));
    let response = block_on(provider.generate(request(), CallContext::default())).unwrap();
    assert_eq!(
        response.message.content,
        vec![ContentPart::Text {
            text: "hello".into()
        }]
    );
    assert_eq!(response.usage, Usage::default());
    assert_eq!(response.finish_reason, FinishReason::Stop);
}

#[test]
fn interleaved_text_and_parallel_tool_calls_keep_block_order_and_round_trip() {
    let state = ProviderState {
        provider: "mock".into(),
        data: json!({"opaque": "continuation"}),
    };
    let provider = MockProvider::scripted(vec![
        started(),
        GenerationEvent::TextStarted { index: 0 },
        GenerationEvent::ToolCallStarted {
            index: 1,
            id: "call-1".into(),
            name: "read_file".into(),
        },
        GenerationEvent::ToolArgumentsDelta {
            index: 1,
            fragment: "{\"path\":".into(),
        },
        GenerationEvent::ToolCallStarted {
            index: 2,
            id: "call-2".into(),
            name: "read_file".into(),
        },
        GenerationEvent::ToolArgumentsDelta {
            index: 2,
            fragment: "{\"path\":\"b.rs\"}".into(),
        },
        GenerationEvent::BlockEnded { index: 2 },
        GenerationEvent::TextDelta {
            index: 0,
            text: "Reading files".into(),
        },
        GenerationEvent::ToolArgumentsDelta {
            index: 1,
            fragment: "\"a.rs\"}".into(),
        },
        GenerationEvent::BlockEnded { index: 0 },
        GenerationEvent::BlockEnded { index: 1 },
        GenerationEvent::UsageUpdated {
            usage: Usage {
                input_tokens: Some(10),
                ..Usage::default()
            },
        },
        GenerationEvent::UsageUpdated {
            usage: Usage {
                output_tokens: Some(5),
                ..Usage::default()
            },
        },
        GenerationEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            provider_state: Some(state.clone()),
        },
    ]);
    let response = block_on(provider.generate(tool_request(), CallContext::default())).unwrap();
    assert_eq!(response.request_id.as_deref(), Some("req-1"));
    assert_eq!(response.message.provider_state, Some(state));
    assert_eq!(
        response.usage,
        Usage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            total_tokens: None
        }
    );
    assert_eq!(
        response.message.content,
        vec![
            ContentPart::Text {
                text: "Reading files".into()
            },
            ContentPart::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "a.rs"})
            }),
            ContentPart::ToolCall(ToolCall {
                id: "call-2".into(),
                name: "read_file".into(),
                arguments: json!({"path": "b.rs"})
            }),
        ]
    );
    let mut follow_up = tool_request();
    follow_up.messages.push(response.message);
    // Execution is external. A failed tool result is still a valid model input.
    follow_up.messages.push(Message::tool(ToolResult {
        call_id: "call-2".into(),
        outcome: ToolOutcome::Failure {
            code: "not_found".into(),
            message: "missing file".into(),
        },
    }));
    follow_up.messages.push(Message::tool(ToolResult {
        call_id: "call-1".into(),
        outcome: ToolOutcome::Success(json!("file content")),
    }));
    let encoded = serde_json::to_string(&follow_up).unwrap();
    let decoded: GenerateRequest = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, follow_up);
    assert!(block_on(MockProvider::text("done").generate(decoded, CallContext::default())).is_ok());
}

#[test]
fn unsupported_features_are_not_silently_ignored() {
    let provider = MockProvider::text("unused").with_capabilities(Capabilities {
        streaming: Support::Supported,
        tools: Support::Unsupported,
        ..Capabilities::default()
    });
    assert_eq!(
        error_kind(&provider, tool_request()),
        ErrorKind::UnsupportedFeature
    );
}

#[test]
fn unknown_support_is_not_mistaken_for_unsupported() {
    let provider = MockProvider::text("ok").with_capabilities(Capabilities::default());
    assert!(block_on(provider.generate(tool_request(), CallContext::default())).is_ok());
}

#[test]
fn setup_failure_preserves_classification_and_provider_code() {
    let mut error = ProviderError::new(ErrorKind::RateLimited, "try later");
    error.provider_code = Some("quota_exceeded".into());
    let provider = MockProvider::text("unused").with_setup_error(error.clone());
    assert_eq!(
        block_on(provider.generate(request(), CallContext::default())).unwrap_err(),
        error
    );
}

#[test]
fn cancellation_and_expired_deadlines_fail_before_setup() {
    let provider = MockProvider::text("unused");
    let context = CallContext::default();
    context.cancellation.cancel();
    assert_eq!(
        block_on(provider.generate(request(), context))
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    let context = CallContext {
        deadline: Some(Instant::now()),
        ..CallContext::default()
    };
    assert_eq!(
        block_on(provider.generate(request(), context))
            .unwrap_err()
            .kind,
        ErrorKind::Timeout
    );
}

#[test]
fn cancellation_after_partial_output_is_terminal_and_preserves_prior_events() {
    block_on(async {
        let provider = MockProvider::text("partial");
        let context = CallContext::default();
        let cancellation = context.cancellation.clone();
        let mut stream = provider.stream(request(), context).await.unwrap();
        assert!(matches!(
            stream.next().await,
            Some(GenerationEvent::Started { .. })
        ));
        assert!(matches!(
            stream.next().await,
            Some(GenerationEvent::TextStarted { .. })
        ));
        assert_eq!(
            stream.next().await,
            Some(GenerationEvent::TextDelta {
                index: 0,
                text: "partial".into()
            })
        );
        cancellation.cancel();
        assert_eq!(stream.next().await, Some(GenerationEvent::Cancelled));
        assert_eq!(stream.next().await, None);
    });
}

#[test]
fn failure_after_partial_output_is_not_success() {
    let provider = MockProvider::scripted(vec![
        started(),
        GenerationEvent::TextStarted { index: 0 },
        GenerationEvent::TextDelta {
            index: 0,
            text: "partial".into(),
        },
        GenerationEvent::Failed {
            error: ProviderError::new(ErrorKind::Connection, "connection lost"),
        },
    ]);
    assert_eq!(error_kind(&provider, request()), ErrorKind::Connection);
}

#[test]
fn invalid_streams_never_become_completed_responses() {
    let scripts = vec![
        vec![completed()],
        vec![started()],
        vec![started(), started(), completed()],
        vec![
            started(),
            GenerationEvent::TextDelta {
                index: 0,
                text: "missing start".into(),
            },
            completed(),
        ],
        vec![
            started(),
            GenerationEvent::TextStarted { index: 1 },
            completed(),
        ],
        vec![
            started(),
            GenerationEvent::TextStarted { index: 0 },
            completed(),
        ],
        vec![started(), completed(), completed()],
        vec![
            started(),
            GenerationEvent::ToolCallStarted {
                index: 0,
                id: "c".into(),
                name: "t".into(),
            },
            GenerationEvent::ToolArgumentsDelta {
                index: 0,
                fragment: "{".into(),
            },
            GenerationEvent::BlockEnded { index: 0 },
            completed(),
        ],
        vec![
            started(),
            GenerationEvent::ToolCallStarted {
                index: 0,
                id: "c".into(),
                name: "t".into(),
            },
            GenerationEvent::ToolArgumentsDelta {
                index: 0,
                fragment: "[]".into(),
            },
            GenerationEvent::BlockEnded { index: 0 },
            completed(),
        ],
    ];
    for (index, events) in scripts.into_iter().enumerate() {
        assert_eq!(
            error_kind(&MockProvider::scripted(events), request()),
            ErrorKind::InvalidResponse,
            "script {index}"
        );
    }
}

#[test]
fn history_rejects_orphan_results_duplicate_calls_and_foreign_state() {
    let provider = MockProvider::text("unused");
    let mut orphan = request();
    orphan.messages.push(Message::tool(ToolResult {
        call_id: "missing".into(),
        outcome: ToolOutcome::Success(json!(null)),
    }));
    assert_eq!(error_kind(&provider, orphan), ErrorKind::InvalidRequest);

    let call = ContentPart::ToolCall(ToolCall {
        id: "c".into(),
        name: "read_file".into(),
        arguments: json!({}),
    });
    let mut duplicate = request();
    duplicate.messages.push(Message {
        role: Role::Assistant,
        content: vec![call.clone(), call],
        provider_state: None,
    });
    assert_eq!(error_kind(&provider, duplicate), ErrorKind::InvalidRequest);

    let mut foreign = request();
    foreign.messages[0].provider_state = Some(ProviderState {
        provider: "other".into(),
        data: json!({}),
    });
    assert_eq!(error_kind(&provider, foreign), ErrorKind::InvalidRequest);
}

#[test]
fn unresolved_history_and_undefined_named_choice_are_rejected() {
    let provider = MockProvider::text("unused");
    let mut unresolved = request();
    unresolved.messages.push(Message {
        role: Role::Assistant,
        content: vec![ContentPart::ToolCall(ToolCall {
            id: "c".into(),
            name: "read_file".into(),
            arguments: json!({}),
        })],
        provider_state: None,
    });
    assert_eq!(error_kind(&provider, unresolved), ErrorKind::InvalidRequest);
    let mut named = tool_request();
    named.tool_choice = ToolChoice::Named("undefined".into());
    assert_eq!(error_kind(&provider, named), ErrorKind::InvalidRequest);
}

#[test]
fn cancellation_between_setup_and_first_poll_has_a_started_event() {
    block_on(async {
        let provider = MockProvider::text("unused");
        let context = CallContext::default();
        let cancellation = context.cancellation.clone();
        let mut stream = provider.stream(request(), context).await.unwrap();
        cancellation.cancel();
        assert!(matches!(
            stream.next().await,
            Some(GenerationEvent::Started { .. })
        ));
        assert_eq!(stream.next().await, Some(GenerationEvent::Cancelled));
        assert_eq!(stream.next().await, None);
    });
}

#[test]
fn direct_collector_detects_silent_stream_end() {
    let stream = Box::pin(futures_util::stream::iter(vec![started()]));
    assert_eq!(
        block_on(collect_response(stream)).unwrap_err().kind,
        ErrorKind::InvalidResponse
    );
}
