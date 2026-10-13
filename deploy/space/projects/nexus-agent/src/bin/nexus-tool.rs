//! Immutable, Rust-only worker executable. Executes JSON requests in the tool container.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Deserialize)]
struct Request {
    agent: String,
    node: String,
    call: String,
    op: String,
    args: Value,
    command: Option<Vec<String>>,
}
fn checked_path(raw: &str, writing: bool) -> Result<PathBuf> {
    let input = Path::new(raw);
    if !input.is_absolute()
        || input
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        bail!("absolute workspace path required")
    }
    let mut cursor = input.to_path_buf();
    let mut missing = Vec::new();
    while !cursor.exists() {
        if fs::symlink_metadata(&cursor).is_ok() {
            bail!("dangling symlink is not permitted")
        }
        missing.push(cursor.file_name().context("invalid path")?.to_owned());
        if !cursor.pop() {
            bail!("path has no existing parent")
        }
    }
    let mut resolved = cursor.canonicalize()?;
    for part in missing.iter().rev() {
        resolved.push(part)
    }
    let permitted =
        resolved.starts_with("/workspace") || (!writing && resolved.starts_with("/checks"));
    if !permitted {
        bail!("path outside writable workspace or read-only checks")
    }
    Ok(resolved)
}
fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn text(v: &Value, k: &str) -> Result<String> {
    Ok(v[k]
        .as_str()
        .with_context(|| format!("missing {k}"))?
        .to_owned())
}
fn prefix(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(16000).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
async fn execute(r: Request) -> Result<Value> {
    for ident in [&r.agent, &r.node] {
        if ident.is_empty()
            || ident.len() > 128
            || !ident
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("invalid execution identity")
        }
    }
    if r.call.is_empty() || r.call.len()>1024 {bail!("invalid call identity")}
    let artifact_key=format!("{:x}",Sha256::digest(r.call.as_bytes()));
    let a = &r.args;
    match r.op.as_str() {
        "read" => {
            let p = checked_path(&text(a, "path")?, false)?;
            let offset = a["offset"].as_u64().unwrap_or(0);
            let limit = a["limit"].as_u64().unwrap_or(16000).min(64000);
            let mut f = fs::File::open(&p)?;
            f.seek(SeekFrom::Start(offset))?;
            let mut raw = Vec::new();
            f.take(limit).read_to_end(&mut raw)?;
            Ok(
                json!({"path":p,"text":String::from_utf8_lossy(&raw),"offset":offset,"next_offset":offset+raw.len() as u64,"size":fs::metadata(&p)?.len(),"sha256":digest(&p)?}),
            )
        }
        "write" => {
            let p = checked_path(&text(a, "path")?, true)?;
            let parent = p.parent().context("missing parent")?;
            fs::create_dir_all(parent)?;
            if let Some(expected) = a["expected_sha256"].as_str()
                && (!p.exists() || digest(&p)? != expected)
            {
                bail!("file changed since read")
            }
            let raw = text(a, "text")?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(raw.as_bytes())?;
            temp.as_file().sync_all()?;
            temp.persist(&p).map_err(|e| e.error)?;
            Ok(
                json!({"path":p,"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(raw.as_bytes()))}),
            )
        }
        "list" => {
            let p = checked_path(a["path"].as_str().unwrap_or("/workspace/work"), false)?;
            let mut entries = fs::read_dir(p)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            let values=entries.into_iter().take(200).map(|e|{let meta=e.file_type()?;Ok(json!({"name":e.file_name().to_string_lossy(),"directory":meta.is_dir(),"symlink":meta.is_symlink()}))}).collect::<Result<Vec<_>>>()?;
            Ok(json!({"entries":values}))
        }
        "run" | "extension" => {
            let cwd = checked_path(a["cwd"].as_str().unwrap_or("/workspace/work"), true)?;
            let timeout = a["timeout_seconds"].as_u64().unwrap_or(120).clamp(1, 300);
            let base = checked_path(
                &format!("/workspace/work/.nexus-artifacts/{}/{}", r.agent, artifact_key),
                true,
            )?;
            fs::create_dir_all(base.parent().context("missing artifact parent")?)?;
            let out = base.with_extension("stdout");
            let err = base.with_extension("stderr");
            let argv = if r.op == "run" {
                vec!["sh".to_owned(), "-c".to_owned(), text(a, "command")?]
            } else {
                r.command.context("extension command required")?
            };
            let (program, args) = argv.split_first().context("empty command")?;
            let mut cmd = Command::new(program);
            cmd.args(args)
                .current_dir(cwd)
                .env("NEXUS_AGENT_ID", r.agent)
                .env("NEXUS_NODE_ID", r.node)
                .stdin(Stdio::piped())
                .stdout(fs::File::create(&out)?)
                .stderr(fs::File::create(&err)?)
                .kill_on_drop(true);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.as_std_mut().process_group(0);
            }
            let mut child = cmd.spawn()?;
            let pid = child.id();
            if let Some(mut stdin) = child.stdin.take() {
                if r.op == "extension" {
                    stdin.write_all(&serde_json::to_vec(a)?).await?;
                }
                stdin.shutdown().await?;
            }
            let (status, timed_out) =
                match tokio::time::timeout(Duration::from_secs(timeout), child.wait()).await {
                    Ok(result) => (result?, false),
                    Err(_) => {
                        #[cfg(unix)]
                        if let Some(pid) = pid {
                            unsafe {
                                libc::killpg(pid as i32, libc::SIGKILL);
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = pid;
                            child.start_kill()?;
                        }
                        (child.wait().await?, true)
                    }
                };
            Ok(
                json!({"exit_code":status.code().unwrap_or(-9),"timed_out":timed_out,"stdout":prefix(&out)?,"stderr":prefix(&err)?,"stdout_path":out,"stderr_path":err,"stdout_bytes":fs::metadata(&out)?.len(),"stderr_bytes":fs::metadata(&err)?.len()}),
            )
        }
        _ => bail!("unknown operation"),
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("idle") {
        nexus_agent::shutdown().await;
        return Ok(());
    }
    let mut raw = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 1024 * 1024 {
        bail!("request too large")
    }
    let response = execute(serde_json::from_slice(&raw)?).await?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}
