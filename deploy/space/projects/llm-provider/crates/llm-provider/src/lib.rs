//! Runtime-independent call boundary and shared protocol validation.

mod collect;
mod validate;

pub use collect::collect_response;
pub use validate::{validate_request, validate_response};

use futures_core::Stream;
use futures_util::task::AtomicWaker;
use llm_contract::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Waker,
    time::Instant,
};

pub type ProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;
pub type EventStream = Pin<Box<dyn Stream<Item = GenerationEvent> + Send + 'static>>;

/// One cancellation handle per call. Clones refer to the same call.
#[derive(Clone, Default)]
pub struct Cancellation {
    inner: Arc<CancellationInner>,
}

#[derive(Default)]
struct CancellationInner {
    cancelled: AtomicBool,
    waker: AtomicWaker,
}

impl Cancellation {
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.waker.wake();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Adapters register the active call's waker before checking cancellation.
    pub fn register(&self, waker: &Waker) {
        self.inner.waker.register(waker);
    }

    /// Runtime-independent notification for the one active waiter on this call.
    pub async fn cancelled(&self) {
        std::future::poll_fn(|cx| {
            self.register(cx.waker());
            if self.is_cancelled() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await
    }
}

#[derive(Clone, Default)]
pub struct CallContext {
    pub cancellation: Cancellation,
    /// Monotonic deadline. Adapters must arrange a timer to wake pending I/O.
    pub deadline: Option<Instant>,
    /// Caller-owned tracing label, not a provider request ID.
    pub trace_id: Option<String>,
}

impl CallContext {
    pub fn check(&self) -> Result<(), ProviderError> {
        if self.cancellation.is_cancelled() {
            return Err(ProviderError::new(ErrorKind::Cancelled, "call cancelled"));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ProviderError::new(
                ErrorKind::Timeout,
                "call deadline exceeded",
            ));
        }
        Ok(())
    }
}

/// Implementations perform one model request, never an agent/tool execution loop.
/// No silent feature downgrade or automatic retry. Validate before sending.
///
/// `stream` returns setup failures as Err. After returning a stream, failures are
/// terminal events. Streams begin with Started and end with one terminal event.
/// Dropping a future/stream must release local resources; cancellation cannot
/// promise that a remote server stops work or billing.
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;

    /// Static/configured knowledge only; no implicit endpoint probe.
    fn capabilities(&self, model: &ModelSpec) -> Capabilities;

    /// Explicitly abandon adapter-owned continuation resources. Stateless
    /// adapters may reject this; releasing an already closed state is idempotent.
    fn release_state<'a>(
        &'a self,
        _state: ProviderState,
        context: CallContext,
    ) -> ProviderFuture<'a, bool> {
        Box::pin(async move {
            context.check()?;
            Err(ProviderError::new(
                ErrorKind::UnsupportedFeature,
                "this adapter has no releasable continuation",
            ))
        })
    }

    fn stream<'a>(
        &'a self,
        request: GenerateRequest,
        context: CallContext,
    ) -> ProviderFuture<'a, EventStream>;

    /// Default implementation consumes and validates the stream. On failure,
    /// partial output is available only through the streaming API.
    /// Non-streaming providers may override this method and reject `stream`.
    fn generate<'a>(
        &'a self,
        request: GenerateRequest,
        context: CallContext,
    ) -> ProviderFuture<'a, GenerateResponse> {
        Box::pin(async move { collect_response(self.stream(request, context).await?).await })
    }
}
