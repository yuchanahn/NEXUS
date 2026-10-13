//! Incremental SSE framing. A truncated stream never becomes a successful response.
use crate::{Protocol, codec, control, http_error, invalid};
use futures_util::StreamExt;
use llm_contract::*;
use llm_provider::{CallContext, EventStream};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
struct Frames {
    pending: Vec<u8>,
    data: Vec<String>,
}
impl Frames {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > 1024 * 1024 {
            return Err(invalid("SSE line exceeds limit"));
        }
        let mut frames = vec![];
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line = self.pending.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8(line).map_err(|_| invalid("invalid SSE UTF-8"))?;
            if line.is_empty() {
                if !self.data.is_empty() {
                    frames.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).into());
            }
            if self.data.iter().map(String::len).sum::<usize>() > 1024 * 1024 {
                return Err(invalid("SSE frame exceeds limit"));
            }
        }
        Ok(frames)
    }
}
#[derive(Default)]
struct Tool {
    id: String,
    name: String,
    args: String,
    block: Option<u32>,
}
#[derive(Default)]
struct Chat {
    next: u32,
    text: Option<u32>,
    tools: BTreeMap<u64, Tool>,
    finish: Option<FinishReason>,
    request: Option<GenerateRequest>,
}
impl Chat {
    fn accept(
        &mut self,
        data: &str,
        estimated: bool,
    ) -> Result<Vec<GenerationEvent>, ProviderError> {
        let mut events = vec![];
        if data == "[DONE]" {
            let mut reason = self
                .finish
                .take()
                .ok_or_else(|| invalid("SSE ended without finish reason"))?;
            if self.next == 0 {
                return Err(invalid("SSE contains no visible output"));
            }
            if !self.tools.is_empty()
                && matches!(reason, FinishReason::Length | FinishReason::ContentFilter)
            {
                return Err(invalid("tool stream was not completed"));
            }
            let mut calls = Vec::new();
            for tool in self.tools.values() {
                if tool.block.is_none() {
                    return Err(invalid("incomplete streamed tool header"));
                }
                let args: Value = serde_json::from_str(&tool.args)
                    .map_err(|_| invalid("invalid streamed tool JSON"))?;
                if !args.is_object() {
                    return Err(invalid("streamed tool arguments must be an object"));
                }
                calls.push(ToolCall {
                    id: tool.id.clone(),
                    name: tool.name.clone(),
                    arguments: args,
                });
            }
            if let Some(request) = &self.request {
                crate::validate_calls(request, &calls)?;
            }
            if !calls.is_empty() {
                reason = FinishReason::ToolCalls;
            }
            for index in 0..self.next {
                events.push(GenerationEvent::BlockEnded { index });
            }
            events.push(GenerationEvent::Completed {
                finish_reason: reason,
                provider_state: None,
            });
            return Ok(events);
        }
        let v: Value = serde_json::from_str(data).map_err(|_| invalid("invalid SSE JSON"))?;
        if !v["error"].is_null() {
            return Err(invalid("upstream SSE reported an error"));
        }
        if !estimated && v["usage"].is_object() {
            events.push(GenerationEvent::UsageUpdated {
                usage: codec::usage(&v["usage"]),
            });
        }
        let choices = v["choices"]
            .as_array()
            .ok_or_else(|| invalid("missing SSE choices"))?;
        if choices.len() > 1 {
            return Err(invalid("multiple SSE choices unsupported"));
        }
        if let Some(choice) = choices.first() {
            if choice["index"].as_u64().is_some_and(|n| n != 0) {
                return Err(invalid("invalid SSE choice index"));
            }
            let delta = &choice["delta"];
            if self.finish.is_some()
                && (delta["content"].as_str().is_some_and(|s| !s.is_empty())
                    || delta["tool_calls"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty()))
            {
                return Err(invalid("output after SSE finish reason"));
            }
            if let Some(t) = delta["content"].as_str().filter(|s| !s.is_empty()) {
                let index = if let Some(i) = self.text {
                    i
                } else {
                    let i = self.next;
                    self.next += 1;
                    self.text = Some(i);
                    events.push(GenerationEvent::TextStarted { index: i });
                    i
                };
                events.push(GenerationEvent::TextDelta {
                    index,
                    text: t.into(),
                });
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    let key = call["index"]
                        .as_u64()
                        .ok_or_else(|| invalid("missing streamed tool index"))?;
                    let tool = self.tools.entry(key).or_default();
                    if let Some(id) = call["id"].as_str() {
                        if tool.block.is_some() {
                            return Err(invalid("tool ID changed after stream start"));
                        }
                        tool.id.push_str(id);
                    }
                    if let Some(name) = call["function"]["name"].as_str() {
                        if tool.block.is_some() {
                            return Err(invalid("tool name changed after stream start"));
                        }
                        tool.name.push_str(name);
                    }
                    let fragment = call["function"]["arguments"].as_str().unwrap_or("");
                    tool.args.push_str(fragment);
                    if tool.block.is_none()
                        && !tool.id.is_empty()
                        && !tool.name.is_empty()
                        && !tool.args.is_empty()
                    {
                        let index = self.next;
                        self.next += 1;
                        tool.block = Some(index);
                        events.push(GenerationEvent::ToolCallStarted {
                            index,
                            id: tool.id.clone(),
                            name: tool.name.clone(),
                        });
                        events.push(GenerationEvent::ToolArgumentsDelta {
                            index,
                            fragment: tool.args.clone(),
                        });
                    } else if let Some(index) = tool.block
                        && !fragment.is_empty()
                    {
                        events.push(GenerationEvent::ToolArgumentsDelta {
                            index,
                            fragment: fragment.into(),
                        });
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.finish = Some(codec::finish(reason));
            }
        }
        Ok(events)
    }
}
pub(crate) fn events(
    response: reqwest::Response,
    _protocol: Protocol,
    context: CallContext,
    estimated: bool,
    request: GenerateRequest,
) -> EventStream {
    Box::pin(async_stream::stream! {
        yield GenerationEvent::Started{request_id:None};
        let mut bytes=response.bytes_stream();let mut frames=Frames::default();let mut chat=Chat{request:Some(request),..Default::default()};let mut total=0usize;
        loop {
            let next=tokio::select!{biased;e=control(&context)=>{yield if e.kind==ErrorKind::Cancelled{GenerationEvent::Cancelled}else{GenerationEvent::Failed{error:e}};return;},next=bytes.next()=>next};
            let chunk=match next{Some(Ok(chunk))=>chunk,Some(Err(e))=>{yield GenerationEvent::Failed{error:http_error(e)};return;},None=>{yield GenerationEvent::Failed{error:invalid("upstream SSE truncated before terminal event")};return;}};
            total=total.saturating_add(chunk.len());if total>crate::BODY_LIMIT{yield GenerationEvent::Failed{error:invalid("SSE byte limit exceeded")};return;}
            let parsed=match frames.push(&chunk){Ok(parsed)=>parsed,Err(error)=>{yield GenerationEvent::Failed{error};return;}};
            for data in parsed{match chat.accept(&data,estimated){Ok(events)=>{for event in events{let terminal=event.is_terminal();yield event;if terminal{return;}}},Err(error)=>{yield GenerationEvent::Failed{error};return;}}}
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_crlf_multiline_and_fragmented_frames() {
        let mut f = Frames::default();
        let source = "data: 한\r\ndata: 글\r\n\r\n".as_bytes();
        let mut out = vec![];
        for byte in source {
            out.extend(f.push(&[*byte]).unwrap());
        }
        assert_eq!(out, vec!["한\n글"]);
    }
    #[test]
    fn truncation_not_success() {
        let mut c = Chat::default();
        assert!(c.accept("[DONE]", false).is_err());
    }
    #[test]
    fn interleaved_tools_and_strict_arguments() {
        let mut c = Chat::default();
        let first = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"sum","arguments":"{\"x\":"}},{"index":1,"id":"b","function":{"name":"sum","arguments":"{}"}}]},"finish_reason":null}]}"#;
        assert_eq!(c.accept(first, false).unwrap().len(), 4);
        c.accept(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"tool_calls"}]}"#,false).unwrap();
        assert!(
            c.accept("[DONE]", false)
                .unwrap()
                .last()
                .unwrap()
                .is_terminal()
        );
    }
}
