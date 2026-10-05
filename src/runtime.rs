//! Standalone process composition and bounded shutdown.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::catalog::CatalogStore;
use crate::contracts::{ConfigSnapshot, GatewayApi, GatewayError, NoopTelemetry, StatusSink,
    ServiceId, Revision, ServiceStatus};
use crate::downstream::SdkDownstream;
use crate::health::HealthState;
use crate::local_config::LocalSource;
use crate::reconcile::Reconciler;
use crate::router::{ExecLimits, Router as ExecRouter};
use crate::search::SearchService;
use crate::server::{AuthConfig, McpServer};

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8080);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

struct LocalStatus;
#[async_trait]
impl StatusSink for LocalStatus {
    async fn publish(&self, _service: ServiceId, _revision: Revision, _status: ServiceStatus) -> Result<(), GatewayError> { Ok(()) }
}

struct GatewayFacade { search: Arc<SearchService>, exec: Arc<ExecRouter> }
#[async_trait]
impl GatewayApi for GatewayFacade {
    async fn search(&self, request: crate::contracts::SearchRequest) -> Result<Vec<crate::contracts::SearchHit>, GatewayError> { self.search.search(request).await }
    async fn exec(&self, request: crate::contracts::ExecRequest, context: crate::contracts::ExecContext) -> Result<rmcp::model::CallToolResponse, GatewayError> { self.exec.exec(request, context).await }
}

pub struct Runtime {
    router: Arc<ExecRouter>,
    cancel: CancellationToken,
    health: HealthState,
}

impl Runtime {
    pub async fn standalone(config: PathBuf, listen: Option<SocketAddr>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let telemetry = Arc::new(NoopTelemetry);
        let catalog = CatalogStore::new();
        let http = reqwest::Client::new();
        let downstream = Arc::new(SdkDownstream::new(http, telemetry.clone()));
        let search = SearchService::new(catalog.clone(), telemetry.clone());
        let router = Arc::new(ExecRouter::new(catalog.clone(), downstream.clone(), telemetry.clone(), ExecLimits::default()));
        let reconciler = Reconciler::new(catalog, downstream, Arc::new(LocalStatus), telemetry);
        let source = Arc::new(LocalSource::new(config));
        let cancel = CancellationToken::new();
        let (snapshot_tx, snapshot_rx) = watch::channel(ConfigSnapshot { revision: 0, services: Vec::new() });
        let (state_tx, mut state_rx) = watch::channel(crate::contracts::SourceState::default());
        let reconcile_cancel = cancel.clone();
        let reconcile_task = tokio::spawn(reconciler.run_source(source, snapshot_tx, state_tx, reconcile_cancel));
        let health = HealthState::new();
        health.set_initialized(true);
        health.set_accepting(true);
        let health_watch = health.clone();
        let reconciler_watch = reconciler.clone();
        tokio::spawn(async move {
            loop {
                let state = *state_rx.borrow();
                health_watch.set_source_healthy(state.healthy);
                health_watch.set_reconciled(state.initial_complete && reconciler_watch.initial_reconcile_complete());
                if state.initial_complete && reconciler_watch.initial_reconcile_complete() { break; }
                tokio::select! {
                    changed = state_rx.changed() => if changed.is_err() { break; },
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                }
            }
        });
        // Keep the receiver alive: Reconciler owns its subscribed receiver.
        let _ = snapshot_rx;
        let facade: Arc<dyn GatewayApi> = Arc::new(GatewayFacade { search, exec: router.clone() });
        let app = Router::new()
            .merge(McpServer::new(facade, AuthConfig::unauthenticated(), Vec::new()).router())
            .merge(health.router());
        let listener = tokio::net::TcpListener::bind(listen.unwrap_or(DEFAULT_LISTEN)).await?;
        let shutdown_health = health.clone();
        let shutdown_router = router.clone();
        let shutdown_cancel = cancel.clone();
        let shutdown = async move {
            wait_for_signal().await;
            shutdown_health.set_accepting(false);
            shutdown_router.shutdown(SHUTDOWN_GRACE).await;
            shutdown_cancel.cancel();
        };
        axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;
        health.stop();
        let _ = reconcile_task.await;
        Ok(())
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    { let _ = tokio::signal::ctrl_c().await; }
