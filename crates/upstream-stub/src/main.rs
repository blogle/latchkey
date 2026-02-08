use anyhow::Context;
use axum::{http::HeaderMap, http::StatusCode, routing::get, routing::post, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "0.0.0.0:8082";
const FALLBACK_EXPECTED_KEY: &str = "dev-upstream-key";

#[derive(Debug, Deserialize)]
struct UpstreamRequest {
    tool_name: String,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    params: Value,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind = std::env::var("LATCHKEY_UPSTREAM_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let addr: SocketAddr = bind.parse().context("invalid LATCHKEY_UPSTREAM_BIND value")?;

    let app =
        Router::new().route("/healthz", get(healthz)).route("/v1/upstream", post(upstream_call));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind upstream listener on {addr}"))?;

    info!(%addr, "upstream stub booted");
    axum::serve(listener, app).await.context("upstream stub failed")
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn upstream_call(
    headers: HeaderMap,
    Json(request): Json<UpstreamRequest>,
) -> (StatusCode, Json<Value>) {
    let expected = std::env::var("EXPECTED_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| FALLBACK_EXPECTED_KEY.to_string());

    let provided =
        headers.get("x-api-key").and_then(|value| value.to_str().ok()).unwrap_or_default();

    if provided != expected {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid_api_key"})));
    }

    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "tool_name": request.tool_name,
            "operation": request.operation,
            "summary": "stub upstream call succeeded",
            "params_echo": request.params,
        })),
    )
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
