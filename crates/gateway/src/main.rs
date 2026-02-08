use anyhow::Context;
use axum::error_handling::HandleErrorLayer;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::{net::SocketAddr, time::Duration};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tower::timeout::TimeoutLayer;
use tower::{BoxError, ServiceBuilder};
use tower_http::{
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "0.0.0.0:8080";
const MAX_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT_SECONDS: u64 = 5;
const DEFAULT_TOOL_SERVER_URL: &str =
    "http://latchkey-tool-server.latchkey-system.svc.cluster.local:8081";
const DEFAULT_TOKENS: &str = "demo-agent=demo-token";
const DEFAULT_ALLOWLIST: &str = "demo-agent=demo.echo";
const DEFAULT_RATE_LIMIT_PER_MINUTE: usize = 60;

#[derive(Clone)]
struct AppState {
    tool_server_url: String,
    auth_tokens: HashMap<String, String>,
    allowlist: HashMap<String, HashSet<String>>,
    rate_limit_per_minute: usize,
    client: reqwest::Client,
    request_windows: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct MpcRequest {
    tool_name: String,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct MpcResponse {
    request_id: String,
    tool_name: String,
    result: Value,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind = std::env::var("LATCHKEY_GATEWAY_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let addr: SocketAddr = bind.parse().context("invalid LATCHKEY_GATEWAY_BIND value")?;

    let state = AppState::from_env()?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/v1/mcp", post(proxy_mcp))
        .with_state(state)
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

async fn proxy_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MpcRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    let request_id = request_id_from_headers(&headers);
    let tool_name = request.tool_name.clone();

    let Some(principal_id) = principal_id_from_headers(&headers, &state.auth_tokens) else {
        emit_audit(
            &request_id,
            "anonymous",
            &tool_name,
            "deny",
            "error",
            StatusCode::UNAUTHORIZED,
            Some("missing_or_invalid_token"),
            started,
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized", "request_id": request_id})),
        );
    };

    if !is_tool_allowed(&state.allowlist, &principal_id, &tool_name) {
        emit_audit(
            &request_id,
            &principal_id,
            &tool_name,
            "deny",
            "error",
            StatusCode::FORBIDDEN,
            Some("tool_not_allowed"),
            started,
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "forbidden", "request_id": request_id})),
        );
    }

    if !consume_rate_limit(&state, &principal_id).await {
        emit_audit(
            &request_id,
            &principal_id,
            &tool_name,
            "deny",
            "error",
            StatusCode::TOO_MANY_REQUESTS,
            Some("rate_limited"),
            started,
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "rate_limited", "request_id": request_id})),
        );
    }

    let url = format!("{}/v1/tool", state.tool_server_url.trim_end_matches('/'));
    let upstream = state.client.post(url).json(&request).send().await;

    let response = match upstream {
        Ok(response) if response.status().is_success() => {
            let payload =
                response.json::<Value>().await.unwrap_or_else(|_| json!({"status": "ok"}));
            emit_audit(
                &request_id,
                &principal_id,
                &tool_name,
                "allow",
                "success",
                StatusCode::OK,
                None,
                started,
            );
            (StatusCode::OK, Json(json!(MpcResponse { request_id, tool_name, result: payload })))
        }
        Ok(response) => {
            error!(
                request_id = %request_id,
                principal_id = %principal_id,
                tool_name = %tool_name,
                upstream_status = %response.status(),
                "tool server returned non-success status"
            );
            emit_audit(
                &request_id,
                &principal_id,
                &tool_name,
                "allow",
                "error",
                StatusCode::BAD_GATEWAY,
                Some("tool_server_error"),
                started,
            );
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "tool_server_error", "request_id": request_id})),
            )
        }
        Err(err) => {
            error!(
                request_id = %request_id,
                principal_id = %principal_id,
                tool_name = %tool_name,
                error = %err,
                "tool server request failed"
            );
            emit_audit(
                &request_id,
                &principal_id,
                &tool_name,
                "allow",
                "error",
                StatusCode::BAD_GATEWAY,
                Some("tool_server_unreachable"),
                started,
            );
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "tool_server_unreachable", "request_id": request_id})),
            )
        }
    };

    response
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

