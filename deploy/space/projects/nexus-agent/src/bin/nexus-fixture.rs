//! Deterministic protocol fixture for deployment tests. This is explicitly NOT an LLM.
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::{Value, json};
async fn completion(Json(r): Json<Value>) -> Json<Value> {
    let previous = r["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
        .any(|c| c["function"]["name"] == "nexus_write");
    let (name, args) = if previous {
        (
            "nexus_finish",
            json!({"summary":"Deterministic Rust Docker fixture completed","evidence":["/workspace/work/portable-proof.txt"]}),
        )
    } else {
        (
            "nexus_write",
            json!({"path":"/workspace/work/portable-proof.txt","text":"RUST_DOCKER_RECEIPT"}),
        )
    };
    Json(
        json!({"id":uuid::Uuid::new_v4().to_string(),"object":"chat.completion","model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":uuid::Uuid::new_v4().to_string(),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},"fixture":true}),
    )
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(
        std::env::var("FIXTURE_BIND").unwrap_or("0.0.0.0:8090".into()),
    )
    .await?;
    axum::serve(
        listener,
        Router::new()
            .route("/health", get(|| async { Json(json!({"fixture":true})) }))
            .route("/v1/chat/completions", post(completion)),
    )
    .with_graceful_shutdown(nexus_agent::shutdown())
    .await?;
    Ok(())
}
