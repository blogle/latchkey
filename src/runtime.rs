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
use crate::contracts::{
    ConfigSnapshot, ConfigSource, GatewayApi, GatewayError, NoopTelemetry, Revision, ServiceId,
    ServiceStatus, StatusSink,
};
use crate::downstream::SdkDownstream;
use crate::health::HealthState;
use crate::local_config::LocalSource;
use crate::reconcile::Reconciler;
use crate::router::{ExecLimits, Router as ExecRouter};
use crate::search::SearchService;
use crate::server::{AuthConfig, McpServer};

const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8080);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

struct LocalStatus;
#[async_trait]
impl StatusSink for LocalStatus {
    async fn publish(
        &self,
        _service: ServiceId,
        _revision: Revision,
        _status: ServiceStatus,
    ) -> Result<(), GatewayError> {
        Ok(())
    }
}

struct GatewayFacade {
    search: Arc<SearchService>,
    exec: Arc<ExecRouter>,
}
#[async_trait]
impl GatewayApi for GatewayFacade {
    async fn search(
        &self,
        request: crate::contracts::SearchRequest,
    ) -> Result<Vec<crate::contracts::SearchHit>, GatewayError> {
        self.search.search(request).await
    }

    async fn exec(
        &self,
        request: crate::contracts::ExecRequest,
        context: crate::contracts::ExecContext,
    ) -> Result<rmcp::model::CallToolResponse, GatewayError> {
        self.exec.exec(request, context).await
    }
}

pub struct Runtime {
    router: Arc<ExecRouter>,
    cancel: CancellationToken,
    health: HealthState,
}

impl Runtime {
    pub async fn standalone(
        config: PathBuf,
        listen: Option<SocketAddr>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let telemetry = Arc::new(NoopTelemetry);
        let catalog = CatalogStore::new();
        let http = reqwest::Client::new();
        let downstream = Arc::new(SdkDownstream::new(http, telemetry.clone()));
        let search = SearchService::new(catalog.clone(), telemetry.clone());
        let router = Arc::new(ExecRouter::new(
            catalog.clone(),
            downstream.clone(),
            telemetry.clone(),
            ExecLimits::default(),
        ));
        let reconciler = Reconciler::new(catalog, downstream, Arc::new(LocalStatus), telemetry);
        let source = Arc::new(LocalSource::new(config));
        let cancel = CancellationToken::new();
        let (snapshot_tx, snapshot_rx) = watch::channel(ConfigSnapshot {
            revision: 0,
            services: Vec::new(),
        });
        let (state_tx, state_rx) = watch::channel(crate::contracts::SourceState::default());
        let source_cancel = cancel.clone();
        let source_task = tokio::spawn({
            let source = Arc::clone(&source);
            async move { source.run(snapshot_tx, state_tx, source_cancel).await }
        });
        let mut snapshot_rx = snapshot_rx;
        let mut state_rx = state_rx;
        snapshot_rx
            .changed()
            .await
            .map_err(|_| "local source stopped before its first snapshot")?;
        state_rx
            .changed()
            .await
            .map_err(|_| "local source stopped before its first state")?;
        let initial_state = *state_rx.borrow();
        let initial_snapshot = snapshot_rx.borrow_and_update().clone();
        reconciler.reconcile(initial_snapshot).await;
        let health = HealthState::new();
        health.set_initialized(true);
        health.set_source_healthy(initial_state.healthy);
        health.set_reconciled(true);
        health.set_accepting(true);
        let health_watch = health.clone();
        tokio::spawn(async move {
            loop {
                let state = *state_rx.borrow();
                health_watch.set_source_healthy(state.healthy);
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
        });
        let facade: Arc<dyn GatewayApi> = Arc::new(GatewayFacade {
            search,
            exec: router.clone(),
        });
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
        let server = axum::serve(listener, app).with_graceful_shutdown(shutdown);
        let (server_result, _reconcile_result) =
            tokio::join!(server, reconciler.run(snapshot_rx, cancel.clone()));
        server_result?;
        health.stop();
        let _ = source_task.await;
        Ok(())
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
