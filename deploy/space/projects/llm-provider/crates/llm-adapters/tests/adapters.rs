use axum::{
    Json, Router,
    response::{Sse, sse::Event},
    routing::post,
};
use llm_adapters::*;
use llm_contract::*;
use llm_provider::{CallContext, LlmProvider, collect_response};
use serde_json::json;
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};

#[tokio::test]
async fn harness_contract_preserves_tool_state_and_releases_it() {
    let app = Router::new()
        .route(
            "/generate",
            post(|Json(request): Json<GenerateRequest>| async move {
                assert_eq!(request.model.id, "upstream");
                let resumed = request.messages.len() > 1;
                if resumed {
                    assert_eq!(
                        request.messages[1].provider_state.as_ref().unwrap().data["job_id"],
                        "fixture-job"
                    );
                    assert!(matches!(
                        request.messages[2].content[0],
                        ContentPart::ToolResult(_)
                    ));
                }
                Json(GenerateResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: if resumed {
                            vec![ContentPart::Text {
                                text: "resumed".into(),
                            }]
                        } else {
                            vec![ContentPart::ToolCall(ToolCall {
                                id: "harness-call".into(),
                                name: "echo".into(),
                                arguments: json!({"text":"x"}),
                            })]
                        },
                        provider_state: if resumed {
                            None
                        } else {
                            Some(ProviderState {
                                provider: "fixture".into(),
                                data: json!({"job_id":"fixture-job","revision":0}),
                            })
                        },
                    },
                    finish_reason: if resumed {
                        FinishReason::Stop
                    } else {
                        FinishReason::ToolCalls
                    },
                    usage: Usage::default(),
                    request_id: None,
                })
            }),
        )
        .route(
            "/release",
            post(|Json(state): Json<ProviderState>| async move {
                assert_eq!(state.provider, "fixture");
                Json(json!({"released":true}))
            }),
        );
    let (base, server) = serve(app).await;
    let adapter = HttpProvider::new(config(base, Protocol::NexusHarness, false)).unwrap();
    let mut request = GenerateRequest::text("local", "test");
    request.tools.push(ToolDefinition {
        name: "echo".into(),
        description: "fixture".into(),
        input_schema: json!({"type":"object"}),
    });
    let first = adapter
        .generate(request.clone(), CallContext::default())
        .await
        .unwrap();
    assert_eq!(first.finish_reason, FinishReason::ToolCalls);
    let state = first.message.provider_state.clone().unwrap();
    request.messages.push(first.message);
    request.messages.push(Message::tool(ToolResult {
        call_id: "harness-call".into(),
        outcome: ToolOutcome::Success(json!({"echo":"x"})),
    }));
    let final_response = adapter
        .generate(request.clone(), CallContext::default())
        .await
        .unwrap();
    assert_eq!(final_response.finish_reason, FinishReason::Stop);
    assert!(
        adapter
            .release_state(state, CallContext::default())
            .await
            .unwrap()
    );
    request.options.temperature = Some(0.5);
    assert_eq!(
        adapter
            .generate(request, CallContext::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedFeature
    );
    server.abort();
}

fn config(base: String, protocol: Protocol, streaming: bool) -> AdapterConfig {
    AdapterConfig {
        allow_http: false,
        id: "fixture".into(),
        protocol,
        base_url: base,
        auth_env: None,
        auth_file: None,
        text_only: false,
        streaming,
        max_tokens_field: "max_tokens".into(),
        models: vec![Model {
            id: "local".into(),
            upstream: "upstream".into(),
            status: "fixture".into(),
            tool_protocol: None,
            tool_status: None,
        }],
        tools: None,
        usage_estimated: false,
    }
}
async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}
#[tokio::test]
async fn emulated_http_round_trip_keeps_execution_outside_provider() {
    let (base, task) = serve(Router::new().route("/chat/completions", post(|Json(v): Json<serde_json::Value>| async move {
        assert!(v.get("tools").is_none());
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        let prompt = v["messages"][0]["content"].as_str().unwrap();
        let nonce = prompt.split("nonce must be \"").nth(1).unwrap().split('"').next().unwrap();
        let reply = if prompt.contains("fixture-receipt") {
            assert!(prompt.contains("tool_result"));
            assert!(prompt.contains("instructions"));
            json!({"version":1,"nonce":nonce,"kind":"final","text":"fixture-receipt"})
        } else { json!({"version":1,"nonce":nonce,"kind":"calls","calls":[{"name":"echo","arguments":{"text":"x"}}]}) };
        Json(json!({"choices":[{"message":{"content":reply.to_string()},"finish_reason":"stop"}]}))
    }))).await;
    let mut cfg = config(base, Protocol::Chat, false);
    cfg.text_only = true;
    cfg.models[0].tool_protocol = Some(ToolProtocol::Emulated);
    cfg.models[0].tool_status = Some("provider_round_trip_verified".into());
    let provider = HttpProvider::new(cfg).unwrap();
    let mut request = GenerateRequest::text("local", "ask");
    request.instructions = Some("fixture instructions".into());
    request.tools = vec![ToolDefinition {
        name: "echo".into(),
        description: "echo".into(),
        input_schema: json!({"type":"object"}),
    }];
    let first = provider
        .generate(request.clone(), CallContext::default())
        .await
        .unwrap();
    let ContentPart::ToolCall(call) = &first.message.content[0] else {
        panic!("missing call")
    };
    let result = ToolResult {
        call_id: call.id.clone(),
        outcome: ToolOutcome::Success(json!({"receipt":"fixture-receipt"})),
    };
    request.messages.push(first.message);
    request.messages.push(Message::tool(result));
    request.tool_choice = ToolChoice::None;
    let final_response = provider
        .generate(request, CallContext::default())
        .await
        .unwrap();
    assert_eq!(final_response.finish_reason, FinishReason::Stop);
    assert_eq!(
        final_response.message.content,
        vec![ContentPart::Text {
            text: "fixture-receipt".into()
        }]
    );
    task.abort();
}

