//! Real rmcp Streamable HTTP fixture used by transport tests.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use axum::http::{Request, StatusCode};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    middleware::Next,
    response::{IntoResponse, Response},
    serve,
};
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::json;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

#[derive(Clone)]
struct FixtureHandler {
    state: Arc<FixtureState>,
}

struct FixtureState {
    calls: AtomicUsize,
    arrivals: AtomicUsize,
    side_effects: AtomicUsize,
    barrier_release: Notify,
    authorized: AtomicBool,
    expire_call_once: AtomicBool,
    traceparent_seen: std::sync::Mutex<Vec<String>>,
}

fn tool(name: &'static str, description: &'static str) -> Tool {
    Tool::new(
        name,
        description,
        json!({"type": "object"}).as_object().unwrap().clone(),
    )
}

impl ServerHandler for FixtureHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("latchkey-fixture", "0.1.0"))
    }

    #[allow(clippy::manual_async_fn)]
    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            let page = request.and_then(|params| params.cursor).unwrap_or_default();
            let mut result = match page.as_str() {
                "page-2" => {
                    ListToolsResult::with_all_items(vec![tool("structured", "structured response")])
                }
                "page-3" => ListToolsResult::with_all_items(vec![tool("mixed", "mixed response")]),
                "page-4" => ListToolsResult::with_all_items(vec![tool("barrier", "barrier call")]),
                _ => ListToolsResult::with_all_items(vec![tool("echo", "echo arguments")]),
            };
            result.next_cursor = match page.as_str() {
                "" => Some("page-2".into()),
                "page-2" => Some("page-3".into()),
                "page-3" => Some("page-4".into()),
                _ => None,
            };
            Ok(result)
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResponse, rmcp::ErrorData>> + Send + '_
    {
        async move {
            self.state.calls.fetch_add(1, Ordering::Relaxed);
            if let Some(traceparent) = _context.meta.get_traceparent() {
                self.state
                    .traceparent_seen
                    .lock()
                    .unwrap()
                    .push(traceparent.to_owned());
            }
            match request.name.as_ref() {
                "echo" => Ok(CallToolResult::structured(
                    json!({"arguments": request.arguments.unwrap_or_default()}),
                )
                .into()),
                "structured" => {
                    Ok(CallToolResult::structured(json!({"ok": true, "duration_ms": 1})).into())
                }
                "mixed" => Ok(CallToolResult::structured(json!({
                    "structured": {"ok": true},
                }))
                .with_meta(Some(rmcp::model::MetaObject(
                    json!({"fixture": "mixed"}).as_object().unwrap().clone(),
                )))
                .into()),
                "is_error" => Ok(CallToolResult::error(vec![]).into()),
                "side_effect" => {
                    self.state.side_effects.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Ok(CallToolResult::structured(json!({"side_effect": true})).into())
                }
                "barrier" => {
                    self.state.arrivals.fetch_add(1, Ordering::SeqCst);
                    self.state.barrier_release.notified().await;
                    Ok(CallToolResult::structured(json!({"released": true})).into())
                }
                _ => Err(rmcp::ErrorData::invalid_params(
                    "fixture tool not found",
                    None,
                )),
            }
        }
    }
}

/// A running real HTTP MCP server. Dropping it aborts its listener task.
pub struct Fixture {
    pub endpoint: String,
    state: Arc<FixtureState>,
    task: JoinHandle<()>,
}

#[allow(dead_code)]
impl Fixture {
    pub fn calls(&self) -> usize {
        self.state.calls.load(Ordering::SeqCst)
    }
    pub fn arrivals(&self) -> usize {
        self.state.arrivals.load(Ordering::SeqCst)
    }
    pub fn side_effects(&self) -> usize {
        self.state.side_effects.load(Ordering::SeqCst)
    }
    pub fn release_barrier(&self) {
        self.state.barrier_release.notify_waiters();
    }
    pub fn authorized(&self) -> bool {
        self.state.authorized.load(Ordering::SeqCst)
    }
    pub fn traceparents(&self) -> Vec<String> {
        self.state.traceparent_seen.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start() -> Fixture {
    start_with_options(0, false).await
}

pub async fn start_with_barrier(_barrier_target: usize) -> Fixture {
    start_with_options(_barrier_target, false).await
}

#[allow(dead_code)]
pub async fn start_with_expired_call() -> Fixture {
    start_with_options(0, true).await
}

async fn start_with_options(_barrier_target: usize, expire_call_once: bool) -> Fixture {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(FixtureState {
        calls: AtomicUsize::new(0),
        arrivals: AtomicUsize::new(0),
        side_effects: AtomicUsize::new(0),
        barrier_release: Notify::new(),
        authorized: AtomicBool::new(false),
        expire_call_once: AtomicBool::new(expire_call_once),
        traceparent_seen: std::sync::Mutex::new(Vec::new()),
    });
    let handler = FixtureHandler {
        state: state.clone(),
    };
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_json_response(true),
    );
    let app = Router::new()
        .fallback_service(service)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));
    let task = tokio::spawn(async move {
        let _ = serve(listener, app.into_make_service()).await;
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    Fixture {
        endpoint: format!("http://{address}/mcp"),
        state,
        task,
    }
}

async fn auth_middleware(
    State(state): State<Arc<FixtureState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request
        .headers()
        .get("authorization")
        .is_some_and(|value| value == "Bearer fixture-token")
    {
        state.authorized.store(true, Ordering::SeqCst);
    }
    if state.expire_call_once.load(Ordering::SeqCst) {
        let (parts, body) = request.into_parts();
        let bytes = to_bytes(body, 2 * 1024 * 1024).await.unwrap_or_default();
        if bytes.windows(10).any(|window| window == b"tools/call")
            && state.expire_call_once.swap(false, Ordering::SeqCst)
        {
            return (StatusCode::NOT_FOUND, "expired session").into_response();
        }
        return next
            .run(Request::from_parts(parts, Body::from(bytes)))
            .await;
    }
    next.run(request).await
}
