mod crd;

use crate::crd::{LatchkeyPolicy, LatchkeyPrincipal, LatchkeyServer, LatchkeyTool};
use anyhow::Context;
use futures::{StreamExt, TryStreamExt};
use kube::{Api, Client, ResourceExt};
use kube_runtime::watcher::{self, Event};
use tokio::task::JoinSet;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    info!("operator booted");

    let client = Client::try_default().await.context("failed to create kubernetes client")?;

    let mut tasks = JoinSet::new();
    tasks.spawn(watch_servers(client.clone()));
    tasks.spawn(watch_tools(client.clone()));
    tasks.spawn(watch_principals(client.clone()));
    tasks.spawn(watch_policies(client));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received");
        }
        Some(result) = tasks.join_next() => {
            match result {
                Ok(Ok(())) => info!("watch task exited cleanly"),
                Ok(Err(err)) => error!(error = %err, "watch task failed"),
                Err(err) => error!(error = %err, "watch task join failure"),
            }
        }
    }

    tasks.abort_all();
    while tasks.join_next().await.is_some() {}

    Ok(())
}

async fn watch_servers(client: Client) -> anyhow::Result<()> {
    let api: Api<LatchkeyServer> = Api::all(client);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) =
        stream.try_next().await.context("latchkeyserver watch stream failure")?
    {
        log_event("LatchkeyServer", event);
    }

    Ok(())
}

async fn watch_tools(client: Client) -> anyhow::Result<()> {
    let api: Api<LatchkeyTool> = Api::all(client);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) = stream.try_next().await.context("latchkeytool watch stream failure")? {
        log_event("LatchkeyTool", event);
    }

    Ok(())
}

async fn watch_principals(client: Client) -> anyhow::Result<()> {
    let api: Api<LatchkeyPrincipal> = Api::all(client);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) =
        stream.try_next().await.context("latchkeyprincipal watch stream failure")?
    {
        log_event("LatchkeyPrincipal", event);
    }

    Ok(())
}

async fn watch_policies(client: Client) -> anyhow::Result<()> {
    let api: Api<LatchkeyPolicy> = Api::all(client);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) =
        stream.try_next().await.context("latchkeypolicy watch stream failure")?
    {
        log_event("LatchkeyPolicy", event);
    }

    Ok(())
}

fn log_event<T>(resource: &str, event: Event<T>)
where
    T: ResourceExt,
{
    match event {
        Event::Apply(obj) => info!(resource, name = %obj.name_any(), "applied"),
        Event::Delete(obj) => info!(resource, name = %obj.name_any(), "deleted"),
        Event::Init => info!(resource, "initializing list"),
        Event::InitApply(obj) => info!(resource, name = %obj.name_any(), "initial apply"),
        Event::InitDone => info!(resource, "initialization complete"),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .with_span_list(false)
        .init();
}
