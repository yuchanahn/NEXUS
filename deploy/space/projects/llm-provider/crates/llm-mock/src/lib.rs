//! Deterministic, network-free provider for contract tests and consumer development.

use futures_core::Stream;
use llm_contract::*;
use llm_provider::{CallContext, EventStream, LlmProvider, ProviderFuture, validate_request};
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

/// Replays the same script per call, without executing tools or retrying.
/// Scripts may intentionally violate the protocol to test consumer validation.
pub struct MockProvider {
    capabilities: Capabilities,
    events: Vec<GenerationEvent>,
    setup_error: Option<ProviderError>,
}

impl MockProvider {
    pub fn scripted(events: Vec<GenerationEvent>) -> Self {
        Self {
            capabilities: Capabilities {
                tool_protocol: ToolProtocol::Native,
                tool_streaming: Support::Supported,
                streaming: Support::Supported,
                tools: Support::Supported,
                required_tool_choice: Support::Supported,
                named_tool_choice: Support::Supported,
                max_output_tokens: Support::Supported,
                temperature: Support::Supported,
            },
            events,
            setup_error: None,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::scripted(vec![
            GenerationEvent::Started { request_id: None },
            GenerationEvent::TextStarted { index: 0 },
            GenerationEvent::TextDelta {
                index: 0,
                text: text.into(),
            },
            GenerationEvent::BlockEnded { index: 0 },
            GenerationEvent::Completed {
                finish_reason: FinishReason::Stop,
                provider_state: None,
            },
        ])
    }

    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_setup_error(mut self, error: ProviderError) -> Self {
        self.setup_error = Some(error);
        self
    }
}

impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn capabilities(&self, _model: &ModelSpec) -> Capabilities {
        self.capabilities.clone()
    }

    fn stream<'a>(
        &'a self,
        request: GenerateRequest,
        context: CallContext,
    ) -> ProviderFuture<'a, EventStream> {
        Box::pin(async move {
            context.check()?;
            validate_request(&request, self.id(), &self.capabilities(&request.model))?;
            if self.capabilities.streaming == Support::Unsupported {
                return Err(ProviderError::new(
                    ErrorKind::UnsupportedFeature,
                    "streaming is unsupported",
                ));
            }
            if let Some(error) = &self.setup_error {
                return Err(error.clone());
            }
            Ok(Box::pin(MockStream {
                events: self.events.clone().into(),
                context,
                started: false,
                terminal: false,
            }) as EventStream)
        })
    }
}

struct MockStream {
    events: VecDeque<GenerationEvent>,
    context: CallContext,
    started: bool,
    terminal: bool,
}

impl Stream for MockStream {
    type Item = GenerationEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if !self.terminal {
            self.context.cancellation.register(cx.waker());
            if let Err(error) = self.context.check() {
                if !self.started {
                    self.started = true;
                    return Poll::Ready(Some(GenerationEvent::Started { request_id: None }));
                }
                self.events.clear();
                self.terminal = true;
                return Poll::Ready(Some(if error.kind == ErrorKind::Cancelled {
                    GenerationEvent::Cancelled
                } else {
                    GenerationEvent::Failed { error }
                }));
            }
        }
        let event = self.events.pop_front();
        if let Some(event) = &event {
            self.started |= matches!(event, GenerationEvent::Started { .. });
            self.terminal |= event.is_terminal();
        }
        Poll::Ready(event)
    }
}
