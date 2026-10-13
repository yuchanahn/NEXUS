//! HF Spaces supervisor. Core/provider UID 1000, tools UID 10001, immutable root-owned binaries.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("missing {name}"))
}
fn save(path: &str, value: &Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec(value)?)?;
    Ok(())
}
fn child(
    program: &str,
    args: &[&str],
    uid: &str,
    vars: &[(&str, String)],
) -> Result<tokio::process::Child> {
    let mut cmd = Command::new("setpriv");
    cmd.args([
        "--reuid",
        uid,
        "--regid",
        uid,
        "--clear-groups",
        "--no-new-privs",
        "--bounding-set=-all",
        "--inh-caps=-all",
        "--ambient-caps=-all",
        program,
    ])
    .args(args)
    .env_clear()
    .env("PATH", "/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin")
    .env(
        "HOME",
        if uid == "10001" {
            "/workspace/home"
        } else {
            "/home/nexus"
        },
    )
    .env("RUSTUP_HOME", "/usr/local/rustup")
    .env("CARGO_HOME", "/workspace/cargo")
    .stdin(Stdio::null())
    .kill_on_drop(true);
    for (key, value) in vars {
        cmd.env(key, value);
    }
    Ok(cmd.spawn()?)
}

struct Executor {
    token: String,
    slots: tokio::sync::Semaphore,
}
fn authorized(h: &axum::http::HeaderMap, token: &str) -> bool {
    let expected = format!("Bearer {token}");
    let actual = h
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0
}
fn valid_agent(v: &str) -> bool {
    uuid::Uuid::parse_str(v).is_ok()
}
#[cfg(unix)]
fn kill_agent(agent: &str) {
    // Only this UID's processes with the exact inherited agent identity can be signalled.
    use std::os::unix::fs::MetadataExt;
    if !valid_agent(agent) {
        return;
    }
    let needle = format!("NEXUS_AGENT_ID={agent}");
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if entry.metadata().is_ok_and(|m| m.uid() == 10001) {
                let p = entry.path();
                if std::fs::read(p.join("environ"))
                    .is_ok_and(|raw| raw.split(|b| *b == 0).any(|s| s == needle.as_bytes()))
                {
                    if let Some(pid) = entry
                        .file_name()
                        .to_str()
                        .and_then(|s| s.parse::<i32>().ok())
                    {
                        unsafe {
                            libc::kill(pid, libc::SIGKILL);
                        }
                    }
                }
            }
        }
    }
}
#[cfg(not(unix))]
fn kill_agent(_: &str) {}
async fn operation(
    axum::extract::State(e): axum::extract::State<Arc<Executor>>,
    h: axum::http::HeaderMap,
    axum::Json(v): axum::Json<Value>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    use axum::http::StatusCode as S;
    if !authorized(&h, &e.token) {
        return Err(S::UNAUTHORIZED);
    }
    let agent = v["agent"]
        .as_str()
        .filter(|s| valid_agent(s))
        .ok_or(S::BAD_REQUEST)?;
    let _slot = e
        .slots
        .acquire()
        .await
        .map_err(|_| S::SERVICE_UNAVAILABLE)?;
    let mut cmd = Command::new("nexus-tool");
    cmd.env_clear()
        .env("PATH", "/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/workspace/home")
        .env("RUSTUP_HOME", "/usr/local/rustup")
        .env("CARGO_HOME", "/workspace/cargo")
        .env("NEXUS_AGENT_ID", agent)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut process = cmd.spawn().map_err(|_| S::INTERNAL_SERVER_ERROR)?;
    let mut input = process.stdin.take().ok_or(S::INTERNAL_SERVER_ERROR)?;
    input
        .write_all(&serde_json::to_vec(&v).map_err(|_| S::BAD_REQUEST)?)
        .await
        .map_err(|_| S::INTERNAL_SERVER_ERROR)?;
    drop(input);
    let output =
        match tokio::time::timeout(Duration::from_secs(330), process.wait_with_output()).await {
            Ok(Ok(o)) => o,
            _ => {
                kill_agent(agent);
                return Err(S::GATEWAY_TIMEOUT);
            }
        };
    if !output.status.success() {
        return Err(S::UNPROCESSABLE_ENTITY);
    }
    if output.stdout.len() > 256000 {
        return Err(S::PAYLOAD_TOO_LARGE);
    }
    Ok(axum::Json(
        serde_json::from_slice(&output.stdout).map_err(|_| S::INTERNAL_SERVER_ERROR)?,
    ))
}
async fn executor() -> Result<()> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 10001 {
        bail!("executor must use isolated worker UID")
    }
    let state = Arc::new(Executor {
        token: env("NEXUS_PROCESS_TOKEN")?,
        slots: tokio::sync::Semaphore::new(2),
    });
    let app = axum::Router::new()
        .route(
            "/health",
            axum::routing::get(
                |axum::extract::State(e): axum::extract::State<Arc<Executor>>,
                 h: axum::http::HeaderMap| async move {
                    if authorized(&h, &e.token) {
                        Ok(axum::Json(
                            json!({"status":"ready","isolation":"separate-uid-process"}),
                        ))
                    } else {
                        Err(axum::http::StatusCode::UNAUTHORIZED)
                    }
                },
            ),
        )
        .route("/operation", axum::routing::post(operation))
        .route(
            "/kill/{id}",
            axum::routing::post(
                |axum::extract::State(e): axum::extract::State<Arc<Executor>>,
                 h: axum::http::HeaderMap,
                 axum::extract::Path(id): axum::extract::Path<String>| async move {
                    if !authorized(&h, &e.token) {
                        return Err(axum::http::StatusCode::UNAUTHORIZED);
                    }
                    if !valid_agent(&id) {
                        return Err(axum::http::StatusCode::BAD_REQUEST);
                    }
                    kill_agent(&id);
                    Ok(axum::Json(json!({"stopped":true})))
                },
            ),
        )
        .layer(axum::extract::DefaultBodyLimit::max(256000))
        .with_state(state);
    axum::serve(tokio::net::TcpListener::bind("127.0.0.1:8092").await?, app).await?;
    Ok(())
}

