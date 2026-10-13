use futures_executor::block_on;
use llm_contract::{ContentPart, GenerateRequest};
use llm_mock::MockProvider;
use llm_provider::{CallContext, LlmProvider};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Change the implementation here; the consumer only depends on LlmProvider.
    let provider: Box<dyn LlmProvider> =
        Box::new(MockProvider::text("Hello from the provider contract."));
    let response = block_on(provider.generate(
        GenerateRequest::text("mock-model", "Hello"),
        CallContext::default(),
    ))?;
    for part in response.message.content {
        if let ContentPart::Text { text } = part {
            println!("{text}");
        }
    }
    Ok(())
}
