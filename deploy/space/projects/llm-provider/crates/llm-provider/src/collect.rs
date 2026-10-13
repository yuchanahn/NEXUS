use crate::EventStream;
use futures_util::StreamExt;
use llm_contract::*;
use std::collections::{BTreeMap, HashSet};

enum Block {
    Text(String),
    Tool {
        id: String,
        name: String,
        raw: String,
    },
    Ended(ContentPart),
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse, message)
}

/// Collects one stream, preserving output order even when block deltas interleave.
/// Providers must end their stream promptly after the terminal event.
pub async fn collect_response(mut stream: EventStream) -> Result<GenerateResponse, ProviderError> {
    let mut started = false;
    let mut request_id = None;
    let mut blocks = BTreeMap::new();
    let mut call_ids = HashSet::new();
    let mut usage = Usage::default();
    let mut completed = None;

    while let Some(event) = stream.next().await {
        if completed.is_some() {
            return Err(invalid("event after terminal completion"));
        }
        if !started && !matches!(event, GenerationEvent::Started { .. }) {
            return Err(invalid("first stream event must be Started"));
        }
        match event {
            GenerationEvent::Started { request_id: id } => {
                if started {
                    return Err(invalid("duplicate Started event"));
                }
                started = true;
                request_id = id;
            }
            GenerationEvent::TextStarted { index } => {
                if index as usize != blocks.len() {
                    return Err(invalid("block starts must have consecutive indices"));
                }
                blocks.insert(index, Block::Text(String::new()));
            }
            GenerationEvent::TextDelta { index, text } => match blocks.get_mut(&index) {
                Some(Block::Text(buffer)) => buffer.push_str(&text),
                _ => {
                    return Err(invalid(
                        "text delta targets an absent, ended, or non-text block",
                    ));
                }
            },
            GenerationEvent::ToolCallStarted { index, id, name } => {
                if index as usize != blocks.len() {
                    return Err(invalid("block starts must have consecutive indices"));
                }
                if id.trim().is_empty() || name.trim().is_empty() || !call_ids.insert(id.clone()) {
                    return Err(invalid("tool call names and unique IDs must be nonempty"));
                }
                blocks.insert(
                    index,
                    Block::Tool {
                        id,
                        name,
                        raw: String::new(),
                    },
                );
            }
            GenerationEvent::ToolArgumentsDelta { index, fragment } => match blocks.get_mut(&index)
            {
                Some(Block::Tool { raw, .. }) => raw.push_str(&fragment),
                _ => {
                    return Err(invalid(
                        "argument delta targets an absent, ended, or non-tool block",
                    ));
                }
            },
            GenerationEvent::BlockEnded { index } => {
                let block = blocks
                    .remove(&index)
                    .ok_or_else(|| invalid("ending an absent block"))?;
                let part = match block {
                    Block::Text(text) => ContentPart::Text { text },
                    Block::Tool { id, name, raw } => {
                        let arguments: serde_json::Value = serde_json::from_str(&raw)
                            .map_err(|_| invalid("tool arguments are not complete valid JSON"))?;
                        if !arguments.is_object() {
                            return Err(invalid("tool arguments must be a JSON object"));
                        }
                        ContentPart::ToolCall(ToolCall {
                            id,
                            name,
                            arguments,
                        })
                    }
                    Block::Ended(_) => return Err(invalid("block ended twice")),
                };
                blocks.insert(index, Block::Ended(part));
            }
            GenerationEvent::UsageUpdated { usage: update } => {
                if update.input_tokens.is_some() {
                    usage.input_tokens = update.input_tokens;
                }
                if update.output_tokens.is_some() {
                    usage.output_tokens = update.output_tokens;
                }
                if update.total_tokens.is_some() {
                    usage.total_tokens = update.total_tokens;
                }
            }
            GenerationEvent::Completed {
                finish_reason,
                provider_state,
            } => {
                if blocks.values().any(|b| !matches!(b, Block::Ended(_))) {
                    return Err(invalid("completion contains unfinished blocks"));
                }
                completed = Some((finish_reason, provider_state));
            }
            GenerationEvent::Failed { error } => return Err(error),
            GenerationEvent::Cancelled => {
                return Err(ProviderError::new(ErrorKind::Cancelled, "call cancelled"));
            }
        }
    }

    let (finish_reason, provider_state) =
        completed.ok_or_else(|| invalid("stream ended without completion"))?;
    let content = blocks
        .into_values()
        .map(|b| match b {
            Block::Ended(part) => part,
            _ => unreachable!("completion checked all blocks"),
        })
        .collect();
    Ok(GenerateResponse {
        message: Message {
            role: Role::Assistant,
            content,
            provider_state,
        },
        finish_reason,
        usage,
        request_id,
    })
}
