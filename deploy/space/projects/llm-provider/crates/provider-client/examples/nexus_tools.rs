//! A minimal caller-owned agent loop. The provider never runs the tool callback.
use llm_contract::*;
use llm_provider::{CallContext, LlmProvider};
use provider_client::RemoteProvider;
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("PROVIDER_URL").unwrap_or_else(|_| "http://127.0.0.1:18080".into());
    let token = std::env::var("PROVIDER_BEARER_TOKEN")?;
    let model = std::env::var("NEXUS_MODEL").unwrap_or_else(|_| "chatgpt-web/light".into());
    let provider = RemoteProvider::new(&url, token, "nexus-web")?;
    let capabilities = provider
        .fetch_capabilities(ModelSpec { id: model.clone() }, CallContext::default())
        .await?;
    if capabilities.tools != Support::Supported {
        return Err("provider does not advertise tool support".into());
    }
    let provider = provider.with_capabilities(ModelSpec { id: model.clone() }, capabilities);
    let receipt = uuid::Uuid::new_v4().to_string();
    let mut request = GenerateRequest::text(
        model,
        "Call echo_probe with text NEXUS_A_B and return the exact echo and receipt from the result.",
    );
    request.instructions = Some("Use the declared echo_probe; do not invent results.".into());
    request.tools.push(ToolDefinition { name:"echo_probe".into(), description:"A harmless fixture, executed only by this example agent.".into(), input_schema:json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}) });
    let mut latest_state = None;
    let mut calls = 0;
    for _ in 0..8 {
        let context = CallContext {
            deadline: Some(Instant::now() + Duration::from_secs(110)),
            ..CallContext::default()
        };
        let response = match provider.generate(request.clone(), context).await {
            Ok(response) => response,
            Err(error) => {
                if let Some(state) = latest_state {
                    let _ = provider.release_state(state, CallContext::default()).await;
                }
                return Err(error.into());
            }
        };
        latest_state = response.message.provider_state.clone();
        let pending: Vec<_> = response
            .message
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();
        request.messages.push(response.message.clone());
        if response.finish_reason != FinishReason::ToolCalls {
            let text = response
                .message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if calls == 0 || !text.contains(&receipt) {
                return Err("actual tool result was not verified".into());
            }
            println!(
                "PASS Rust client -> Docker provider -> MCP tool -> external agent -> same model continuation ({calls} tool calls)"
            );
            return Ok(());
        }
        for call in pending {
            if call.name != "echo_probe" {
                if let Some(state) = latest_state {
                    let _ = provider.release_state(state, CallContext::default()).await;
                }
                return Err("undeclared tool requested".into());
            }
            // This is the external agent's fixture executor, not provider code.
            let outcome =
                ToolOutcome::Success(json!({"echo":call.arguments["text"],"receipt":receipt}));
            request.messages.push(Message::tool(ToolResult {
                call_id: call.id,
                outcome,
            }));
            calls += 1;
        }
    }
    if let Some(state) = latest_state {
        let _ = provider.release_state(state, CallContext::default()).await;
    }
    Err("agent example turn limit exceeded".into())
}