struct Archive {
    client: reqwest::Client,
    hf: String,
    dataset: String,
    node: String,
}
impl Archive {
    async fn restore(&self) -> Result<()> {
        // A surviving local DB (and its WAL) is newer than the periodic remote snapshot.
        // Never overwrite it on an ordinary container/process restart.
        if std::path::Path::new("/data/core/agent.sqlite").exists() {
            return Ok(());
        }
        let r = self
            .client
            .get(format!(
                "https://huggingface.co/datasets/{}/resolve/main/nodes/{}/snapshot.json",
                self.dataset, self.node
            ))
            .bearer_auth(&self.hf)
            .send()
            .await?;
        if r.status() == 404 {
            return Ok(());
        }
        let value: Value = r.error_for_status()?.json().await?;
        let db = STANDARD.decode(
            value["database"]
                .as_str()
                .context("missing snapshot database")?,
        )?;
        std::fs::write("/data/core/agent.sqlite", db)?;
        if let Some(work) = value["workspace"].as_str() {
            std::fs::write("/workspace/restore.tar.gz", STANDARD.decode(work)?)?;
            let status = child(
                "tar",
                &[
                    "-xzf",
                    "/workspace/restore.tar.gz",
                    "-C",
                    "/workspace",
                    "--no-same-owner",
                    "--no-same-permissions",
                ],
                "10001",
                &[],
            )?
            .wait()
            .await?;
            if !status.success() {
                bail!("workspace restoration failed")
            }
            std::fs::remove_file("/workspace/restore.tar.gz")?;
        }
        Ok(())
    }
    async fn persist(&self) -> Result<()> {
        let source = rusqlite::Connection::open("/data/core/agent.sqlite")?;
        source.backup("main", "/data/core/snapshot.sqlite", None)?;
        let status = child(
            "tar",
            &[
                "-czf",
                "/workspace/snapshot.tar.gz",
                "--exclude=target",
                "--exclude=.git",
                "--exclude=.nexus-artifacts",
                "--exclude=snapshot.tar.gz",
                "-C",
                "/workspace",
                "work",
                "extensions",
                "projects",
            ],
            "10001",
            &[],
        )?
        .wait()
        .await?;
        if !status.success() {
            bail!("workspace archive failed")
        }
        let work = std::fs::read("/workspace/snapshot.tar.gz")?;
        if work.len() > 25_000_000 {
            bail!("workspace snapshot exceeds 25MB; split large artifacts into separate storage")
        }
        let value = json!({"schema":1,"node":self.node,"saved_at":nexus_agent::types::now(),"database":STANDARD.encode(std::fs::read("/data/core/snapshot.sqlite")?),"workspace":STANDARD.encode(work)});
        let content = STANDARD.encode(serde_json::to_vec(&value)?);
        let header =
            json!({"key":"header","value":{"summary":format!("NEXUS checkpoint {}",self.node)}});
        let file = json!({"key":"file","value":{"path":format!("nodes/{}/snapshot.json",self.node),"encoding":"base64","content":content}});
        self.client
            .post(format!(
                "https://huggingface.co/api/datasets/{}/commit/main",
                self.dataset
            ))
            .bearer_auth(&self.hf)
            .header("content-type", "application/x-ndjson")
            .body(format!("{header}\n{file}\n"))
            .send()
            .await?
            .error_for_status()?;
        std::fs::remove_file("/workspace/snapshot.tar.gz")?;
        eprintln!("NEXUS checkpoint persisted: {}", self.node);
        Ok(())
    }
}
async fn boot() -> Result<()> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        bail!(
            "Space bootstrap requires root to establish distinct core and worker UIDs; refusing shared-UID fallback"
        )
    }
    let node = env("NEXUS_NODE_ID")?;
    if node.is_empty() || !node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        bail!("invalid node id")
    }
    let peers: Value = serde_json::from_str(&env("NEXUS_PEERS")?)?;
    let hf = env("NEXUS_HF_TOKEN")?;
    let admin = env("NEXUS_ADMIN_TOKEN")?;
    let peer = env("NEXUS_PEER_TOKEN")?;
    if admin.len() < 32 || peer.len() < 32 || admin == peer {
        bail!("invalid owner/peer credentials")
    }
    let exec = secret();
    let provider = secret();
    let archive = Archive {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?,
        hf: hf.clone(),
        dataset: env("NEXUS_STATE_DATASET")?,
        node: node.clone(),
    };
    for path in [
        "/data/core",
        "/data/config",
        "/workspace/work",
        "/workspace/extensions",
        "/workspace/home",
        "/workspace/cargo",
    ] {
        std::fs::create_dir_all(path)?;
    }
    Command::new("chown")
        .args(["-R", "10001:10001", "/workspace"])
        .status()
        .await?;
    Command::new("chmod")
        .args(["700", "/data/core", "/data/config"])
        .status()
        .await?;
    archive.restore().await?;
    let model = std::env::var("NEXUS_MODEL").unwrap_or("turboderp/Qwen3.8-27B-exl3".into());
    if model.to_lowercase().contains("astra") {
        bail!("Astra forbidden")
    }
    let upstream =
        std::env::var("NEXUS_UPSTREAM").unwrap_or("https://hermes.ai.unturf.com/v1".into());
    save(
        "/data/config/provider.json",
        &json!({"providers":[{"id":"cloud","protocol":"chat","base_url":upstream,"streaming":true,"models":[{"id":model,"upstream":model,"status":"upstream_verified","tool_protocol":"emulated","tool_status":"provider_round_trip_verified"}]}]}),
    )?;
    save(
        "/data/config/core.json",
        &json!({"node_id":node,"bind":"0.0.0.0:7860","database":"/data/core/agent.sqlite","admin_token":admin,"peer_token":peer,
        "default_route":{"id":"primary","url":"http://127.0.0.1:8080","provider":"cloud","model":model,"token":provider},
        "sandbox":{"image":"space-process","network":"none","owner":node,"mounts":[],"extensions":"/workspace/extensions","protected_roots":[]},"peers":peers,"parallelism":1,"context_chars":48000}),
    )?;
    Command::new("chown")
        .args(["-R", "1000:1000", "/data/core", "/data/config"])
        .status()
        .await?;
    let mut worker = child(
        "nexus-space",
        &["executor"],
        "10001",
        &[("NEXUS_PROCESS_TOKEN", exec.clone())],
    )?;
    let mut upstream_process = child(
        "provider-server",
        &[],
        "1000",
        &[
            ("PROVIDER_CONFIG", "/data/config/provider.json".into()),
            ("PROVIDER_BIND", "127.0.0.1:8080".into()),
            ("PROVIDER_BEARER_TOKEN", provider),
        ],
    )?;
    let http = reqwest::Client::new();
    let mut ready = false;
    for _ in 0..60 {
        if http
            .get("http://127.0.0.1:8092/health")
            .bearer_auth(&exec)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !ready {
        bail!("worker readiness deadline")
    }
    let mut core = child(
        "nexus-agent",
        &["serve", "/data/config/core.json"],
        "1000",
        &[
            ("NEXUS_PROCESS_EXECUTOR", "http://127.0.0.1:8092".into()),
            ("NEXUS_PROCESS_TOKEN", exec),
            ("NEXUS_HF_GATEWAY_TOKEN", hf),
        ],
    )?;
    for _ in 0..60 {
        if http
            .get("http://127.0.0.1:7860/health")
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let inventory: Value = http
        .get("http://127.0.0.1:7860/v1/agents")
        .bearer_auth(&admin)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if inventory["agents"].as_array().is_some_and(|a| a.is_empty()) {
        let goal = env("NEXUS_GOAL")?;
        http.post("http://127.0.0.1:7860/v1/agents")
            .bearer_auth(&admin)
            .json(&json!({"name":format!("Cloud researcher {node}"),"goal":goal,"continuous":true}))
            .send()
            .await?
            .error_for_status()?;
    }
    eprintln!(
        "NEXUS Space node active: {node}; worker isolation: separate UID; snapshot interval: 120s"
    );
    let mut timer = tokio::time::interval(Duration::from_secs(120));
    loop {
        tokio::select! {
            _=timer.tick()=>{if let Err(_)=archive.persist().await{eprintln!("NEXUS checkpoint failed; local state retained, remote durability not confirmed");}},
            result=core.wait()=>{result?; bail!("core exited; Space must restart")},
            result=worker.wait()=>{result?; bail!("worker exited; Space must restart")},
            result=upstream_process.wait()=>{result?; bail!("provider exited; Space must restart")},
            _=nexus_agent::shutdown()=>{let _=archive.persist().await;break}
        }
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("executor") => executor().await,
        _ => boot().await,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_identity_requires_uuid() {
        assert!(valid_agent(&uuid::Uuid::new_v4().to_string()));
        assert!(!valid_agent("../core"));
    }
    #[test]
    fn executor_requires_correct_secret() {
        let mut h = axum::http::HeaderMap::new();
        assert!(!authorized(&h, "long-token"));
        h.insert("authorization", "Bearer long-token".parse().unwrap());
        assert!(authorized(&h, "long-token"));
        assert!(!authorized(&h, "other-token"));
    }
}
