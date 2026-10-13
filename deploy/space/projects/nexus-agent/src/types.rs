use llm_contract::{Message, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub node_id: String,
    pub bind: String,
    pub database: PathBuf,
    pub admin_token: String,
    pub peer_token: String,
    pub default_route: Route,
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub peers: Vec<Peer>,
    #[serde(default = "parallel")]
    pub parallelism: usize,
    #[serde(default = "context_limit")]
    pub context_chars: usize,
}
fn parallel() -> usize {
    2
}
fn context_limit() -> usize {
    48_000
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Peer {
    pub node_id: String,
    pub url: String,
    pub token: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub url: String,
    pub provider: String,
    pub model: String,
    pub token: String,
}
impl Route {
    pub fn public(&self) -> Value {
        json!({"id":self.id,"url":self.url,"provider":self.provider,"model":self.model})
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Mount {
    pub source: PathBuf,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub volume: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub image: String,
    pub network: String,
    pub mounts: Vec<Mount>,
    pub extensions: PathBuf,
    pub owner: String,
    #[serde(default)]
    pub protected_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub root_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub continuous: bool,
    pub global_goal: String,
    pub goal: String,
    pub route_id: String,
    pub status: String,
    pub due: i64,
    pub steps: u64,
    pub errors: u64,
    pub verifier: Option<String>,
    pub snapshot: Snapshot,
    pub created: i64,
    pub updated: i64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub messages: Vec<Message>,
    pub work_state: Value,
    pub summary: Value,
    pub archive_seq: u64,
    pub pinned_tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub inbox_seen: Vec<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub retry_streak: u32,
    #[serde(default)]
    pub wait_until: Option<i64>,
}
impl Snapshot {
    pub fn stateful(&self) -> bool {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == llm_contract::Role::Assistant)
            .is_some_and(|m| m.provider_state.is_some())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub id: String,
    pub kind: String,
    pub from_node: String,
    pub from_agent: String,
    pub to_agent: Option<String>,
    pub topic: Option<String>,
    pub title: String,
    pub body: Value,
    #[serde(default)]
    pub metadata: Value,
}
impl Envelope {
    pub fn head(&self) -> Value {
        json!({"id":self.id,"kind":self.kind,"from_node":self.from_node,"from_agent":self.from_agent,"topic":self.topic,"title":self.title})
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CreateGoal {
    pub name: String,
    pub goal: String,
    #[serde(default)]
    pub continuous: bool,
    #[serde(default)]
    pub route_id: Option<String>,
    #[serde(default)]
    pub verifier: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Extension {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub command: Vec<String>,
}
