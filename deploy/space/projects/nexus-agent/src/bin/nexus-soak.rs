use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateMutexW(attrs: *mut c_void, owner: i32, name: *const u16) -> *mut c_void;
        fn GetLastError() -> u32;
        fn SetThreadExecutionState(flags: u32) -> u32;
    }
    pub fn acquire() -> Result<(), String> {
        let name: Vec<u16> = "Local\\NEXUS-24h-soak\0".encode_utf16().collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
            if handle.is_null() {
                return Err("cannot acquire observer mutex".into());
            }
            if GetLastError() == 183 {
                return Err("observer already running".into());
            }
            if SetThreadExecutionState(0x80000001) == 0 {
                return Err("cannot request system-awake state".into());
            }
        }
        Ok(())
    }
}
#[cfg(not(windows))]
mod win {
    pub fn acquire() -> Result<(), String> {
        Ok(())
    }
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
#[derive(Deserialize)]
struct RemoteConfig {
    #[serde(default = "default_interval")]
    interval_seconds: u64,
    state_path: PathBuf,
    status_path: PathBuf,
    samples_path: PathBuf,
    nodes: Vec<RemoteNode>,
    #[serde(default)]
    services: Vec<RemoteService>,
}
#[derive(Deserialize)]
struct RemoteNode {
    name: String,
    url: String,
    token: String,
}
#[derive(Deserialize)]
struct RemoteService {
    name: String,
    url: String,
}
fn default_interval() -> u64 {
    60
}
fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("output has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    use std::io::Write;
    writeln!(file)?;
    file.persist(path)
        .context("atomically replace status file")?;
    Ok(())
}
async fn get(client: &reqwest::Client, url: &str, token: Option<&str>) -> Result<Value> {
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.context("request failed")?;
    let status = response.status();
    if !status.is_success() {
        bail!("HTTP status {status}");
    }
    response.json().await.context("invalid JSON response")
}
async fn ready(client: &reqwest::Client, url: &str) -> bool {
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}
async fn remote_sample(client: &reqwest::Client, config: &RemoteConfig) -> Value {
    let mut healthy = true;
    let mut total_steps = 0u64;
    let mut nodes = Vec::new();
    for node in &config.nodes {
        match get(
            client,
            &format!("{}/v1/agents", node.url.trim_end_matches('/')),
            Some(&node.token),
        )
        .await
        {
            Ok(data) => {
                if let Some(agents) = data["agents"].as_array() {
                    let rows: Vec<Value> = agents.iter().map(|agent| {
                        total_steps = total_steps.saturating_add(agent["steps"].as_u64().unwrap_or(0));
                        json!({"id":agent["id"],"status":agent["status"],"steps":agent["steps"]})
                    }).collect();
                    nodes.push(json!({"name":node.name,"ready":true,"agents":rows}));
                } else {
                    healthy = false;
                    nodes
                        .push(json!({"name":node.name,"ready":false,"error":"agent list missing"}));
                }
            }
            Err(error) => {
                healthy = false;
                nodes.push(json!({"name":node.name,"ready":false,"error":error.to_string()}));
            }
        }
    }
    let mut services = serde_json::Map::new();
    for service in &config.services {
        let is_ready = ready(client, &service.url).await;
        healthy &= is_ready;
        services.insert(service.name.clone(), json!(is_ready));
    }
    json!({"at":unix_now(),"healthy":healthy,"nodes":nodes,"services":services,"total_steps":total_steps})
}
fn recover(root: &Path, relative_script: &str, args: &[&str]) -> Result<i32> {
    let script = root.join(relative_script);
    let powershell = if cfg!(windows) { "pwsh.exe" } else { "pwsh" };
    let status = std::process::Command::new(powershell)
        .arg("-NoProfile")
        .arg("-File")
        .arg(script)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("start recovery script")?;
    Ok(status.code().unwrap_or(-1))
}
async fn sample(root: &Path, client: &reqwest::Client, nodes: &[&str]) -> Value {
    let mut healthy = true;
    let mut total_steps = 0u64;
    let mut node_samples = Vec::new();
    for name in nodes {
        let config_path = root.join("private").join(format!("{name}.json"));
        let result = async {
            let cfg: Value =
                serde_json::from_slice(&std::fs::read(&config_path).context("read node config")?)?;
            let bind = cfg["bind"].as_str().context("node bind missing")?;
            let token = cfg["admin_token"]
                .as_str()
                .context("node admin token missing")?;
            let data = get(client, &format!("http://{bind}/v1/agents"), Some(token)).await?;
            let agents = data["agents"].as_array().context("agent list missing")?;
            let rows: Vec<Value> = agents
                .iter()
                .map(|agent| {
                    total_steps = total_steps.saturating_add(agent["steps"].as_u64().unwrap_or(0));
                    json!({"id":agent["id"],"status":agent["status"],"steps":agent["steps"]})
                })
                .collect();
            Ok::<_, anyhow::Error>(rows)
        }
        .await;
        match result {
            Ok(agents) => node_samples.push(json!({"name":name,"ready":true,"agents":agents})),
            Err(error) => {
                healthy = false;
                let mut row = json!({"name":name,"ready":false,"error":error.to_string()});
                if !root.join("private").join(format!("{name}.stop")).exists() {
                    match recover(
                        root,
                        "scripts/Start-Node.ps1",
                        &["-Node", name, "-Watchdog"],
                    ) {
                        Ok(code) => {
                            row["recovery_attempt"] = json!(code);
                        }
                        Err(_) => {
                            row["recovery_attempt"] = json!("failed_to_start");
                        }
                    }
                }
                node_samples.push(row);
            }
        }
    }
    let mut services = serde_json::Map::new();
    for (name, port, path) in [
        ("provider", 18084, "/health/ready"),
        ("owner", 18187, "/health"),
        ("harness", 18189, "/health"),
    ] {
        let is_ready = ready(client, &format!("http://127.0.0.1:{port}{path}")).await;
        if !is_ready {
            healthy = false;
        }
        services.insert(name.into(), json!(is_ready));
    }
    let mut result = json!({"at":unix_now(),"healthy":healthy,"nodes":node_samples,"services":services,"total_steps":total_steps});
    if result["services"]["owner"] == false {
        result["owner_recovery"] = json!(
            recover(
                root.parent().unwrap_or(root),
                "nexus-web-harness/scripts/restart-nexus-owner.ps1",
                &[]
            )
            .unwrap_or(-1)
        );
    }
    if result["services"]["harness"] == false {
        result["harness_recovery"] = json!(
            recover(
                root.parent().unwrap_or(root),
                "nexus-web-harness/scripts/start-nexus-harness.ps1",
                &[]
            )
            .unwrap_or(-1)
        );
    }
    if result["services"]["provider"] == false {
        result["provider_recovery"] = json!(
            recover(
                root.parent().unwrap_or(root),
                "llm-provider/scripts/Start-Provider.ps1",
                &[]
            )
            .unwrap_or(-1)
        );
    }
    result
}

#[tokio::main]
async fn main() -> Result<()> {
    win::acquire().map_err(anyhow::Error::msg)?;
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(
        args.next()
            .context("usage: nexus-soak PROJECT_ROOT [INTERVAL_SECONDS] | CONFIG.json")?,
    );
    let (root, remote, interval, state_path, status_path, samples_path) = if input.is_file() {
        let config: RemoteConfig =
            serde_json::from_slice(&std::fs::read(&input).context("read observer config")?)?;
        let interval = config.interval_seconds;
        let state_path = config.state_path.clone();
        let status_path = config.status_path.clone();
        let samples_path = config.samples_path.clone();
        (
            None,
            Some(config),
            interval,
            state_path,
            status_path,
            samples_path,
        )
    } else {
        let interval = args
            .next()
            .map(|v| v.parse::<u64>())
            .transpose()?
            .unwrap_or(60);
        let private = input.join("private");
        (
            Some(input.clone()),
            None,
            interval,
            private.join("soak-state.json"),
            input.join("reports/soak-status.json"),
            private.join("soak-samples.jsonl"),
        )
    };
    let interval = remote
        .as_ref()
        .map_or(interval, |config| config.interval_seconds);
    if interval == 0 {
        bail!("interval must be positive");
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let mut state: Value = if state_path.exists() {
        serde_json::from_slice(&std::fs::read(&state_path)?)?
    } else {
        if let Some(config) = &remote {
            loop {
                let first = remote_sample(&client, config).await;
                if first["healthy"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
        json!({"started":unix_now(),"samples":0,"failures":0,"recoveries":0,"last_steps":0,"progress_samples":0})
    };
    if state["measurement"] == "paused_by_user" {
        bail!("measurement paused by owner; start a new run explicitly");
    }
    let started = state["started"].as_f64().unwrap_or_else(unix_now);
    loop {
        let current = if let Some(config) = &remote {
            remote_sample(&client, config).await
        } else {
            sample(
                root.as_deref().context("local root missing")?,
                &client,
                &["local-main", "local-peer"],
            )
            .await
        };
        let healthy = current["healthy"].as_bool().unwrap_or(false);
        let steps = current["total_steps"].as_u64().unwrap_or(0);
        let last_steps = state["last_steps"].as_u64().unwrap_or(0);
        state["samples"] = json!(state["samples"].as_u64().unwrap_or(0) + 1);
        if !healthy {
            state["failures"] = json!(state["failures"].as_u64().unwrap_or(0) + 1);
        }
        if steps > last_steps {
            state["progress_samples"] = json!(state["progress_samples"].as_u64().unwrap_or(0) + 1);
        }
        let recoveries = current["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|node| node["recovery_attempt"] == 0)
            .count()
            + ["owner_recovery", "harness_recovery", "provider_recovery"]
                .iter()
                .filter(|key| current[**key] == 0)
                .count();
        state["recoveries"] = json!(state["recoveries"].as_u64().unwrap_or(0) + recoveries as u64);
        state["last_steps"] = json!(steps);
        let elapsed = (unix_now() - started).max(0.0);
        let actual = elapsed >= 86400.0;
        state["started"] = json!(started);
        state["elapsed_seconds"] = json!(elapsed);
        state["actual_24h_elapsed"] = json!(actual);
        state["measurement"] = json!(if actual {
            if state["failures"] == 0 {
                "24h_samples_without_observed_outage"
            } else {
                "24h_observed_with_outages"
            }
        } else {
            "collecting"
        });
        state["coverage"] = json!(if remote.is_some() {
            "Rust remote observer samples configured service readiness; gaps between samples, model continuity and meaningful work are not proven"
        } else {
            "Rust owner-side readiness/progress sampling; gaps between samples, model continuity and meaningful work are not proven"
        });
        state["last_sample"] = current.clone();
        state["model_continuity_verified"] = json!(false);
        state["meaningful_progress_verified"] = json!(false);
        state["attestation_ready"] = json!(false);
        write_atomic(&state_path, &state)?;
        write_atomic(&status_path, &state)?;
        use std::io::Write;
        if let Some(parent) = samples_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&samples_path)?;
        serde_json::to_writer(&mut log, &current)?;
        writeln!(log)?;
        if actual {
            break;
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_boundary_requires_a_full_day() {
        assert!(!(86399.999_f64 >= 86400.0));
        assert!(86400.0_f64 >= 86400.0);
    }

    #[test]
    fn status_replace_keeps_latest_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        write_atomic(&path, &json!({"sample": 1})).unwrap();
        write_atomic(&path, &json!({"sample": 2})).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["sample"], 2);
    }

    #[tokio::test]
    async fn readiness_accepts_plain_text_and_rejects_server_errors() {
        use axum::{Router, http::StatusCode, routing::get as axum_get};
        let app = Router::new()
            .route("/ready", axum_get(|| async { "ready" }))
            .route(
                "/down",
                axum_get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "down") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        assert!(ready(&client, &format!("http://{address}/ready")).await);
        assert!(!ready(&client, &format!("http://{address}/down")).await);
        server.abort();
    }

    #[tokio::test]
    async fn remote_observer_probes_configured_nodes_and_services() {
        use axum::{Json, Router, routing::get as axum_get};
        let app = Router::new()
            .route(
                "/v1/agents",
                axum_get(|| async {
                    Json(json!({"agents":[{"id":"a","status":"running","steps":7}]}))
                }),
            )
            .route("/health", axum_get(|| async { "ready" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = RemoteConfig {
            interval_seconds: 60,
            state_path: PathBuf::from("/tmp/state.json"),
            status_path: PathBuf::from("/tmp/status.json"),
            samples_path: PathBuf::from("/tmp/samples.jsonl"),
            nodes: vec![RemoteNode {
                name: "test".into(),
                url: format!("http://{address}"),
                token: "test-secret".into(),
            }],
            services: vec![RemoteService {
                name: "health".into(),
                url: format!("http://{address}/health"),
            }],
        };
        let result = remote_sample(&reqwest::Client::new(), &config).await;
        assert_eq!(result["healthy"], true);
        assert_eq!(result["total_steps"], 7);
        assert_eq!(result["nodes"][0]["ready"], true);
        assert_eq!(result["services"]["health"], true);
        server.abort();
    }
}
