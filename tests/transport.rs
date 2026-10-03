mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use latchkey::contracts::{
    DownstreamIo, ExecContext, NoopTelemetry, ResolvedService, Revision, SensitiveHeaders,
    ServiceId, ServiceSpec,
};
use latchkey::downstream::SdkDownstream;
use tokio_util::sync::CancellationToken;
use url::Url;

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

fn context() -> ExecContext {
    ExecContext::new(
        Instant::now() + Duration::from_secs(3),
        CancellationToken::new(),
        opentelemetry::Context::new(),
    )
}

#[tokio::test]
async fn real_rmcp_fixture_lists_pages_and_preserves_arguments() {
    let fixture = support::start().await;
    let downstream = SdkDownstream::new(reqwest::Client::new(), Arc::new(NoopTelemetry));
    let tools = downstream
        .discover(service(&fixture.endpoint), context())
        .await
        .unwrap();
    assert_eq!(tools.len(), 2);
    let mut args = serde_json::Map::new();
    args.insert("large".into(), serde_json::json!(9_007_199_254_740_993_u64));
    let result = downstream
        .call(service(&fixture.endpoint), "echo".into(), args, context())
        .await
        .unwrap();
    assert!(matches!(result, rmcp::model::CallToolResponse::Complete(_)));
    assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}