#[tokio::test]
async fn antigravity_preserves_signed_parts_and_call_ids() {
    let signed = json!({"model":"upstream","session_id":"fixture-session","parts":[{"functionCall":{"id":"native-call","name":"echo","args":{}},"thoughtSignature":"opaque-fixture-signature"}]});
    let expected = signed.clone();
    let (base, task) = serve(Router::new().route("/chat/completions", post(move |Json(v):Json<serde_json::Value>| {
        let expected = expected.clone();
        async move {
            if v["messages"].as_array().unwrap().len()>1 {
                assert_eq!(v["messages"][1]["nexus_state"], expected);
                assert_eq!(v["messages"][2]["tool_call_id"], "native-call");
                Json(json!({"choices":[{"message":{"content":"complete"},"finish_reason":"stop"}]}))
            } else {
                Json(json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"native-call","type":"function","function":{"name":"echo","arguments":"{}"}}],"nexus_state":expected},"finish_reason":"tool_calls"}]}))
            }
        }
    }))).await;
    let mut cfg = config(base, Protocol::AntigravityChat, false);
    cfg.models[0].tool_protocol = Some(ToolProtocol::Native);
    let provider = HttpProvider::new(cfg).unwrap();
    let mut request = GenerateRequest::text("local", "ask");
    request.tools = vec![ToolDefinition {
        name: "echo".into(),
        description: "echo".into(),
        input_schema: json!({"type":"object"}),
    }];
    let first = provider
        .generate(request.clone(), CallContext::default())
        .await
        .unwrap();
    assert_eq!(first.message.provider_state.as_ref().unwrap().data, signed);
    request.messages.push(first.message);
    request.messages.push(Message::tool(ToolResult {
        call_id: "native-call".into(),
        outcome: ToolOutcome::Failure {
            code: "fixture_failure".into(),
            message: "external failure".into(),
        },
    }));
    assert_eq!(
        provider
            .generate(request, CallContext::default())
            .await
            .unwrap()
            .finish_reason,
        FinishReason::Stop
    );
    task.abort();
}
#[test]
fn responses_preserve_block_order_and_tool_results() {
    let mut r = GenerateRequest::text("local", "ask");
    r.instructions = Some("system".into());
    r.messages.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentPart::Text {
                text: "before".into(),
            },
            ContentPart::ToolCall(ToolCall {
                id: "call".into(),
                name: "sum".into(),
                arguments: json!({"x":1}),
            }),
            ContentPart::Text {
                text: "after".into(),
            },
        ],
        provider_state: None,
    });
    r.messages.push(Message::tool(ToolResult {
        call_id: "call".into(),
        outcome: ToolOutcome::Success(json!({"answer":1})),
    }));
    let v = responses_request(&r, false).unwrap();
    assert_eq!(v["instructions"], "system");
    assert_eq!(v["input"][1]["content"], "before");
    assert_eq!(v["input"][2]["type"], "function_call");
    assert_eq!(v["input"][3]["content"], "after");
    assert_eq!(v["input"][4]["type"], "function_call_output");
}
#[test]
fn incomplete_or_malformed_results_are_not_success() {
    assert!(chat_response(json!({"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"f","arguments":"{}"}}]},"finish_reason":"length"}]})).is_err());
    assert_eq!(chat_response(json!({"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"f","arguments":"{}"}}]},"finish_reason":"stop"}]})).unwrap().finish_reason, FinishReason::ToolCalls);
    assert!(responses_response(json!({"status":"failed","output":[{"type":"message","content":[{"type":"output_text","text":"partial"}]}]})).is_err());
    assert!(responses_response(json!({"status":"in_progress","output":[]})).is_err());
    assert!(chat_response(json!({"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"f","arguments":"[]"}}]},"finish_reason":"tool_calls"}]})).is_err());
    assert!(
        chat_response(
            json!({"choices":[{"message":{"reasoning_content":"hidden"},"finish_reason":"length"}]})
        )
        .is_err()
    );
}

#[tokio::test]
async fn prompt_transport_is_stateless_plaintext_and_rejects_features() {
    let (base, task) = serve(Router::new().route(
        "/api/generateStream",
        post(|Json(v): Json<serde_json::Value>| async move {
            assert!(v["prompt"] == "hello" || v["prompt"] == "blocked");
            assert_eq!(v["network"], false);
            assert_eq!(v["withoutContext"], true);
            (
                [("content-type", "application/text")],
                if v["prompt"] == "blocked" {
                    "很抱歉地告诉您，网站盗用接口。"
                } else {
                    "ok"
                },
            )
        }),
    ))
    .await;
    let p = HttpProvider::new(config(base, Protocol::Yqcloud, false)).unwrap();
    let response = p
        .generate(
            GenerateRequest::text("local", "hello"),
            CallContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::Unknown);
    assert_eq!(response.usage, Usage::default());
    assert_eq!(
        p.generate(
            GenerateRequest::text("local", "blocked"),
            CallContext::default()
        )
        .await
        .unwrap_err()
        .kind,
        ErrorKind::AccessDenied
    );
    let mut request = GenerateRequest::text("local", "hello");
    request.instructions = Some("keep system".into());
    assert_eq!(
        p.generate(request, CallContext::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedFeature
    );
    task.abort();
}
#[tokio::test]
async fn actual_http_mapping_and_unknown_usage() {
    let (base,task)=serve(Router::new().route("/chat/completions",post(|Json(v):Json<serde_json::Value>|async move{
  assert_eq!(v["model"],"upstream");assert_eq!(v["messages"][0]["content"],"hello");Json(json!({"id":"r","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}))
 }))).await;
    let p = HttpProvider::new(config(base, Protocol::Chat, false)).unwrap();
    let response = p
        .generate(
            GenerateRequest::text("local", "hello"),
            CallContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.usage, Usage::default());
    assert_eq!(response.request_id.as_deref(), Some("r"));
    task.abort();
}
#[tokio::test]
async fn streaming_native_calls_are_structured_not_executed() {
    let frames=vec![json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"sum","arguments":"{\"x\":"}}]},"finish_reason":null}]}).to_string(),json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"tool_calls"}]}).to_string(),"[DONE]".into()];
    let (base, task) = serve(Router::new().route(
        "/chat/completions",
        post(move || {
            let frames = frames.clone();
            async move {
                Sse::new(futures_util::stream::iter(
                    frames
                        .into_iter()
                        .map(|s| Ok::<_, Infallible>(Event::default().data(s))),
                ))
            }
        }),
    ))
    .await;
    let p = HttpProvider::new(config(base, Protocol::Chat, true)).unwrap();
    let mut request = GenerateRequest::text("local", "hello");
    request.tools = vec![ToolDefinition {
        name: "sum".into(),
        description: "sum".into(),
        input_schema: json!({"type":"object"}),
    }];
    let response = collect_response(p.stream(request, CallContext::default()).await.unwrap())
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    let ContentPart::ToolCall(call) = &response.message.content[0] else {
        panic!("expected call")
    };
    assert_eq!(call.arguments, json!({"x":1}));
    task.abort();
}
#[tokio::test]
async fn http_setup_deadline_and_error_redaction() {
    let (base, task) = serve(Router::new().route(
        "/chat/completions",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Json(json!({"secret":"must never appear"}))
        }),
    ))
    .await;
    let p = HttpProvider::new(config(base, Protocol::Chat, false)).unwrap();
    let c = CallContext {
        deadline: Some(Instant::now() + Duration::from_millis(20)),
        ..Default::default()
    };
    let error = p
        .generate(GenerateRequest::text("local", "hello"), c)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(!error.message.contains("secret"));
    task.abort();
    let (base, task) = serve(Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                axum::http::StatusCode::UNAUTHORIZED,
                "personal token must not leak",
            )
        }),
    ))
    .await;
    let p = HttpProvider::new(config(base, Protocol::Chat, false)).unwrap();
    let error = p
        .generate(
            GenerateRequest::text("local", "hello"),
            CallContext::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Authentication);
    assert_eq!(error.provider_code.as_deref(), Some("401"));
    assert!(!error.message.contains("token"));
    task.abort();
}
