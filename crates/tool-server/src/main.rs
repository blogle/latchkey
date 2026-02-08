use anyhow::Context;
use axum::{extract::State, http::StatusCode, routing::get, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "0.0.0.0:8081";
const DEFAULT_UPSTREAM_URL: &str =
    "http://latchkey-upstream-stub.latchkey-system.svc.cluster.local:8082/v1/upstream";
const FALLBACK_UPSTREAM_KEY: &str = "dev-upstream-key";

#[derive(Clone)]
struct AppState {
    upstream_url: String,
    upstream_api_key: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize, Serialize)]
struct ToolRequest {
    tool_name: String,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    params: Value,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind =
        std::env::var("LATCHKEY_TOOL_SERVER_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let addr: SocketAddr = bind.parse().context("invalid LATCHKEY_TOOL_SERVER_BIND value")?;

    let state = AppState::from_env()?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/tool", post(call_tool))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind tool server listener on {addr}"))?;

    info!(%addr, "tool server booted");
    axum::serve(listener, app).await.context("tool server failed")
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn call_tool(
    State(state): State<AppState>,
    Json(request): Json<ToolRequest>,
) -> (StatusCode, Json<Value>) {
    let response = state
        .client
        .post(&state.upstream_url)
        .header("x-api-key", &state.upstream_api_key)
        .json(&request)
        .send()
        .await;

    match response {
        Ok(response) if response.status().is_success() => {
            let payload =
                response.json::<Value>().await.unwrap_or_else(|_| json!({"status": "ok"}));

            (
                StatusCode::OK,
                Json(json!({
                    "tool_name": request.tool_name,
                    "operation": request.operation,
                    "upstream": payload,
                })),
            )
        }
        Ok(response) => {
            error!(status = %response.status(), "upstream returned error status");
            (StatusCode::BAD_GATEWAY, Json(json!({"error": "upstream_error"})))
        }
        Err(err) => {
            error!(error = %err, "upstream request failed");
            (StatusCode::BAD_GATEWAY, Json(json!({"error": "upstream_unreachable"})))
        }
    }
}

impl AppState {
    fn from_env() -> anyhow::Result<Self> {
        let upstream_url = std::env::var("LATCHKEY_UPSTREAM_URL")
            .unwrap_or_else(|_| DEFAULT_UPSTREAM_URL.to_string());
        let upstream_api_key = std::env::var("UPSTREAM_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| FALLBACK_UPSTREAM_KEY.to_string());

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .context("failed to construct upstream client")?;

        Ok(Self { upstream_url, upstream_api_key, client })
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
