use anyhow::Context;
use axum::error_handling::HandleErrorLayer;
use axum::{http::StatusCode, routing::get, Router};
use std::{net::SocketAddr, time::Duration};
use tower::timeout::TimeoutLayer;
use tower::{BoxError, ServiceBuilder};
use tower_http::{
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "0.0.0.0:8080";
const MAX_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT_SECONDS: u64 = 5;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind = std::env::var("LATCHKEY_GATEWAY_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let addr: SocketAddr = bind.parse().context("invalid LATCHKEY_GATEWAY_BIND value")?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_timeout_error))
                .layer(TimeoutLayer::new(Duration::from_secs(REQUEST_TIMEOUT_SECONDS)))
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(PropagateRequestIdLayer::x_request_id())
                .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
                .layer(TraceLayer::new_for_http()),
        );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind gateway listener on {addr}"))?;

    info!(%addr, "gateway booted");
    axum::serve(listener, app).await.context("gateway server failed")
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn readyz() -> StatusCode {
    StatusCode::OK
}

async fn metrics() -> &'static str {
    "# latchkey metrics placeholder\nlatchkey_requests_total 0\n"
}

async fn handle_timeout_error(error: BoxError) -> (StatusCode, &'static str) {
    if error.is::<tower::timeout::error::Elapsed>() {
        (StatusCode::REQUEST_TIMEOUT, "request timed out")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "internal gateway error")
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
