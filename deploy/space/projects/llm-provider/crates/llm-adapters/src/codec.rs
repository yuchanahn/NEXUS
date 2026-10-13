use crate::{invalid, unsupported};
use llm_contract::*;
use serde_json::{Value, json};
pub fn plain_messages(r: &GenerateRequest) -> Result<Vec<Value>, ProviderError> {
    let mut out = vec![];
    if let Some(s) = &r.instructions {
        out.push(json!({"role":"system","content":s}));
    }
    for m in &r.messages {
        let mut text = String::new();
        for p in &m.content {
            if let ContentPart::Text { text: t } = p {
                text.push_str(t);
            } else {
                return Err(unsupported("this endpoint does not support tool messages"));
            }
        }
        out.push(json!({"role":match m.role{Role::User=>"user",Role::Assistant=>"assistant",Role::Tool=>return Err(unsupported("tool messages unsupported"))},"content":text}));
    }
    Ok(out)
}
fn result_text(r: &ToolResult) -> String {
    match &r.outcome {
        ToolOutcome::Success(v) => v.to_string(),
        ToolOutcome::Failure { code, message } => {
            json!({"error":{"code":code,"message":message}}).to_string()
        }
    }
}
pub fn chat_request(
    r: &GenerateRequest,
    stream: bool,
    max_field: &str,
) -> Result<Value, ProviderError> {
    let mut messages = vec![];
    if let Some(s) = &r.instructions {
        messages.push(json!({"role":"system","content":s}));
    }
    for m in &r.messages {
        let mut text = String::new();
        let mut calls = vec![];
        for p in &m.content {
            match p{ContentPart::Text{text:t}=>text.push_str(t),ContentPart::ToolCall(t)=>calls.push(json!({"id":t.id,"type":"function","function":{"name":t.name,"arguments":t.arguments.to_string()}})),ContentPart::ToolResult(t)=>messages.push(json!({"role":"tool","tool_call_id":t.call_id,"content":result_text(t)}))}
        }
        if m.role != Role::Tool {
            let mut v = json!({"role":if m.role==Role::User{"user"}else{"assistant"},"content":if text.is_empty()&&!calls.is_empty(){Value::Null}else{text.into()}});
            if !calls.is_empty() {
                v["tool_calls"] = calls.into();
            }
            messages.push(v);
        }
    }
    let mut v = json!({"model":r.model.id,"messages":messages,"stream":stream});
    if let Some(n) = r.options.max_output_tokens {
        v[max_field] = n.into();
    }
    if let Some(t) = r.options.temperature {
        v["temperature"] = json!(t);
    }
    if !r.tools.is_empty() {
        v["tools"]=r.tools.iter().map(|t|json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.input_schema}})).collect::<Vec<_>>().into();
    }
    if !r.tools.is_empty() || r.tool_choice != ToolChoice::Auto {
        v["tool_choice"] = match &r.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Named(n) => json!({"type":"function","function":{"name":n}}),
        };
    }
    Ok(v)
}
pub fn responses_request(r: &GenerateRequest, stream: bool) -> Result<Value, ProviderError> {
    let mut input = vec![];
    for m in &r.messages {
        for p in &m.content {
            match p {
                ContentPart::Text{text}=>input.push(json!({"role":if m.role==Role::User{"user"}else{"assistant"},"content":text})),
                ContentPart::ToolCall(t)=>input.push(json!({"type":"function_call","call_id":t.id,"name":t.name,"arguments":t.arguments.to_string()})),
                ContentPart::ToolResult(t)=>input.push(json!({"type":"function_call_output","call_id":t.call_id,"output":result_text(t)}))
            }
        }
    }
    let mut v = json!({"model":r.model.id,"input":input,"stream":stream,"store":false});
    if let Some(s) = &r.instructions {
        v["instructions"] = s.clone().into();
    }
    if let Some(n) = r.options.max_output_tokens {
        v["max_output_tokens"] = n.into();
    }
    if let Some(t) = r.options.temperature {
        v["temperature"] = json!(t);
    }
    if !r.tools.is_empty() {
        v["tools"]=r.tools.iter().map(|t|json!({"type":"function","name":t.name,"description":t.description,"parameters":t.input_schema})).collect::<Vec<_>>().into();
    }
    if !r.tools.is_empty() || r.tool_choice != ToolChoice::Auto {
        v["tool_choice"] = match &r.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Named(n) => json!({"type":"function","name":n}),
        };
    }
    Ok(v)
}
pub(crate) fn finish(v: &str) -> FinishReason {
    match v {
        "stop" | "completed" | "STOP" => FinishReason::Stop,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "length" | "max_output_tokens" | "MAX_TOKENS" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        "" | "unknown" => FinishReason::Unknown,
        other => FinishReason::Other(other.into()),
    }
}
pub(crate) fn usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v
            .get("prompt_tokens")
            .or_else(|| v.get("input_tokens"))
            .and_then(Value::as_u64),
        output_tokens: v
            .get("completion_tokens")
            .or_else(|| v.get("output_tokens"))
            .and_then(Value::as_u64),
        total_tokens: v.get("total_tokens").and_then(Value::as_u64),
    }
}
fn call(
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
) -> Result<ContentPart, ProviderError> {
    let id = id
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid("missing tool call ID"))?;
    let name = name
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid("missing tool name"))?;
    let arguments = serde_json::from_str::<Value>(arguments.unwrap_or(""))
        .map_err(|_| invalid("invalid tool arguments JSON"))?;
    if !arguments.is_object() {
        return Err(invalid("tool arguments must be an object"));
    }
    Ok(ContentPart::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    }))
}
fn response(
    parts: Vec<ContentPart>,
    mut reason: FinishReason,
    u: Usage,
    id: Option<String>,
) -> Result<GenerateResponse, ProviderError> {
    if parts
        .iter()
        .any(|part| matches!(part, ContentPart::ToolCall(_)))
        && matches!(reason, FinishReason::Length | FinishReason::ContentFilter)
    {
        return Err(invalid("tool call response was not completed"));
    }
    if parts.is_empty() {
        return Err(invalid(
            "upstream response has no visible text or tool calls",
        ));
    }
    if parts
        .iter()
        .any(|part| matches!(part, ContentPart::ToolCall(_)))
    {
        reason = FinishReason::ToolCalls;
    }
    Ok(GenerateResponse {
        message: Message {
            role: Role::Assistant,
            content: parts,
            provider_state: None,
        },
        finish_reason: reason,
        usage: u,
        request_id: id,
    })
}
pub fn chat_response(v: Value) -> Result<GenerateResponse, ProviderError> {
    let choice = v
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| invalid("missing chat choice"))?;
    let m = &choice["message"];
    let mut parts = vec![];
    if let Some(t) = m["content"].as_str().filter(|s| !s.is_empty()) {
        parts.push(ContentPart::Text { text: t.into() });
    }
    if let Some(calls) = m["tool_calls"].as_array() {
        for t in calls {
            parts.push(call(
                t["id"].as_str(),
                t["function"]["name"].as_str(),
                t["function"]["arguments"].as_str(),
            )?);
        }
    }
    response(
        parts,
        finish(choice["finish_reason"].as_str().unwrap_or("")),
        usage(&v["usage"]),
        v["id"].as_str().map(str::to_owned),
    )
}
pub fn responses_response(v: Value) -> Result<GenerateResponse, ProviderError> {
    let status = v["status"].as_str().unwrap_or("");
    if !matches!(status, "completed" | "incomplete") {
        return Err(invalid(
            "Responses generation did not reach a valid terminal status",
        ));
    }
    let mut parts = vec![];
    for item in v["output"]
        .as_array()
        .ok_or_else(|| invalid("missing Responses output"))?
    {
        match item["type"].as_str() {
            Some("message") => {
                for p in item["content"]
                    .as_array()
                    .ok_or_else(|| invalid("invalid message content"))?
                {
                    if p["type"] == "output_text"
                        && let Some(t) = p["text"].as_str().filter(|s| !s.is_empty())
                    {
                        parts.push(ContentPart::Text { text: t.into() });
                    }
                }
            }
            Some("function_call") => parts.push(call(
                item["call_id"].as_str(),
                item["name"].as_str(),
                item["arguments"].as_str(),
            )?),
            Some("reasoning") => {}
            _ => return Err(unsupported("unsupported Responses output item")),
        }
    }
    let reason = if parts.iter().any(|p| matches!(p, ContentPart::ToolCall(_))) {
        FinishReason::ToolCalls
    } else if status == "incomplete" {
        finish(v["incomplete_details"]["reason"].as_str().unwrap_or(""))
    } else {
        finish(status)
    };
    response(
        parts,
        reason,
        usage(&v["usage"]),
        v["id"].as_str().map(str::to_owned),
    )
}
pub(crate) fn grok_response(v: Value) -> Result<GenerateResponse, ProviderError> {
    if v["status"] != "succeeded" {
        return Err(ProviderError::new(
            match v["status"].as_str() {
                Some("rate_limited") => ErrorKind::RateLimited,
                Some("403") => ErrorKind::AccessDenied,
                _ => ErrorKind::ProviderInternal,
            },
            "Grok Space upstream generation failed",
        ));
    }
    response(
        v["output_text"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|t| vec![ContentPart::Text { text: t.into() }])
            .unwrap_or_default(),
        FinishReason::Stop,
        Usage::default(),
        v["debug_request_id"].as_str().map(str::to_owned),
    )
}
