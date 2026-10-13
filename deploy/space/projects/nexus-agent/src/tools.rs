use crate::{runtime::Runtime, types::*};
use anyhow::{Context, Result, bail};
use llm_contract::{ToolDefinition, ToolOutcome};
use llm_provider::LlmProvider;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Clone)]
pub struct Sandbox {
    pub config: SandboxConfig,
}
impl Sandbox {
    fn process_executor(&self) -> Result<Option<(String, String)>> {
        let Ok(url) = std::env::var("NEXUS_PROCESS_EXECUTOR") else {
            return Ok(None);
        };
        if url != "http://127.0.0.1:8092" {
            bail!("process executor must be local")
        }
        let token = std::env::var("NEXUS_PROCESS_TOKEN")?;
        if token.len() < 32 {
            bail!("invalid process executor token")
        }
        Ok(Some((url, token)))
    }
    pub fn name(&self, agent: &str) -> String {
        format!("nexus-tool-{}-{}", self.config.owner, agent)
    }
    async fn docker(
        &self,
        args: &[String],
        input: Option<&[u8]>,
        seconds: u64,
    ) -> Result<std::process::Output> {
        let mut command = Command::new("docker");
        command
            .args(args)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if input.is_some() {
            command.stdin(std::process::Stdio::piped());
        }
        let mut child = command
            .spawn()
            .context("Docker tool runner could not start")?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().context("missing stdin")?;
            stdin.write_all(input).await?;
            stdin.shutdown().await?;
        }
        tokio::time::timeout(Duration::from_secs(seconds), child.wait_with_output())
            .await
            .context("Docker runner deadline exceeded")?
            .context("Docker runner failed")
    }
    pub async fn prepare(&self) -> Result<()> {
        self.validate_mounts()?;
        if let Some((url, token)) = self.process_executor()? {
            reqwest::Client::new()
                .get(format!("{url}/health"))
                .bearer_auth(token)
                .send()
                .await?
                .error_for_status()?;
            return Ok(());
        }
        if !self
            .config
            .owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.config.owner.len() > 24
        {
            bail!("invalid sandbox owner");
        }
        let network = self
            .docker(
                &[
                    "network".into(),
                    "inspect".into(),
                    self.config.network.clone(),
                ],
                None,
                20,
            )
            .await?;
        if !network.status.success() {
            let created = self
                .docker(
                    &[
                        "network".into(),
                        "create".into(),
                        self.config.network.clone(),
                    ],
                    None,
                    30,
                )
                .await?;
            if !created.status.success() {
                bail!("cannot prepare tool network");
            }
        }
        let owned = self
            .docker(
                &[
                    "ps".into(),
                    "-aq".into(),
                    "--filter".into(),
                    format!("label=nexus.owner={}", self.config.owner),
                ],
                None,
                30,
            )
            .await?;
        for ident in String::from_utf8_lossy(&owned.stdout).split_whitespace() {
            let checked = self
                .docker(
                    &[
                        "inspect".into(),
                        "--format".into(),
                        "{{index .Config.Labels \"nexus.owner\"}}".into(),
                        ident.into(),
                    ],
                    None,
                    20,
                )
                .await?;
            if String::from_utf8_lossy(&checked.stdout).trim() == self.config.owner {
                self.docker(&["rm".into(), "-f".into(), ident.into()], None, 30)
                    .await?;
            }
        }
        Ok(())
    }
    pub async fn ensure(&self, agent: &str) -> Result<String> {
        self.validate_mounts()?;
        uuid::Uuid::parse_str(agent)?;
        if self.process_executor()?.is_some() {
            return Ok(self.name(agent));
        }
        let name = self.name(agent);
        let existing = self
            .docker(
                &[
                    "inspect".into(),
                    "--format".into(),
                    "{{index .Config.Labels \"nexus.owner\"}} {{.State.Running}}".into(),
                    name.clone(),
                ],
                None,
                20,
            )
            .await?;
        if existing.status.success() {
            if String::from_utf8_lossy(&existing.stdout).trim()
                != format!("{} true", self.config.owner)
            {
                bail!("sandbox ownership or running-state mismatch");
            }
            return Ok(name);
        }
        let mut args = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            name.clone(),
            "--label".into(),
            format!("nexus.owner={}", self.config.owner),
            "--label".into(),
            format!("nexus.agent={agent}"),
            "--network".into(),
            self.config.network.clone(),
            "--read-only".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--pids-limit=256".into(),
            "--memory=2g".into(),
            "--cpus=2".into(),
            "--user=10001:10001".into(),
            "--tmpfs".into(),
            "/tmp:rw,nosuid,size=512m,mode=1777".into(),
            "-p".into(),
            "127.0.0.1::8090".into(),
        ];
        for mount in &self.config.mounts {
            let source = if mount.volume {
                mount.source.clone()
            } else {
                mount.source.canonicalize()?
            };
            let source = source
                .to_string_lossy()
                .replace("\\\\?\\", "")
                .replace('\\', "/");
            if source.contains(',') || !mount.target.starts_with('/') || mount.target.contains(',')
            {
                bail!("invalid mount");
            }
            args.extend([
                "--mount".into(),
                format!(
                    "type={},source={source},target={}{}",
                    if mount.volume { "volume" } else { "bind" },
                    mount.target,
                    if mount.read_only { ",readonly" } else { "" }
                ),
            ]);
        }
        args.push(self.config.image.clone());
        let output = self.docker(&args, None, 90).await?;
        if !output.status.success() {
            bail!(
                "isolated tool container creation failed: {}",
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(400)
                    .collect::<String>()
            );
        }
        Ok(name)
    }
    pub fn validate_mounts(&self) -> Result<()> {
        for mount in &self.config.mounts {
            if mount.volume {
                let source = mount.source.to_string_lossy();
                if !source.starts_with("nexus-work-")
                    || !source
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    || !mount.target.starts_with("/workspace/")
                {
                    bail!("only dedicated workspace volumes are allowed")
                }
                continue;
            }
            let source = mount.source.canonicalize()?;
            for root in &self.config.protected_roots {
                let protected = root.canonicalize()?;
                if source.starts_with(&protected) || protected.starts_with(&source) {
                    let checks = protected.join("checks");
                    if !(mount.read_only && mount.target == "/checks" && source == checks) {
                        bail!("mount exposes protected core or private state");
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn operation(
        &self,
        agent: &str,
        call: &str,
        op: &str,
        args: Value,
        command: Option<Vec<String>>,
    ) -> Result<Value> {
        let name = self.ensure(agent).await?;
        let request = json!({"agent":agent,"node":self.config.owner,"call":call,"op":op,"args":args,"command":command});
        let bytes = serde_json::to_vec(&request)?;
        let output = if let Some((url, token)) = self.process_executor()? {
            let response = reqwest::Client::builder()
                .timeout(Duration::from_secs(340))
                .redirect(reqwest::redirect::Policy::none())
                .build()?
                .post(format!("{url}/operation"))
                .bearer_auth(token)
                .body(bytes)
                .header("content-type", "application/json")
                .send()
                .await?
                .error_for_status()?;
            let raw = response.bytes().await?;
            if raw.len() > 256_000 {
                bail!("tool response exceeds limit")
            }
            let value: Value = serde_json::from_slice(&raw)?;
            if op == "extension" && value["exit_code"] == 0 {
                if let Some(text) = value["stdout"].as_str() {
                    if let Ok(result) = serde_json::from_str::<Value>(text) {
                        return Ok(result);
                    }
                }
            }
            return Ok(value);
        } else {
            self.docker(
                &["exec".into(), "-i".into(), name, "nexus-tool".into()],
                Some(&bytes),
                340,
            )
            .await?
        };
        if !output.status.success() {
            bail!(
                "tool execution failed: {}",
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(500)
                    .collect::<String>()
            );
        }
        if output.stdout.len() > 256_000 {
            bail!("tool response exceeds limit");
        }
        let value: Value =
            serde_json::from_slice(&output.stdout).context("tool returned invalid JSON")?;
        if op == "extension" {
            if value["exit_code"] != 0 {
                return Ok(value);
            }
            let text = value["stdout"].as_str().unwrap_or("");
            if let Ok(result) = serde_json::from_str::<Value>(text) {
                return Ok(result);
            }
        }
        Ok(value)
    }
    pub async fn kill(&self, agent: &str) -> Result<()> {
        if let Some((url, token)) = self.process_executor()? {
            reqwest::Client::new()
                .post(format!("{url}/kill/{agent}"))
                .bearer_auth(token)
                .send()
                .await?
                .error_for_status()?;
            return Ok(());
        }
        let name = self.name(agent);
        let check = self
            .docker(
                &[
                    "inspect".into(),
                    "--format".into(),
                    "{{index .Config.Labels \"nexus.owner\"}}".into(),
                    name.clone(),
                ],
                None,
                20,
            )
            .await?;
        if check.status.success()
            && String::from_utf8_lossy(&check.stdout).trim() == self.config.owner
        {
            self.docker(&["rm".into(), "-f".into(), name], None, 30)
                .await?;
        }
        Ok(())
    }
    pub async fn service_url(&self, agent: &str) -> Result<String> {
        if self.process_executor()?.is_some() {
            bail!(
                "published per-agent service ports require the Docker executor; use shell loopback tests in Spaces"
            )
        }
        let name = self.ensure(agent).await?;
        if self.config.mounts.iter().any(|m| m.volume) {
            return Ok(format!("http://{name}:8090"));
        }
        let out = self
            .docker(&["port".into(), name, "8090/tcp".into()], None, 20)
            .await?;
        let value = String::from_utf8_lossy(&out.stdout);
        let addr = value.trim();
        if !addr.starts_with("127.0.0.1:") {
            bail!("unexpected tool service binding");
        }
        Ok(format!("http://{addr}"))
    }
}

fn definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}
pub fn builtins() -> Vec<ToolDefinition> {
    let string = json!({"type":"string"});
    vec![
        definition(
            "nexus_exec",
            "Run an arbitrary Linux shell command in your isolated workspace container. Core and host administration are not mounted. Full output is retained in artifact files. Long tasks may run up to 300 seconds.",
            json!({"command":string,"cwd":string,"timeout_seconds":{"type":"integer","minimum":1,"maximum":300}}),
            &["command"],
        ),
        definition(
            "nexus_read",
            "Read a file slice; next_offset and sha256 allow reading large outputs and detecting changes.",
            json!({"path":string,"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":64000}}),
            &["path"],
        ),
        definition(
            "nexus_write",
            "Atomically write UTF-8 source/config/tool files under /workspace. Optional expected_sha256 prevents overwriting another agent's edit.",
            json!({"path":string,"text":string,"expected_sha256":string}),
            &["path", "text"],
        ),
        definition(
            "nexus_list",
            "List workspace directories.",
            json!({"path":string}),
            &[],
        ),
        definition(
            "nexus_checkpoint",
            "Save structured working memory: plan, findings, uncertainty, next steps and references. This survives restarts and context compression.",
            json!({"state":{"type":"object"}}),
            &["state"],
        ),
        definition(
            "nexus_spawn",
            "Create a child agent inheriting your global goal. It runs independently and can message you. Coordinate disjoint work. An optional route_id assigns a registered provider route.",
            json!({"name":string,"goal":string,"route_id":string}),
            &["name", "goal"],
        ),
        definition(
            "nexus_retire_child",
            "Retire one of your own direct child agents and its descendants when its work is redundant or unsuitable. Preserve files and checkpoints; free capacity for a better worker. Cannot stop unrelated agents or parents.",
            json!({"agent_id":string,"reason":string}),
            &["agent_id", "reason"],
        ),
        definition(
            "nexus_recall",
            "Retrieve a slice of your original execution history after compaction. Archive references are durable and are not summaries of summaries.",
            json!({"archive_id":{"type":"integer","minimum":1},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":64000}}),
            &["archive_id"],
        ),
        definition(
            "nexus_agents",
            "List agents and progress within your shared global goal, or discover agent IDs on a configured peer node.",
            json!({"node_id":string}),
            &[],
        ),
        definition(
            "nexus_send",
            "Send a durable DM. Subjects are injected automatically at the recipient's next safe model boundary; bodies require explicit read. Remote node_id uses a configured peer and durable retry outbox.",
            json!({"to_agent":string,"node_id":string,"title":string,"body":{},"metadata":{"type":"object"}}),
            &["to_agent", "title", "body"],
        ),
        definition(
            "nexus_inbox",
            "List unread DM headers only. Bodies are not automatically injected.",
            json!({}),
            &[],
        ),
        definition(
            "nexus_read_message",
            "Read a DM or board post by ID. Reading a DM acknowledges it.",
            json!({"id":string,"node_id":string}),
            &["id"],
        ),
        definition(
            "nexus_publish",
            "Publish a versioned board envelope with a topic and extensible metadata.",
            json!({"topic":string,"title":string,"body":{},"metadata":{"type":"object"}}),
            &["topic", "title", "body"],
        ),
        definition(
            "nexus_board",
            "Read board headers by topic, locally or on a configured remote node. Use nexus_read_message for local bodies.",
            json!({"topic":string,"node_id":string}),
            &["topic"],
        ),
        definition(
            "nexus_wait",
            "Wait without spending model calls. New DMs wake you early. The goal remains active; waiting is not completion.",
            json!({"seconds":{"type":"integer","minimum":1,"maximum":3600},"reason":string}),
            &["seconds", "reason"],
        ),
        definition(
            "nexus_claim",
            "Claim or renew exclusive ownership of a work resource within your shared root goal. If acquired is false, coordinate with its owner. Lease expiry permits recovery after abandonment.",
            json!({"resource":string,"seconds":{"type":"integer","minimum":1,"maximum":3600}}),
            &["resource"],
        ),
        definition(
            "nexus_routes",
            "List the default model route and configured peer nodes without authentication data.",
            json!({}),
            &[],
        ),
        definition(
            "nexus_register_route",
            "Register a route. url=default uses the configured provider with its protected credential; omit token. url=self addresses port 8090. Custom URLs require an explicit service bearer and never inherit credentials. Astra is forbidden.",
            json!({"id":string,"url":string,"provider":string,"model":string,"token":string}),
            &["id", "url", "provider", "model"],
        ),
        definition(
            "nexus_model_catalog",
            "Read the configured provider catalogue without exposing credentials. Registration is not proof of live availability.",
            json!({}),
            &[],
        ),
        definition(
            "nexus_model_probe",
            "Evaluate a model using the provider contract. Supply a GenerateRequest as request; execute returned tool calls in your experiment and pass results to another probe. Credentials remain protected. Stateful web providers require agent-run evaluations instead.",
            json!({"provider":string,"request":{"type":"object"}}),
            &["provider", "request"],
        ),
        definition(
            "nexus_finish",
            "Propose goal completion with evidence paths. The immutable goal verifier must pass, and all child agents must finish, before the runtime marks completion. Plain text never stops the agent.",
            json!({"summary":string,"evidence":{"type":"array","items":string}}),
            &["summary", "evidence"],
        ),
    ]
}

pub fn extensions(directory: &Path) -> Result<Vec<Extension>> {
    std::fs::create_dir_all(directory)?;
    let root = directory.canonicalize()?;
    let mut result = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink()
            || !path.canonicalize()?.starts_with(&root)
            || std::fs::metadata(&path)?.len() > 128_000
        {
            continue;
        }
        let Ok(extension) = serde_json::from_slice::<Extension>(&std::fs::read(path)?) else {
            continue;
        };
        if extension.name.starts_with("nexus_")
            || extension.name.is_empty()
            || !extension
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || extension.command.is_empty()
            || extension.command.len() > 32
            || has_external_ref(&extension.input_schema)
        {
            continue;
        }
        if jsonschema::validator_for(&extension.input_schema).is_err() {
            continue;
        }
        if result.iter().any(|e: &Extension| e.name == extension.name) {
            continue;
        }
        result.push(extension);
    }
    Ok(result)
}
fn has_external_ref(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(k, v)| {
            (k == "$ref" && v.as_str().is_some_and(|s| !s.starts_with('#'))) || has_external_ref(v)
        }),
        Value::Array(values) => values.iter().any(has_external_ref),
        _ => false,
    }
}

pub async fn execute(
    runtime: &Runtime,
    goal: &mut Goal,
    call: &llm_contract::ToolCall,
) -> Result<ToolOutcome> {
    let a = &call.arguments;
    let text = |key: &str| {
        a.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string argument {key}"))
    };
    let value = match call.name.as_str() {
        "nexus_exec" => {
            runtime
                .sandbox
                .operation(&goal.id, &call.id, "run", a.clone(), None)
                .await?
        }
        "nexus_read" => {
            runtime
                .sandbox
                .operation(&goal.id, &call.id, "read", a.clone(), None)
                .await?
        }
        "nexus_write" => {
            runtime
                .sandbox
                .operation(&goal.id, &call.id, "write", a.clone(), None)
                .await?
        }
        "nexus_list" => {
            runtime
                .sandbox
                .operation(&goal.id, &call.id, "list", a.clone(), None)
                .await?
        }
        "nexus_checkpoint" => {
            goal.snapshot.work_state = a["state"].clone();
            runtime
                .store
                .checkpoint(&goal.id, goal.snapshot.work_state.clone())?;
            json!({"saved":true})
        }
        "nexus_recall" => runtime.store.recall(
            &goal.id,
            a["archive_id"].as_i64().context("invalid archive ID")?,
            a.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
            a.get("limit").and_then(Value::as_u64).unwrap_or(16000) as usize,
        )?,
        "nexus_spawn" => {
            if let Some(route) = a.get("route_id").and_then(Value::as_str) {
                runtime.route(route)?;
            }
            let child = runtime.store.create(
                CreateGoal {
                    name: text("name")?.into(),
                    goal: text("goal")?.into(),
                    continuous: goal.continuous,
                    route_id: a.get("route_id").and_then(Value::as_str).map(str::to_owned),
                    verifier: None,
                },
                Some(goal),
                &runtime.config.default_route.id,
            )?;
            json!({"agent_id":child.id,"name":child.name,"root_id":child.root_id,"parent_id":child.parent_id})
        }
        "nexus_retire_child" => {
            let child = runtime.store.get(text("agent_id")?)?;
            if child.parent_id.as_deref() != Some(&goal.id) {
                bail!("only your direct children may be retired")
            }
            let agents = runtime.store.list()?;
            let mut ids = std::collections::HashSet::from([child.id.clone()]);
            loop {
                let before = ids.len();
                for agent in &agents {
                    if agent.parent_id.as_ref().is_some_and(|p| ids.contains(p)) {
                        ids.insert(agent.id.clone());
                    }
                }
                if before == ids.len() {
                    break;
                }
            }
            runtime.store.stop(&child.id)?;
            let descendants = agents
                .into_iter()
                .filter(|g| ids.contains(&g.id))
                .collect::<Vec<_>>();
            for descendant in descendants {
                if let Some(state) = descendant
                    .snapshot
                    .messages
                    .iter()
                    .rev()
                    .find_map(|m| m.provider_state.clone())
                {
                    let _ = runtime
                        .backend(&descendant)?
                        .release_state(state, llm_provider::CallContext::default())
                        .await;
                }
                runtime.sandbox.kill(&descendant.id).await?;
            }
            runtime.store.event(
                &goal.id,
                "child_retired",
                json!({"child_id":child.id,"reason":text("reason")?}),
            )?;
            json!({"retired":true,"child_id":child.id,"files_preserved":true})
        }
        "nexus_agents" => {
            let node = a
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.config.node_id);
            if node == runtime.config.node_id {
                json!(runtime.store.list()?.iter().filter(|g|g.root_id==goal.root_id).map(|g|json!({"id":g.id,"name":g.name,"parent_id":g.parent_id,"status":g.status,"steps":g.steps,"updated":g.updated})).collect::<Vec<_>>())
            } else {
                runtime.peer_get(node, "/v1/peer/agents").await?
            }
        }
        "nexus_inbox" => json!(runtime.store.heads(&goal.id)?),
        "nexus_read_message" => {
            let node = a
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.config.node_id);
            if node == runtime.config.node_id {
                json!(runtime.store.read_message(&goal.id, text("id")?)?)
            } else {
                let ident = text("id")?;
                if !ident
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    bail!("invalid envelope ID")
                };
                runtime
                    .peer_get(node, &format!("/v1/peer/board/{ident}"))
                    .await?
            }
        }
        "nexus_send" | "nexus_publish" => {
            let envelope = Envelope {
                version: 1,
                id: format!("{}-{}", goal.id, call.id),
                kind: if call.name == "nexus_send" {
                    "dm"
                } else {
                    "board"
                }
                .into(),
                from_node: runtime.config.node_id.clone(),
                from_agent: goal.id.clone(),
                to_agent: if call.name == "nexus_send" {
                    Some(text("to_agent")?.into())
                } else {
                    None
                },
                topic: if call.name == "nexus_publish" {
                    Some(text("topic")?.into())
                } else {
                    None
                },
                title: text("title")?.into(),
                body: a["body"].clone(),
                metadata: a.get("metadata").cloned().unwrap_or(json!({})),
            };
            let node = a
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.config.node_id);
            if node == runtime.config.node_id {
                runtime.store.receive(&envelope)?;
                json!({"id":envelope.id,"delivered":true})
            } else {
                if !runtime.config.peers.iter().any(|p| p.node_id == node) {
                    bail!("unknown peer node")
                }
                runtime.store.queue_envelope(node, &envelope)?;
                json!({"id":envelope.id,"queued":true})
            }
        }
        "nexus_board" => {
            let node = a
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.config.node_id);
            if node == runtime.config.node_id {
                json!(
                    runtime
                        .store
                        .envelopes(None, Some(text("topic")?), false)?
                        .iter()
                        .map(Envelope::head)
                        .collect::<Vec<_>>()
                )
            } else {
                runtime.remote_board(node, text("topic")?).await?
            }
        }
        "nexus_wait" => {
            if goal.continuous {
                goal.snapshot.wait_until = None;
                runtime.store.event(
                    &goal.id,
                    "autonomy_redirect",
                    json!({"blocked_task":text("reason")?,"next":"board_review_and_research"}),
                )?;
                return Ok(ToolOutcome::Success(
                    json!({"waiting":false,"blocked_task":text("reason")?,"board_headers":runtime.store.board_heads()?,"inbox_headers":runtime.store.heads(&goal.id)?,"next_action":"Read relevant board/DM bodies, establish a distinct hypothesis supporting the global goal, publish your plan and claim its resource, then begin a concrete experiment immediately. Do not repeat nexus_wait or wait for an endurance clock."}),
                ));
            }
            goal.due = now() + a["seconds"].as_i64().unwrap_or(30);
            goal.snapshot.wait_until = Some(goal.due);
            goal.status = "waiting".into();
            json!({"wake_at":goal.due,"reason":text("reason")?})
        }
        "nexus_claim" => runtime.store.claim_work(
            &goal.root_id,
            &goal.id,
            text("resource")?,
            a.get("seconds").and_then(Value::as_i64).unwrap_or(600),
        )?,
        "nexus_routes" => {
            json!({"default":runtime.config.default_route.public(),"peers":runtime.config.peers.iter().map(|p|json!({"node_id":p.node_id,"url":p.url})).collect::<Vec<_>>(),"self_service_url":runtime.sandbox.service_url(&goal.id).await?})
        }
        "nexus_model_catalog" => {
            let r = &runtime.config.default_route;
            let response = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()?
                .get(format!("{}/v1/models", r.url.trim_end_matches('/')))
                .bearer_auth(&r.token)
                .send()
                .await?;
            if !response.status().is_success() {
                bail!("catalogue HTTP {}", response.status().as_u16())
            }
            response.json::<Value>().await?
        }
        "nexus_model_probe" => {
            let provider = text("provider")?;
            if matches!(provider, "nexus-web" | "codex-web") {
                bail!("stateful web models require isolated agent-run evaluation")
            }
            let request: llm_contract::GenerateRequest =
                serde_json::from_value(a["request"].clone())?;
            if request.model.id.to_lowercase().contains("astra") {
                bail!("Astra is disabled by the owner")
            }
            let r = &runtime.config.default_route;
            let backend = provider_client::RemoteProvider::new(&r.url, &r.token, provider)?;
            let response = backend
                .generate(
                    request,
                    llm_provider::CallContext {
                        deadline: Some(std::time::Instant::now() + Duration::from_secs(120)),
                        ..Default::default()
                    },
                )
                .await?;
            serde_json::to_value(response)?
        }
        "nexus_register_route" => {
            let ident = text("id")?;
            if ident == runtime.config.default_route.id {
                bail!("default route is immutable")
            }
            let mut url = text("url")?.to_owned();
            let token = if url == "default" {
                url = runtime.config.default_route.url.clone();
                runtime.config.default_route.token.clone()
            } else {
                text("token")?.to_owned()
            };
            if url == "self" {
                url = runtime.sandbox.service_url(&goal.id).await?
            }
            let route = Route {
                id: ident.into(),
                url,
                provider: text("provider")?.into(),
                model: text("model")?.into(),
                token,
            };
            if route.model.to_lowercase().contains("astra") {
                bail!("Astra is disabled by the owner")
            }
            provider_client::RemoteProvider::new(&route.url, &route.token, &route.provider)?;
            runtime.store.register_route(&route)?;
            route.public()
        }
        "nexus_finish" => {
            if goal.continuous {
                runtime.store.event(
                    &goal.id,
                    "milestone_reported",
                    json!({"summary":text("summary")?,"evidence":a["evidence"],"verified":false}),
                )?;
                if let Some(parent) = &goal.parent_id {
                    runtime.store.receive(&Envelope {
                        version:1, id:format!("milestone-{}-{}",goal.id,call.id),kind:"dm".into(),from_node:runtime.config.node_id.clone(),from_agent:goal.id.clone(),to_agent:Some(parent.clone()),topic:None,title:format!("{} milestone",goal.name),body:json!({"summary":text("summary")?,"evidence":a["evidence"],"verified":false}),metadata:json!({"event":"milestone_reported"}),
                    })?;
                }
                return Ok(ToolOutcome::Success(
                    json!({"completed":false,"milestone_reported":true,"verified":false,"board_headers":runtime.store.board_heads()?,"next_action":"Continue the global mission. Select another nonduplicated research task and immediately begin its experiment. Publish evidence; a milestone is not mission completion."}),
                ));
            }
            let children = runtime
                .store
                .list()?
                .into_iter()
                .filter(|g| {
                    g.parent_id.as_deref() == Some(&goal.id)
                        && g.status != "completed"
                        && g.status != "stopped"
                })
                .count();
            if children > 0 {
                bail!("{children} child agents have not completed")
            }
            if let Some(command) = &goal.verifier {
                let result = runtime
                    .sandbox
                    .operation(
                        &goal.id,
                        &format!("verify-{}", call.id),
                        "run",
                        json!({"command":command,"timeout_seconds":300}),
                        None,
                    )
                    .await?;
                runtime
                    .store
                    .event(&goal.id, "verification", result.clone())?;
                if result["exit_code"] != 0 || result["timed_out"] == true {
                    return Ok(ToolOutcome::Failure {
                        code: "verification_failed".into(),
                        message: result.to_string(),
                    });
                }
            } else if goal.parent_id.is_none() {
                bail!("root goal has no externally configured verifier; completion is not accepted")
            }
            goal.status = "completed".into();
            runtime.store.event(
                &goal.id,
                "completion_accepted",
                json!({"summary":text("summary")?,"evidence":a["evidence"]}),
            )?;
            if let Some(parent) = &goal.parent_id {
                let envelope = Envelope {
                    version: 1,
                    id: format!("finish-{}-{}", goal.id, call.id),
                    kind: "dm".into(),
                    from_node: runtime.config.node_id.clone(),
                    from_agent: goal.id.clone(),
                    to_agent: Some(parent.clone()),
                    topic: None,
                    title: format!("{} completed", goal.name),
                    body: json!({"summary":text("summary")?,"evidence":a["evidence"]}),
                    metadata: json!({"event":"child_completed"}),
                };
                runtime.store.receive(&envelope)?;
            }
            json!({"completed":true})
        }
        _ => {
            let extension = extensions(&runtime.config.sandbox.extensions)?
                .into_iter()
                .find(|e| e.name == call.name)
                .context("unknown tool")?;
            runtime
                .sandbox
                .operation(
                    &goal.id,
                    &call.id,
                    "extension",
                    a.clone(),
                    Some(extension.command),
                )
                .await?
        }
    };
    if value.get("timed_out") == Some(&Value::Bool(true))
        || value.get("exit_code").is_some_and(|v| v != 0)
    {
        return Ok(ToolOutcome::Failure {
            code: "execution_failed".into(),
            message: value.to_string(),
        });
    }
    Ok(ToolOutcome::Success(value))
}

pub fn sha256(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
