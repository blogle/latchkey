//! Real rmcp Streamable HTTP fixture used by transport tests.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use axum::serve;
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
use tokio::task::JoinHandle;

#[derive(Clone)]
struct FixtureHandler {
    calls: Arc<AtomicUsize>,
}

fn tool(name: &'static str, description: &'static str) -> Tool {
    Tool::new(
        name,
        description,
        json!({"type": "object"}).as_object().unwrap().clone(),
    )
}

#[async_trait]
impl ServerHandler for FixtureHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::new("latchkey-fixture", "0.1.0"),
            instructions: None,
            meta: None,
        }
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let page = request
            .and_then(|params| params.cursor)
            .as_deref()
            .unwrap_or("");
        let mut result = match page {
            "page-2" => {
                ListToolsResult::with_all_items(vec![tool("structured", "structured response")])
            }
            _ => ListToolsResult::with_all_items(vec![tool("echo", "echo arguments")]),
        };
        if page.is_empty() {
            result.next_cursor = Some("page-2".into());
        }
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        match request.name.as_ref() {
            "echo" => Ok(CallToolResult::structured(
                json!({"arguments": request.arguments.unwrap_or_default()}),
            )
            .into()),
            "structured" => {
                Ok(CallToolResult::structured(json!({"ok": true, "duration_ms": 1})).into())
            }
            _ => Err(rmcp::ErrorData::method_not_found(
                "fixture tool not found",
                None,
            )),
        }
    }
}

/// A running real HTTP MCP server. Dropping it aborts its listener task.
pub struct Fixture {
    pub endpoint: String,
    pub calls: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start() -> Fixture {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = FixtureHandler {
        calls: calls.clone(),
    };
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_json_response(true),
    );
    let task = tokio::spawn(async move {
        let _ = serve(listener, service).await;
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    Fixture {
        endpoint: format!("http://{address}/mcp"),
        calls,
        task,
    }
}
