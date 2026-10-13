use llm_contract::{ContentPart, GenerateRequest};
use llm_provider::{CallContext, LlmProvider, collect_response};
use provider_client::RemoteProvider;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::var("PROVIDER_URL").unwrap_or_else(|_| "http://127.0.0.1:18080".into());
    let token = std::env::var("PROVIDER_BEARER_TOKEN")?;
    let client = RemoteProvider::new(&base, token, "mock")?;
    let request = GenerateRequest::text("mock-model", "hello");
    let response = collect_response(client.stream(request, CallContext::default()).await?).await?;
    for part in response.message.content {
        if let ContentPart::Text { text } = part {
            println!("{text}");
        }
    }
    Ok(())
}
