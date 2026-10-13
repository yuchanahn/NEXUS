use llm_adapters::{ConfigFile, HttpProvider};
use llm_mock::MockProvider;
use llm_provider::LlmProvider;
use provider_server::{ServerConfig, ServerState, router};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::var("PROVIDER_BEARER_TOKEN").map_err(
        |_| "PROVIDER_BEARER_TOKEN is required (at least 16 printable ASCII characters)",
    )?;
    let bind = std::env::var("PROVIDER_BIND").unwrap_or_else(|_| "127.0.0.1:18080".into());
    let mut config = ServerConfig::new(token);
    let mut providers: Vec<Arc<dyn LlmProvider>> =
        vec![Arc::new(MockProvider::text("NEXUS mock response"))];
    if let Ok(path) = std::env::var("PROVIDER_CONFIG") {
        let file: ConfigFile = serde_json::from_slice(&std::fs::read(path)?)?;
        for adapter in file.providers {
            for model in &adapter.models {
                config.models.push(serde_json::json!({"provider":adapter.id,"id":model.id,"upstream":model.upstream,"status":model.status,"tool_protocol":model.tool_protocol,"tool_status":model.tool_status}));
            }
            providers.push(Arc::new(HttpProvider::new(adapter)?));
        }
    }
    let count = providers.len();
    let state = ServerState::new(config, providers)?;
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("provider-server listening on {bind}; registered providers: {count}");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
