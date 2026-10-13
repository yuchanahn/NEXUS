use llm_contract::*;
use std::collections::{HashMap, HashSet};

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidRequest, message)
}

fn require(support: Support, feature: &str) -> Result<(), ProviderError> {
    if support == Support::Unsupported {
        return Err(ProviderError::new(
            ErrorKind::UnsupportedFeature,
            format!("unsupported feature: {feature}"),
        ));
    }
    // Unknown may be attempted, but the adapter must not silently omit it.
    Ok(())
}

/// Semantic envelope checks for buffered transports and non-streaming adapters.
pub fn validate_response(
    response: &GenerateResponse,
    provider_id: &str,
) -> Result<(), ProviderError> {
    let invalid = |message| ProviderError::new(ErrorKind::InvalidResponse, message);
    if response.message.role != Role::Assistant {
        return Err(invalid("generated message must have assistant role"));
    }
    if response
        .message
        .provider_state
        .as_ref()
        .is_some_and(|state| state.provider != provider_id)
    {
        return Err(invalid(
            "response continuation state belongs to another adapter",
        ));
    }
    let mut ids = HashSet::new();
    for part in &response.message.content {
        match part {
            ContentPart::ToolCall(call) => {
                if call.id.trim().is_empty()
                    || call.name.trim().is_empty()
                    || !call.arguments.is_object()
                    || !ids.insert(call.id.as_str())
                {
                    return Err(invalid("invalid completed tool call"));
                }
            }
            ContentPart::ToolResult(_) => {
                return Err(invalid(
                    "model output cannot contain a tool execution result",
                ));
            }
            ContentPart::Text { .. } => {}
        }
    }
    Ok(())
}

/// Checks the common envelope and history, not tool schemas or authorization.
pub fn validate_request(
    request: &GenerateRequest,
    provider_id: &str,
    capabilities: &Capabilities,
) -> Result<(), ProviderError> {
    if request.model.id.trim().is_empty() {
        return Err(invalid("model ID is empty"));
    }
    if request.options.max_output_tokens == Some(0) {
        return Err(invalid("max_output_tokens must be positive"));
    }
    if request
        .options
        .temperature
        .is_some_and(|t| !t.is_finite() || t < 0.0)
    {
        return Err(invalid("temperature must be finite and nonnegative"));
    }
    if request.options.max_output_tokens.is_some() {
        require(capabilities.max_output_tokens, "max_output_tokens")?;
    }
    if request.options.temperature.is_some() {
        require(capabilities.temperature, "temperature")?;
    }
    let mut tools = HashSet::new();
    for tool in &request.tools {
        if tool.name.trim().is_empty() || !tools.insert(tool.name.as_str()) {
            return Err(invalid("tool names must be nonempty and unique"));
        }
        if !tool.input_schema.is_object() {
            return Err(invalid("tool input_schema must be a JSON object"));
        }
    }
    if !request.tools.is_empty() {
        require(capabilities.tools, "tools")?;
    }
    match &request.tool_choice {
        ToolChoice::Required => {
            if tools.is_empty() {
                return Err(invalid("required tool choice needs tool definitions"));
            }
            require(capabilities.required_tool_choice, "required tool choice")?;
        }
        ToolChoice::Named(name) => {
            if !tools.contains(name.as_str()) {
                return Err(invalid("named tool choice must reference a defined tool"));
            }
            require(capabilities.named_tool_choice, "named tool choice")?;
        }
        _ => {}
    }

    let mut calls = HashMap::new();
    let mut resolved = HashSet::new();
    for message in &request.messages {
        if message
            .provider_state
            .as_ref()
            .is_some_and(|s| s.provider != provider_id)
        {
            return Err(invalid(
                "provider continuation state belongs to another adapter",
            ));
        }
        for part in &message.content {
            match part {
                ContentPart::ToolCall(call) => {
                    require(capabilities.tools, "tool history")?;
                    if message.role != Role::Assistant
                        || call.id.trim().is_empty()
                        || call.name.trim().is_empty()
                        || !call.arguments.is_object()
                        || calls.insert(call.id.as_str(), call.name.as_str()).is_some()
                    {
                        return Err(invalid("invalid or duplicate assistant tool call"));
                    }
                }
                ContentPart::ToolResult(result) => {
                    require(capabilities.tools, "tool history")?;
                    if message.role != Role::Tool
                        || !calls.contains_key(result.call_id.as_str())
                        || !resolved.insert(result.call_id.as_str())
                    {
                        return Err(invalid(
                            "tool result must resolve a preceding call exactly once",
                        ));
                    }
                }
                ContentPart::Text { .. } if message.role == Role::Tool => {
                    return Err(invalid("tool messages must contain ToolResult parts"));
                }
                _ => {}
            }
        }
    }
    if calls.len() != resolved.len() {
        return Err(invalid("tool history contains unresolved calls"));
    }
    Ok(())
}
