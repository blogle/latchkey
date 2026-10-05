mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use latchkey::contracts::{
    DownstreamIo, ExecContext, NoopTelemetry, ResolvedService, Revision, SensitiveHeaders,
    ServiceId, ServiceSpec,
};
use latchkey::downstream::SdkDownstream;
use opentelemetry::trace::{SpanContext, TraceContextExt, TraceFlags, TraceState};
use opentelemetry::{SpanId, TraceId};
use tokio_util::sync::CancellationToken;
use url::Url;

fn service(endpoint: &str) -> Arc<ResolvedService> {
    service_with_headers(endpoint, Vec::new())
}

fn service_with_headers(endpoint: &str, headers: Vec<(&str, &str)>) -> Arc<ResolvedService> {
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
        headers: SensitiveHeaders::new(
            headers
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        ),
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
    let _ = rustls::crypto::ring::default_provider().install_default();
    let downstream = SdkDownstream::new(reqwest::Client::new(), Arc::new(NoopTelemetry));
    let tools = downstream
        .discover(service(&fixture.endpoint), context())
        .await
        .unwrap();
    assert_eq!(tools.len(), 4);
    let mut args = serde_json::Map::new();
    args.insert("large".into(), serde_json::json!(9_007_199_254_740_993_u64));
    args.insert(
        "nested".into(),
        serde_json::json!({"unicode": "héllo ✓ 日本語", "items": [1, 2, 3]}),
    );
    let expected_args = args.clone();
    let result = downstream
        .call(service(&fixture.endpoint), "echo".into(), args, context())
        .await
        .unwrap();
    match result {
        rmcp::model::CallToolResponse::Complete(result) => {
            assert_eq!(
                result.structured_content,
                Some(serde_json::json!({"arguments": expected_args}))
            );
        }
        other => panic!("unexpected echo response: {other:?}"),
    }
    assert_eq!(fixture.calls(), 1);

    let structured = downstream
        .call(
            service(&fixture.endpoint),
            "structured".into(),
            serde_json::Map::new(),
            context(),
        )
        .await
        .unwrap();
    let mixed = downstream
        .call(
            service(&fixture.endpoint),
            "mixed".into(),
            serde_json::Map::new(),
            context(),
        )
        .await
        .unwrap();
    let is_error = downstream
        .call(
            service(&fixture.endpoint),
            "is_error".into(),
            serde_json::Map::new(),
            context(),
        )
        .await
        .unwrap();
    match structured {
        rmcp::model::CallToolResponse::Complete(result) => {
            assert_eq!(
                result.structured_content,
                Some(serde_json::json!({"ok": true, "duration_ms": 1}))
            );
        }
        other => panic!("unexpected structured response: {other:?}"),
    }
    match mixed {
        rmcp::model::CallToolResponse::Complete(result) => {
            assert_eq!(
                result.meta.and_then(|meta| meta.0.get("fixture").cloned()),
                Some(serde_json::json!("mixed"))
            );
        }
        other => panic!("unexpected mixed response: {other:?}"),
    }
    match is_error {
        rmcp::model::CallToolResponse::Complete(result) => assert_eq!(result.is_error, Some(true)),
        other => panic!("unexpected error response: {other:?}"),
    }
}

#[tokio::test]
async fn static_headers_and_traceparent_reach_the_real_fixture() {
    let fixture = support::start().await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let downstream = SdkDownstream::new(reqwest::Client::new(), Arc::new(NoopTelemetry));
    let span_context = SpanContext::new(
        TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap(),
        SpanId::from_hex("00f067aa0ba902b7").unwrap(),
        TraceFlags::SAMPLED,
        false,
        TraceState::default(),
    );
    let trace = opentelemetry::Context::new().with_remote_span_context(span_context);
    let context = ExecContext::new(
        Instant::now() + Duration::from_secs(3),
        CancellationToken::new(),
        trace,
    );
    downstream
        .call(
            service_with_headers(
                &fixture.endpoint,
                vec![("authorization", "Bearer fixture-token")],
            ),
            "echo".into(),
            serde_json::Map::new(),
            context,
        )
        .await
        .unwrap();
    assert!(fixture.authorized());
    assert_eq!(
        fixture.traceparents(),
        vec!["00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01"]
    );
}

#[tokio::test]
async fn thirty_two_same_service_calls_reach_barrier_before_release() {
    let fixture = support::start_with_barrier(32).await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let downstream = Arc::new(SdkDownstream::new(
        reqwest::Client::new(),
        Arc::new(NoopTelemetry),
    ));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let downstream = Arc::clone(&downstream);
        let service = service(&fixture.endpoint);
        tasks.spawn(async move {
            downstream
                .call(service, "barrier".into(), serde_json::Map::new(), context())
                .await
        });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.arrivals() < 32 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("all 32 calls must arrive before release");
    assert_eq!(fixture.arrivals(), 32);
    fixture.release_barrier();
    let mut completed = 0;
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
        completed += 1;
    }
    assert_eq!(completed, 32);
}

#[tokio::test]
async fn cancellation_does_not_block_other_calls_and_side_effect_is_not_replayed() {
    let fixture = support::start_with_barrier(31).await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let downstream = Arc::new(SdkDownstream::new(
        reqwest::Client::new(),
        Arc::new(NoopTelemetry),
    ));
    let cancelled = CancellationToken::new();
    let cancelled_context = ExecContext::new(
        Instant::now() + Duration::from_secs(3),
        cancelled.clone(),
        opentelemetry::Context::new(),
    );
    let first = {
        let downstream = Arc::clone(&downstream);
        let service = service(&fixture.endpoint);
        tokio::spawn(async move {
            downstream
                .call(
                    service,
                    "barrier".into(),
                    serde_json::Map::new(),
                    cancelled_context,
                )
                .await
        })
    };
    let mut others = tokio::task::JoinSet::new();
    for _ in 0..31 {
        let downstream = Arc::clone(&downstream);
        let service = service(&fixture.endpoint);
        others.spawn(async move {
            downstream
                .call(service, "barrier".into(), serde_json::Map::new(), context())
                .await
        });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.arrivals() < 32 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("all calls reached the barrier");
    cancelled.cancel();
    fixture.release_barrier();
    assert!(first.await.unwrap().is_err());
    while let Some(result) = others.join_next().await {
        result.unwrap().unwrap();
    }

    let timeout_context = ExecContext::new(
        Instant::now() + Duration::from_millis(20),
        CancellationToken::new(),
        opentelemetry::Context::new(),
    );
    let error = downstream
        .call(
            service(&fixture.endpoint),
            "side_effect".into(),
            serde_json::Map::new(),
            timeout_context,
        )
        .await
        .expect_err("side effect call should time out");
    assert!(matches!(error, latchkey::contracts::GatewayError::Timeout));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fixture.side_effects(), 1);
}

#[tokio::test]
async fn expired_session_does_not_replay_a_mutating_call() {
    let fixture = support::start_with_expired_call().await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let downstream = SdkDownstream::new(reqwest::Client::new(), Arc::new(NoopTelemetry));
    let error = downstream
        .call(
            service(&fixture.endpoint),
            "side_effect".into(),
            serde_json::Map::new(),
            context(),
        )
        .await
        .expect_err("expired session must fail without replay");
    assert!(matches!(
        error,
        latchkey::contracts::GatewayError::Unavailable(_)
    ));
    assert_eq!(fixture.side_effects(), 0);
    downstream
        .call(
            service(&fixture.endpoint),
            "side_effect".into(),
            serde_json::Map::new(),
            context(),
        )
        .await
        .unwrap();
    assert_eq!(fixture.side_effects(), 1);
}