impl AppState {
    fn from_env() -> anyhow::Result<Self> {
        let tool_server_url = std::env::var("LATCHKEY_TOOL_SERVER_URL")
            .unwrap_or_else(|_| DEFAULT_TOOL_SERVER_URL.to_string());

        let tokens =
            std::env::var("LATCHKEY_STATIC_TOKENS").unwrap_or_else(|_| DEFAULT_TOKENS.to_string());
        let auth_tokens = parse_tokens(&tokens).context("invalid LATCHKEY_STATIC_TOKENS")?;

        let allowlist = std::env::var("LATCHKEY_TOOL_ALLOWLIST")
            .unwrap_or_else(|_| DEFAULT_ALLOWLIST.to_string());
        let allowlist = parse_allowlist(&allowlist).context("invalid LATCHKEY_TOOL_ALLOWLIST")?;

        let rate_limit_per_minute = std::env::var("LATCHKEY_RATE_LIMIT_PER_MINUTE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
            .build()
            .context("failed to construct http client")?;

        Ok(Self {
            tool_server_url,
            auth_tokens,
            allowlist,
            rate_limit_per_minute,
            client,
            request_windows: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

fn request_id_from_headers(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_string()
}

fn principal_id_from_headers(
    headers: &HeaderMap,
    tokens: &HashMap<String, String>,
) -> Option<String> {
    let bearer =
        headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")?;

    tokens.get(bearer).cloned()
}

fn parse_tokens(input: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut tokens = HashMap::new();

    for pair in input.split(',') {
        if pair.trim().is_empty() {
            continue;
        }

        let (principal, token) =
            pair.split_once('=').context("token entries must be principal=token")?;
        tokens.insert(token.trim().to_string(), principal.trim().to_string());
    }

    Ok(tokens)
}

fn parse_allowlist(input: &str) -> anyhow::Result<HashMap<String, HashSet<String>>> {
    let mut allowlist = HashMap::new();

    for pair in input.split(',') {
        if pair.trim().is_empty() {
            continue;
        }

        let (principal, tools) =
            pair.split_once('=').context("allowlist entries must be principal=tool1|tool2")?;

        let tools: HashSet<String> = tools
            .split('|')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect();

        allowlist.insert(principal.trim().to_string(), tools);
    }

    Ok(allowlist)
}

fn is_tool_allowed(
    allowlist: &HashMap<String, HashSet<String>>,
    principal_id: &str,
    tool_name: &str,
) -> bool {
    allowlist.get(principal_id).map(|tools| tools.contains(tool_name)).unwrap_or(false)
}

async fn consume_rate_limit(state: &AppState, principal_id: &str) -> bool {
    let mut windows = state.request_windows.lock().await;
    let window = windows.entry(principal_id.to_string()).or_default();
    let now = Instant::now();

    while let Some(entry) = window.front() {
        if now.duration_since(*entry) >= Duration::from_secs(60) {
            window.pop_front();
        } else {
            break;
        }
    }

    if window.len() >= state.rate_limit_per_minute {
        return false;
    }

    window.push_back(now);
    true
}

#[allow(clippy::too_many_arguments)]
fn emit_audit(
    request_id: &str,
    principal_id: &str,
    tool_name: &str,
    decision: &str,
    outcome: &str,
    status: StatusCode,
    deny_reason: Option<&str>,
    started: Instant,
) {
    let latency_ms = started.elapsed().as_millis() as u64;
    let deny_reason = deny_reason.unwrap_or("");

    info!(
        event_type = "audit",
        request_id,
        principal_id,
        tool_name,
        decision,
        outcome,
        deny_reason,
        status = %status,
        latency_ms,
        "mcp decision"
    );
}
