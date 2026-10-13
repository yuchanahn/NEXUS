//! Owner-controlled acceptance executable. Never loaded from agent-editable source.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
async fn api(
    c: &reqwest::Client,
    url: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, Value)> {
    let mut request = c
        .request(method.parse()?, format!("{url}{path}"))
        .bearer_auth(token);
    if let Some(v) = body {
        request = request.json(&v)
    }
    let response = request.send().await?;
    Ok((response.status().as_u16(), response.json().await?))
}
async fn collaboration(root: &Path) -> Result<Value> {
    ensure!(
        root.join("Cargo.toml").is_file(),
        "Rust collaboration manifest missing"
    );
    let build = tokio::process::Command::new("cargo")
        .args([
            "build",
            "--locked",
            "--release",
            "--bin",
            "nexus-collaboration",
        ])
        .current_dir(root)
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(build.status.success(), "collaboration Rust build failed");
    let binary = root.join("target/release").join(if cfg!(windows) {
        "nexus-collaboration.exe"
    } else {
        "nexus-collaboration"
    });
    ensure!(binary.is_file(), "Rust executable missing");
    let temp = tempfile::tempdir()?;
    let reserve = std::net::TcpListener::bind("127.0.0.1:0")?;
    let bind = reserve.local_addr()?;
    drop(reserve);
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let config = json!({"node_id":"acceptance","bind":bind.to_string(),"database":temp.path().join("state.sqlite"),"token":token,"peers":{}});
    let config_file = temp.path().join("config.json");
    std::fs::write(&config_file, serde_json::to_vec(&config)?)?;
    let mut child = tokio::process::Command::new(binary)
        .arg("--config")
        .arg(config_file)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let c = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let url = format!("http://{bind}");
    let mut ready = false;
    for _ in 0..100 {
        if c.get(format!("{url}/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            ready = true;
            break;
        }
        if child.try_wait()?.is_some() {
            bail!("candidate service exited")
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ensure!(ready, "service did not start");
    let work=async{
        let mut checks=vec!["rust_build","service_start"];
        ensure!(c.get(format!("{url}/v1/records")).send().await?.status()==401,"unauthenticated request accepted");checks.push("authentication");
        let record=json!({"id":"acceptance-record","title":"검증 α","body":{"receipt":"preserve"},"metadata":{"extension":true}});
        ensure!(api(&c,&url,&token,"POST","/v1/records",Some(record.clone())).await?.0==201,"publish failed");checks.push("publish");
        ensure!(api(&c,&url,&token,"POST","/v1/records",Some(record)).await?.0==200,"dedup failed");checks.push("dedup");
        ensure!(api(&c,&url,&token,"POST","/v1/records",Some(json!({"id":"acceptance-record","body":"changed"}))).await?.0==409,"conflict accepted");checks.push("conflict");
        let got=api(&c,&url,&token,"GET","/v1/records/acceptance-record",None).await?.1;ensure!(got["body"]["receipt"]=="preserve"&&got["metadata"]["extension"]==true,"record changed");checks.push("body_metadata_preservation");
        ensure!(api(&c,&url,&token,"POST","/v1/records",Some(json!({"id":"acceptance-dm","kind":"dm","to_actor":"alice","title":"subject","body":"secret"}))).await?.0==201,"DM failed");
        ensure!(api(&c,&url,&token,"GET","/v1/records/acceptance-dm?actor=bob",None).await?.0==403,"DM leaked");checks.push("dm_privacy");
        let inbox=api(&c,&url,&token,"GET","/v1/inbox?actor=alice&unread=1",None).await?.1;ensure!(inbox["records"][0].get("body").is_none(),"DM body injected");checks.push("headers_only");
        ensure!(api(&c,&url,&token,"POST","/v1/records/acceptance-dm/ack",Some(json!({"actor":"alice"}))).await?.0==200,"ack failed");ensure!(api(&c,&url,&token,"GET","/v1/inbox?actor=alice&unread=1",None).await?.1["records"].as_array().context("inbox")?.is_empty(),"ack not persisted");checks.push("ack");
        ensure!(api(&c,&url,&token,"POST","/v1/schemas",Some(json!({"kind":"typed","schema":{"type":"integer"}}))).await?.0<300,"schema registration failed");ensure!(api(&c,&url,&token,"POST","/v1/records",Some(json!({"kind":"typed","body":"bad"}))).await?.0==422,"invalid schema body accepted");checks.push("json_schema");
        Ok::<_,anyhow::Error>(json!({"passed":true,"implementation":"rust","checks":checks,"scope":"isolated service acceptance; federation/restart covered by Rust integration tests"}))
    }.await;
    child.kill().await?;
    work
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .context("usage: nexus-verify artifact|bootstrap|collaboration|endurance PATH ...")?;
    let path = args.next().context("path required")?;
    let result = match command.as_str() {
        "artifact" => {
            let expected = args.next().context("expected text required")?;
            ensure!(
                std::fs::read_to_string(path)? == expected,
                "artifact mismatch"
            );
            json!({"passed":true,"check":"artifact"})
        }
        "collaboration" => collaboration(Path::new(&path)).await?,
        "endurance" => {
            let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(
                v["actual_24h_elapsed"] == true
                    && v["elapsed_seconds"].as_u64().is_some_and(|n| n >= 86400)
                    && v["failed_samples"] == 0
                    && v["meaningful_progress_verified"] == true,
                "owner endurance attestation not complete"
            );
            json!({"passed":true,"check":"owner_24h_attestation"})
        }
        "bootstrap" => {
            let root = Path::new(&path);
            let a: Value = serde_json::from_slice(&std::fs::read(root.join("root-proof.json"))?)?;
            let b: Value = serde_json::from_slice(&std::fs::read(root.join("child-proof.json"))?)?;
            ensure!(
                a["plugin_value"] == "MODEL_TOOL_PROOF"
                    && !a["nonce"].as_str().unwrap_or("").is_empty()
                    && a["nonce"] == b["nonce"]
                    && a["child_agent_id"] == b["agent_id"],
                "bootstrap proof mismatch"
            );
            let manifest: Value = serde_json::from_slice(&std::fs::read(
                "/workspace/extensions/bootstrap_echo.json",
            )?)?;
            let argv = manifest["command"].as_array().context("manifest command")?;
            let receipt = uuid::Uuid::new_v4().to_string();
            let mut cmd =
                tokio::process::Command::new(argv[0].as_str().context("manifest executable")?);
            for arg in &argv[1..] {
                cmd.arg(arg.as_str().context("manifest argument")?);
            }
            let mut process = cmd
                .env("NEXUS_AGENT_ID", "owner-verifier")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            use tokio::io::AsyncWriteExt;
            let mut input = process.stdin.take().unwrap();
            input
                .write_all(json!({"value":receipt}).to_string().as_bytes())
                .await?;
            input.shutdown().await?;
            drop(input);
            let output =
                tokio::time::timeout(Duration::from_secs(10), process.wait_with_output()).await??;
            let value: Value = serde_json::from_slice(&output.stdout)?;
            ensure!(
                output.status.success()
                    && value["value"] == receipt
                    && value["agent_id"] == "owner-verifier",
                "plugin receipt mismatch"
            );
            json!({"passed":true,"checks":["generated_plugin_executes","external_receipt_preserved","child_dm_nonce_matches","child_identity_matches"]})
        }
        _ => bail!("unknown acceptance"),
    };
    println!("{result}");
    Ok(())
}
