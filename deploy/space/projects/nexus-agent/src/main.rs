use nexus_agent::{api, runtime::Runtime, store::Store, types::Config};
use std::{future::IntoFuture, sync::Arc};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or("serve".into());
    if command != "serve" {
        anyhow::bail!("usage: nexus-agent serve <protected-config.json>");
    }
    let path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("configuration path required"))?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    let store = Arc::new(Store::open(&config.database)?);
    let runtime = Runtime::new(config.clone(), store)?;
    runtime.sandbox.prepare().await?;
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    let (sender, receiver) = tokio::sync::watch::channel(false);
    let loop_runtime = runtime.clone();
    let mut worker = tokio::spawn(loop_runtime.run(receiver));
    eprintln!("NEXUS node {} listening on {}", config.node_id, config.bind);
    let signal_sender = sender.clone();
    let server = axum::serve(listener, api::router(runtime))
        .with_graceful_shutdown(async move {
            nexus_agent::shutdown().await;
            let _ = signal_sender.send(true);
        })
        .into_future();
    tokio::select! {
        result=server=>{let _=sender.send(true);result?;worker.await??;Ok(())},
        result=&mut worker=>{let _=sender.send(true);result??;anyhow::bail!("scheduler exited unexpectedly; supervisor must restart this node")}
    }
}
