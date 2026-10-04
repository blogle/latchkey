mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use latchkey::contracts::{
    CatalogEntry, CatalogRead, CatalogSnapshot, ExecContext, ExecRequest, GatewayApi,
    NoopTelemetry, ResolvedService, Revision, RouteTarget, SensitiveHeaders, ServiceId,
    ServiceSpec,
};
use latchkey::downstream::SdkDownstream;
use latchkey::router::{ExecLimits, Router};
use tokio_util::sync::CancellationToken;
use url::Url;

struct MemoryCatalog(Arc<CatalogSnapshot>);

#[async_trait]
impl CatalogRead for MemoryCatalog {
    async fn snapshot(&self) -> Result<Arc<CatalogSnapshot>, latchkey::contracts::GatewayError> {
        Ok(Arc::clone(&self.0))
    }
}

fn service(endpoint: &str) -> Arc<ResolvedService> {
    Arc::new(ResolvedService {
        spec: ServiceSpec {
            id: ServiceId::new("fixture"),
            prefix: "fixture".into(),
            endpoint: Url::parse(endpoint).unwrap(),
            enabled: true,
            timeout: Duration::from_secs(5),
            refresh_interval: Duration::from_secs(5),
        },
        revision: Revision {
            source_uid: "test".into(),
            generation: 1,
            credential_revision: "test".into(),
        },
        headers: SensitiveHeaders::new(Vec::new()),
    })
}

fn make_router(endpoint: &str) -> Router {
    let service = service(endpoint);
    let mut entries = BTreeMap::new();
    for (canonical, original) in [
        ("fixture__echo", "echo"),
        ("fixture__barrier", "barrier"),
        ("fixture__side_effect", "side_effect"),
    ] {
        entries.insert(
            canonical.to_owned(),
            CatalogEntry {
                name: canonical.to_owned(),
                service: ServiceId::new("fixture"),
                description: canonical.to_owned(),
                input_schema: serde_json::Map::new(),
                downstream_name: original.to_owned(),
                revision: service.revision.clone(),
            },
        );
    }
    let mut routes = BTreeMap::new();
    routes.insert(ServiceId::new("fixture"), RouteTarget::new(service, true));
    Router::new(
        Arc::new(MemoryCatalog(Arc::new(CatalogSnapshot {
            epoch: 1,
            entries,
            routes,
        }))),
        Arc::new(SdkDownstream::new(
            reqwest::Client::new(),
            Arc::new(NoopTelemetry),
        )),
        Arc::new(NoopTelemetry),
        ExecLimits::default(),
    )
}

fn context() -> ExecContext {
    ExecContext::new(
        Instant::now() + Duration::from_secs(3),
        CancellationToken::new(),
        opentelemetry::Context::new(),
    )
}

fn request(name: &str) -> ExecRequest {
    ExecRequest {
        name: name.into(),
        arguments: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn router_unknown_and_expired_requests_do_zero_downstream_work() {
    let fixture = support::start().await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let router = make_router(&fixture.endpoint);
    let unknown = router.exec(request("fixture__missing"), context()).await;
    assert!(matches!(
        unknown,
        Err(latchkey::contracts::GatewayError::UnknownTool(_))
    ));
    let expired = ExecContext::new(
        Instant::now() - Duration::from_millis(1),
        CancellationToken::new(),
        opentelemetry::Context::new(),
    );
    assert!(matches!(
        router.exec(request("fixture__echo"), expired).await,
        Err(latchkey::contracts::GatewayError::Timeout)
    ));
    assert_eq!(fixture.calls(), 0);
}

#[tokio::test]
async fn router_passes_32_calls_concurrently_and_releases_all_admission() {
    let fixture = support::start_with_barrier(32).await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let router = Arc::new(make_router(&fixture.endpoint));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let router = Arc::clone(&router);
        tasks.spawn(async move { router.exec(request("fixture__barrier"), context()).await });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.arrivals() < 32 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("router must admit all 32 calls");
    assert_eq!(fixture.arrivals(), 32);
    fixture.release_barrier();
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(router.active_count(), 0);
}

#[tokio::test]
async fn shutdown_drains_then_cancels_remaining_children_and_closes_admission() {
    let fixture = support::start_with_barrier(1).await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let router = Arc::new(make_router(&fixture.endpoint));
    let running = {
        let router = Arc::clone(&router);
        tokio::spawn(async move { router.exec(request("fixture__barrier"), context()).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.arrivals() < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    fixture.release_barrier();
    running.await.unwrap().unwrap();
    router.shutdown(Duration::from_secs(1)).await;
    assert!(!router.is_accepting());
    assert_eq!(router.active_count(), 0);
    let error = router
        .exec(request("fixture__echo"), context())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        latchkey::contracts::GatewayError::Unavailable(_)
    ));
    assert_eq!(fixture.calls(), 1);

    let fixture = support::start_with_barrier(1).await;
    let router = Arc::new(make_router(&fixture.endpoint));
    let running = {
        let router = Arc::clone(&router);
        tokio::spawn(async move { router.exec(request("fixture__barrier"), context()).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.arrivals() < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let started = Instant::now();
    router.shutdown(Duration::from_millis(50)).await;
    assert!(started.elapsed() < Duration::from_secs(1));
    tokio::time::timeout(Duration::from_secs(1), async {
        while router.active_count() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancelled children release active counters");
    let _ = running.await;
}
