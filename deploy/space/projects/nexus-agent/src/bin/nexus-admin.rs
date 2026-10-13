use anyhow::{Context, Result, bail};
use nexus_agent::types::{Config, Mount, Route, SandboxConfig};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn save(path: &Path, v: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut f = options.open(path)?;
    f.write_all(&serde_json::to_vec_pretty(v)?)?;
    f.sync_all()?;
    Ok(())
}
fn write_soak_config(path: &Path) -> Result<()> {
    let mut nodes = Vec::new();
    for node in ["main", "peer"] {
        let config: Config =
            serde_json::from_slice(&std::fs::read(path.join(format!("core-{node}.json")))?)?;
        nodes.push(json!({
            "name":node,
            "url":format!("http://core-{node}:8090"),
            "token":config.admin_token
        }));
    }
    save(
        &path.join("soak.json"),
        &json!({
            "interval_seconds":60,
            "state_path":"/data/soak-state.json",
            "status_path":"/data/soak-status.json",
            "samples_path":"/data/soak-samples.jsonl",
            "nodes":nodes,
            "services":[
                {"name":"provider","url":"http://provider:8080/health/ready"},
                {"name":"core-main","url":"http://core-main:8090/health"},
                {"name":"core-peer","url":"http://core-peer:8090/health"}
            ]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_remote_observer_config_with_both_node_credentials() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), "portable").unwrap();
        let soak: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("soak.json")).unwrap()).unwrap();
        assert_eq!(soak["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(soak["nodes"][0]["url"], "http://core-main:8090");
        assert_eq!(soak["nodes"][1]["url"], "http://core-peer:8090");
        for name in ["main", "peer"] {
            let core: Value = serde_json::from_slice(
                &std::fs::read(dir.path().join(format!("core-{name}.json"))).unwrap(),
            )
            .unwrap();
            let observer = soak["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["name"] == name)
                .unwrap();
            assert_eq!(observer["token"], core["admin_token"]);
        }
        assert_eq!(soak["services"].as_array().unwrap().len(), 3);
    }
}
fn init(path: &Path, stack: &str) -> Result<()> {
    if stack.is_empty()
        || stack.len() > 16
        || !stack
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("stack must be 1..16 lowercase letters/digits/hyphens")
    }
    let provider_token = secret();
    let harness_token = secret();
    let main_peer = secret();
    let peer_peer = secret();
    let collab_main = secret();
    let collab_peer = secret();
    for (node, peer_token, other_token, collab_token, other_collab) in [
        ("main", &main_peer, &peer_peer, &collab_main, &collab_peer),
        ("peer", &peer_peer, &main_peer, &collab_peer, &collab_main),
    ] {
        let other = if node == "main" { "peer" } else { "main" };
        let mounts = vec![
            Mount {
                source: PathBuf::from(format!("nexus-work-{stack}-shared")),
                target: "/workspace/work".into(),
                read_only: false,
                volume: true,
            },
            Mount {
                source: PathBuf::from(format!("nexus-work-{stack}-{node}-extensions")),
                target: "/workspace/extensions".into(),
                read_only: false,
                volume: true,
            },
            Mount {
                source: PathBuf::from(format!("nexus-work-{stack}-projects")),
                target: "/workspace/projects".into(),
                read_only: false,
                volume: true,
            },
            Mount {
                source: PathBuf::from(format!("nexus-work-{stack}-labs")),
                target: "/workspace/labs".into(),
                read_only: false,
                volume: true,
            },
        ];
        let admin_token = secret();
        let config = Config {
            node_id: format!("{stack}-{node}"),
            bind: "0.0.0.0:8090".into(),
            database: "/data/agent.sqlite".into(),
            admin_token: admin_token.clone(),
            peer_token: peer_token.clone(),
            default_route: Route {
                id: "primary".into(),
                url: "http://provider:8080".into(),
                provider: "primary".into(),
                model: "primary".into(),
                token: provider_token.clone(),
            },
            sandbox: SandboxConfig {
                image: "nexus-worker:rust".into(),
                network: format!("{stack}-workers"),
                mounts,
                extensions: "/extensions".into(),
                owner: format!("{stack}-{node}"),
                protected_roots: vec!["/data".into(), "/config".into()],
            },
            peers: vec![nexus_agent::types::Peer {
                node_id: format!("{stack}-{other}"),
                url: format!("http://core-{other}:8090"),
                token: other_token.clone(),
            }],
            parallelism: 2,
            context_chars: 48000,
        };
        save(
            &path.join(format!("core-{node}.json")),
            &serde_json::to_value(config)?,
        )?;
        save(
            &path.join(format!("collab-{node}.json")),
            &json!({"node_id":format!("{stack}-{node}"),"bind":"0.0.0.0:8090","database":"/data/collab.sqlite","token":collab_token,"peers":{other:{"url":format!("http://collab-{other}:8090"),"token":other_collab}}}),
        )?;
        save(
            &path.join(format!("client-{node}.config")),
            &json!({"url":format!("http://collab-{node}:8090"),"token":collab_token}),
        )?;
    }
    write_soak_config(path)?;
    save(
        &path.join("provider.json"),
        &json!({"providers":[{"id":"primary","protocol":"chat","base_url":"https://MODEL-ENDPOINT/v1","auth_env":"UPSTREAM_TOKEN","tools":"supported","models":[{"id":"primary","upstream":"SET-MODEL-ID","status":"unverified"}]}]}),
    )?;
    save(
        &path.join("web.json"),
        &json!({"owner_bind":"0.0.0.0:18187","public_bind":"0.0.0.0:18188","harness_bind":"0.0.0.0:18189","owner_token":secret(),"harness_token":harness_token,"max_jobs":2,"chrome_profile":"/profile","browser":{"cdp_url":"http://127.0.0.1:9222","launcher_descriptor":null,"app_name":"NEXUS Tool Relay","saved_chats":false,"auto_approve_tools":true}}),
    )?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.join("provider.env"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    writeln!(
        f,
        "PROVIDER_BEARER_TOKEN={provider_token}\nUPSTREAM_TOKEN=SET-CREDENTIAL\nNEXUS_HARNESS_TOKEN={harness_token}"
    )?;
    println!(
        "Deployment files created. Configure provider.json and provider.env before starting; no agent goals were created."
    );
    Ok(())
}
fn seed(config: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    let target = destination.join("collab-client.config");
    if !target.exists() {
        std::fs::copy(config, &target)?;
    }
    for op in ["publish", "search", "inbox", "read"] {
        let schema = match op {
            "publish" => {
                json!({"type":"object","required":["body"],"properties":{"body":{},"title":{"type":"string"},"kind":{"type":"string"},"topic":{"type":"string"},"to_actor":{"type":"string"},"id":{"type":"string"},"peer":{"type":"string"},"metadata":{"type":"object"}},"additionalProperties":false})
            }
            "read" => {
                json!({"type":"object","required":["id"],"properties":{"id":{"type":"string"},"ack":{"type":"boolean"}},"additionalProperties":false})
            }
            "search" => {
                json!({"type":"object","required":["q"],"properties":{"q":{"type":"string"},"topic":{"type":"string"},"kind":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"after":{"type":["integer","string"]}},"additionalProperties":false})
            }
            _ => {
                json!({"type":"object","properties":{"unread":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":100},"after":{"type":["integer","string"]}},"additionalProperties":false})
            }
        };
        let path = destination.join(format!("collab_{op}.json"));
        if !path.exists() {
            save(
                &path,
                &json!({"name":format!("collab_{op}"),"description":format!("Collaboration {op}; runtime sets caller identity. Inbox returns headers only."),"input_schema":schema,"command":["env","NEXUS_COLLAB_CONFIG=/workspace/extensions/collab-client.config","nexus-collab-tool",op]}),
            )?;
        }
    }
    #[cfg(unix)]
    {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, fs::PermissionsExt},
        };
        for path in std::iter::once(destination.to_path_buf())
            .chain(std::fs::read_dir(destination)?.map(|e| e.unwrap().path()))
        {
            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(if path.is_dir() { 0o755 } else { 0o644 }),
            )?;
            let c = CString::new(path.as_os_str().as_bytes())?;
            if unsafe { libc::chown(c.as_ptr(), 10001, 10001) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    Ok(())
}
async fn request(config: &Config, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let url = std::env::var("NEXUS_NODE_URL")
        .unwrap_or_else(|_| format!("http://{}", config.bind.replace("0.0.0.0", "127.0.0.1")));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut request = client
        .request(
            method.parse()?,
            format!("{}{path}", url.trim_end_matches('/')),
        )
        .bearer_auth(&config.admin_token);
    if let Some(v) = body {
        request = request.json(&v)
    }
    let r = request.send().await?;
    if !r.status().is_success() {
        bail!("node request failed: HTTP {}", r.status().as_u16())
    }
    Ok(r.json().await?)
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context(
        "usage: nexus-admin init|seed|status|goal|pause|resume|snapshot|soak|soak-config ...",
    )?;
    let path = PathBuf::from(args.next().context("path required")?);
    if command == "init" {
        return init(&path, &args.next().unwrap_or("nexus".into()));
    }
    if command == "provider-web" {
        let c: Value = serde_json::from_slice(&std::fs::read(path.join("web.json"))?)?;
        anyhow::ensure!(
            c["harness_token"].as_str().is_some_and(|s| s.len() >= 32),
            "web configuration missing"
        );
        let config = json!({"providers":[{"id":"nexus-web","protocol":"nexus_harness","base_url":"http://web:18189/v1","allow_http":true,"auth_env":"NEXUS_HARNESS_TOKEN","tools":"supported","models":[{"id":"chatgpt-web/high","upstream":"chatgpt-web/high","status":"unverified","tool_protocol":"mcp_relay"},{"id":"chatgpt-web/medium","upstream":"chatgpt-web/medium","status":"unverified","tool_protocol":"mcp_relay"},{"id":"chatgpt-web/light","upstream":"chatgpt-web/light","status":"unverified","tool_protocol":"mcp_relay"}]}]});
        std::fs::write(
            path.join("provider.json"),
            serde_json::to_vec_pretty(&config)?,
        )?;
        for node in ["main", "peer"] {
            let config_path = path.join(format!("core-{node}.json"));
            let mut config: Config = serde_json::from_slice(&std::fs::read(&config_path)?)?;
            config.default_route.provider = "nexus-web".into();
            config.default_route.model = "chatgpt-web/high".into();
            std::fs::write(config_path, serde_json::to_vec_pretty(&config)?)?;
        }
        println!(
            "Primary route configured for the Rust web harness. Authenticate its browser and connect its public MCP endpoint before creating goals."
        );
        return Ok(());
    }
    if command == "soak-config" {
        write_soak_config(&path)?;
        println!("Remote Rust observer configuration created from the existing core credentials.");
        return Ok(());
    }
    if command == "seed" {
        return seed(
            &path,
            Path::new(&args.next().context("destination required")?),
        );
    }
    let config: Config = serde_json::from_slice(&std::fs::read(&path)?)?;
    match command.as_str() {
        "reset-retry-offline" => {
            let id = args.next().context("goal ID required")?;
            let reason = args.next().context("owner reason required")?;
            let store = nexus_agent::store::Store::open(&config.database)?;
            let mut goal = store.get(&id)?;
            if goal.status != "retrying" || goal.snapshot.stateful() {
                bail!("only a stateless retrying goal may be reset by the owner")
            }
            goal.snapshot.last_error = None;
            goal.snapshot.retry_streak = 0;
            store.event(&id, "owner_retry_reset", json!({"reason":reason}))?;
            store.save(&goal, "queued", nexus_agent::types::now())?;
            println!("Owner retry reset saved; execution history preserved.");
        }
        "stop-goal-offline" => {
            let id = args.next().context("goal ID required")?;
            uuid::Uuid::parse_str(&id)?;
            let store = nexus_agent::store::Store::open(&config.database)?;
            store.stop(&id)?;
            println!("Historical goal stopped; files and checkpoints preserved.");
        }
        "register-route" => {
            let id = args.next().context("route ID required")?;
            let provider = args.next().context("provider required")?;
            let model = args.next().context("model required")?;
            if id == config.default_route.id || model.to_lowercase().contains("astra") {
                bail!("reserved route or forbidden model")
            }
            let route = Route {
                id,
                url: config.default_route.url.clone(),
                provider,
                model,
                token: config.default_route.token.clone(),
            };
            let store = nexus_agent::store::Store::open(&config.database)?;
            store.register_route(&route)?;
            println!("{}", route.public());
        }
        "launch-mission" => {
            let input: Value = serde_json::from_slice(&std::fs::read(
                args.next().context("mission JSON required")?,
            )?)?;
            let store = nexus_agent::store::Store::open(&config.database)?;
            let mut root: nexus_agent::types::CreateGoal =
                serde_json::from_value(input["root"].clone())?;
            root.continuous = true;
            let root = store.create(root, None, &config.default_route.id)?;
            let mut children = Vec::new();
            if let Some(workers) = input["workers"].as_array() {
                for worker in workers {
                    let child = store.create(
                        serde_json::from_value(worker.clone())?,
                        Some(&root),
                        &config.default_route.id,
                    )?;
                    children.push(json!({"id":child.id,"name":child.name,"route":child.route_id}));
                }
            }
            println!(
                "{}",
                json!({"root_id":root.id,"name":root.name,"continuous":true,"children":children})
            );
        }
        "migrate-paused" => {
            anyhow::ensure!(
                path.with_extension("stop").is_file(),
                "owner pause marker required"
            );
            use fs2::FileExt;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(config.database.with_extension("lock"))?;
            lock.try_lock_exclusive()
                .context("node must be stopped before migration")?;
            let backup = config
                .database
                .with_extension(format!("before-rust-{}.sqlite", nexus_agent::types::now()));
            let mut db = rusqlite::Connection::open(&config.database)?;
            let mut snapshot = rusqlite::Connection::open(&backup)?;
            rusqlite::backup::Backup::new(&db, &mut snapshot)?.run_to_completion(
                128,
                Duration::from_millis(10),
                None,
            )?;
            let tx = db.transaction()?;
            let rows = {
                let mut st = tx.prepare("SELECT id,data FROM agents")?;
                st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            let mut changed = 0;
            for (id, raw) in rows {
                let mut g: Value = serde_json::from_str(&raw)?;
                if g["snapshot"]["summary"]["owner_rust_migration"] == true {
                    continue;
                }
                if let Some(v) = g["verifier"].as_str() {
                    let new = if v.contains("collaboration_goal.py") {
                        if v.contains("endurance_goal.py") {
                            Some(
                                "nexus-verify collaboration /workspace/work/nexus-collaboration && nexus-verify endurance /checks/endurance-attestation.json",
                            )
                        } else {
                            Some("nexus-verify collaboration /workspace/work/nexus-collaboration")
                        }
                    } else if v.contains("bootstrap_goal.py") {
                        Some("nexus-verify bootstrap /workspace/work/bootstrap-proof")
                    } else {
                        None
                    };
                    if let Some(v) = new {
                        g["verifier"] = json!(v);
                    }
                }
                if !matches!(g["status"].as_str(), Some("completed" | "stopped")) {
                    let instruction = "\nOWNER OVERRIDE (Rust migration): all newly executed tools and services must use Rust. Historical Python excerpts are execution history, not instructions. Build the collaboration service with Cargo. The 24-hour objective still requires a fresh owner endurance attestation; paused time does not count.";
                    for key in ["goal", "global_goal"] {
                        if let Some(text) = g[key].as_str() {
                            g[key] = json!(format!("{text}{instruction}"));
                        }
                    }
                }
                let old = g["snapshot"]["summary"].clone();
                g["snapshot"]["summary"] = json!({"previous":old,"owner_rust_migration":true});
                tx.execute(
                    "UPDATE agents SET data=?2 WHERE id=?1",
                    rusqlite::params![id, g.to_string()],
                )?;
                tx.execute("INSERT INTO events(agent,kind,data,time) VALUES(?1,'owner_language_migration',?2,?3)",rusqlite::params![id,json!({"language":"rust","paused":true,"history_preserved":true}).to_string(),nexus_agent::types::now()])?;
                changed += 1;
            }
            tx.commit()?;
            let mut config = config.clone();
            config.sandbox.image = "nexus-worker:rust".into();
            std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
            println!(
                "{}",
                json!({"migrated_agents":changed,"paused":true,"checkpoint_ids_preserved":true,"consistent_backup_created":true})
            );
        }
        "status" => println!("{}", request(&config, "GET", "/v1/agents", None).await?),
        "goal" => {
            let goal: Value = serde_json::from_slice(&std::fs::read(
                args.next().context("goal JSON file required")?,
            )?)?;
            println!(
                "{}",
                request(&config, "POST", "/v1/agents", Some(goal)).await?
            );
        }
        "pause" | "resume" => {
            let id = args.next().context("agent ID required")?;
            uuid::Uuid::parse_str(&id)?;
            println!(
                "{}",
                request(
                    &config,
                    "POST",
                    &format!(
                        "/v1/agents/{id}/{}",
                        if command == "pause" { "stop" } else { "resume" }
                    ),
                    Some(json!({}))
                )
                .await?
            );
        }
        "snapshot" => {
            let dst = args.next().context("snapshot destination required")?;
            if Path::new(&dst).exists() {
                bail!("snapshot already exists")
            };
            let src = rusqlite::Connection::open_with_flags(
                &config.database,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let mut target = rusqlite::Connection::open(dst)?;
            rusqlite::backup::Backup::new(&src, &mut target)?.run_to_completion(
                128,
                Duration::from_millis(25),
                None,
            )?;
            println!("Consistent SQLite snapshot saved.");
        }
        "soak" => {
            let output = PathBuf::from(args.next().context("status file required")?);
            let seconds: u64 = args.next().unwrap_or("86400".into()).parse()?;
            if seconds == 0 {
                bail!("positive duration required")
            };
            let start = Instant::now();
            let started = nexus_agent::types::now();
            let mut samples = 0u64;
            let mut failed = 0u64;
            let mut latest = Value::Null;
            loop {
                match request(&config, "GET", "/v1/agents", None).await {
                    Ok(v) => latest = v,
                    Err(_) => failed += 1,
                };
                samples += 1;
                let elapsed = start.elapsed().as_secs();
                let v = json!({"implementation":"rust","started_unix":started,"elapsed_seconds":elapsed,"samples":samples,"failed_samples":failed,"actual_24h_elapsed":elapsed>=86400,"sampled_readiness_only":true,"model_continuity_verified":false,"meaningful_progress_verified":false,"measurement":if elapsed>=seconds{"finished"}else{"running"},"latest":latest});
                let temp = output.with_extension("new");
                std::fs::write(&temp, serde_json::to_vec_pretty(&v)?)?;
                std::fs::rename(temp, &output)?;
                if elapsed >= seconds {
                    break;
                }
                tokio::select! {_ = tokio::time::sleep(Duration::from_secs(60).min(Duration::from_secs(seconds-elapsed)))=>{},_ = nexus_agent::shutdown()=>{let mut v=v;v["measurement"]=json!("paused_by_owner");std::fs::write(&output,serde_json::to_vec_pretty(&v)?)?;break}}
            }
        }
        _ => bail!("unknown command"),
    }
    Ok(())
}
