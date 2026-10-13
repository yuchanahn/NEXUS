use crate::{runtime::Runtime, types::*};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(e: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":e.to_string()})),
    )
}
fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let actual = headers
        .get("x-nexus-token")
        .or_else(|| headers.get("authorization"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = if headers.contains_key("x-nexus-token") {
        token.to_string()
    } else {
        format!("Bearer {token}")
    };
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0
}
fn require(headers: &HeaderMap, token: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if authorized(headers, token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        ))
    }
}
pub fn router(runtime: Arc<Runtime>) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"service":"nexus-agent","status":"ready"})) }),
        )
        .route("/v1/agents", get(list).post(create))
        .route("/v1/agents/{id}", get(agent))
        .route("/v1/agents/{id}/events", get(events))
        .route("/v1/agents/{id}/stop", post(stop))
        .route("/v1/agents/{id}/resume", post(resume))
        .route("/v1/messages", post(message))
        .route("/v1/peer/envelopes", post(peer_receive))
        .route("/v1/peer/board", get(peer_board))
        .route("/v1/peer/board/{id}", get(peer_board_body))
        .route("/v1/peer/agents", get(peer_agents))
        .layer(DefaultBodyLimit::max(256_000))
        .with_state(runtime)
}
async fn list(State(r): State<Arc<Runtime>>, headers: HeaderMap) -> ApiResult {
    require(&headers, &r.config.admin_token)?;
    let values=r.store.list().map_err(error)?.iter().map(|g|json!({"id":g.id,"root_id":g.root_id,"parent_id":g.parent_id,"name":g.name,"continuous":g.continuous,"goal":g.goal,"route_id":g.route_id,"status":g.status,"steps":g.steps,"errors":g.errors,"due":g.due,"updated":g.updated})).collect::<Vec<_>>();
    Ok(Json(json!({"node_id":r.config.node_id,"agents":values})))
}
async fn create(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Json(input): Json<CreateGoal>,
) -> ApiResult {
    require(&headers, &r.config.admin_token)?;
    r.route(
        input
            .route_id
            .as_deref()
            .unwrap_or(&r.config.default_route.id),
    )
    .map_err(error)?;
    let goal = r
        .store
        .create(input, None, &r.config.default_route.id)
        .map_err(error)?;
    Ok(Json(
        json!({"agent_id":goal.id,"root_id":goal.root_id,"status":goal.status}),
    ))
}
async fn agent(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    require(&headers, &r.config.admin_token)?;
    Ok(Json(json!(r.store.get(&id).map_err(error)?)))
}
#[derive(Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: i64,
}
async fn events(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<EventQuery>,
) -> ApiResult {
    require(&headers, &r.config.admin_token)?;
    Ok(Json(
        json!({"events":r.store.events(&id,q.after).map_err(error)?}),
    ))
}
async fn stop(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    require(&headers, &r.config.admin_token)?;
    let goals = r.store.list().map_err(error)?;
    let mut affected = std::collections::HashSet::from([id.clone()]);
    loop {
        let before = affected.len();
        for goal in &goals {
            if goal
                .parent_id
                .as_ref()
                .is_some_and(|parent| affected.contains(parent))
            {
                affected.insert(goal.id.clone());
            }
        }
        if affected.len() == before {
            break;
        }
    }
    r.store.stop(&id).map_err(error)?;
    for g in goals {
        if affected.contains(&g.id) {
            if let Some(state) = g
                .snapshot
                .messages
                .iter()
                .rev()
                .find_map(|m| m.provider_state.clone())
                && let Ok(backend) = r.backend(&g)
            {
                let _ = backend
                    .release_state(state, llm_provider::CallContext::default())
                    .await;
            }
            r.sandbox.kill(&g.id).await.map_err(error)?;
        }
    }
    Ok(Json(json!({"stopped":true})))
}
async fn resume(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    require(&headers, &r.config.admin_token)?;
    r.store.resume(&id).map_err(error)?;
    Ok(Json(json!({"resumed":true})))
}
#[derive(Deserialize)]
struct HumanMessage {
    to_agent: String,
    title: String,
    body: Value,
}
async fn message(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Json(input): Json<HumanMessage>,
) -> ApiResult {
    require(&headers, &r.config.admin_token)?;
    let e = Envelope {
        version: 1,
        id: id(),
        kind: "dm".into(),
        from_node: r.config.node_id.clone(),
        from_agent: "human".into(),
        to_agent: Some(input.to_agent),
        topic: None,
        title: input.title,
        body: input.body,
        metadata: json!({"source":"owner"}),
    };
    r.store.receive(&e).map_err(error)?;
    Ok(Json(json!({"message_id":e.id})))
}
async fn peer_receive(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Json(e): Json<Envelope>,
) -> ApiResult {
    require(&headers, &r.config.peer_token)?;
    if e.from_node == r.config.node_id
        || !r.config.peers.iter().any(|p| p.node_id == e.from_node)
        || e.from_agent == "human"
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"untrusted sender node"})),
        ));
    }
    let received = r.store.receive(&e).map_err(error)?;
    Ok(Json(json!({"accepted":true,"new":received})))
}
#[derive(Deserialize)]
struct BoardQuery {
    topic: String,
}
async fn peer_board(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Query(q): Query<BoardQuery>,
) -> ApiResult {
    require(&headers, &r.config.peer_token)?;
    let entries = r
        .store
        .envelopes(None, Some(&q.topic), false)
        .map_err(error)?;
    Ok(Json(
        json!({"entries":entries.iter().map(Envelope::head).collect::<Vec<_>>()}),
    ))
}
async fn peer_board_body(
    State(r): State<Arc<Runtime>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    require(&headers, &r.config.peer_token)?;
    Ok(Json(json!(r.store.read_message("", &id).map_err(error)?)))
}
async fn peer_agents(State(r): State<Arc<Runtime>>, headers: HeaderMap) -> ApiResult {
    require(&headers, &r.config.peer_token)?;
    Ok(Json(
        json!({"node_id":r.config.node_id,"agents":r.store.list().map_err(error)?.iter().map(|g|json!({"id":g.id,"name":g.name,"status":g.status,"root_id":g.root_id,"steps":g.steps})).collect::<Vec<_>>()}),
    ))
}
