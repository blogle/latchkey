use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, serve};
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::json;

#[derive(Clone)]
struct Handler;

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("latchkey-fixture", "0.1.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "echo",
            "Return the supplied fixture message",
            json!({"type": "object", "properties": {"message": {"type": "string"}}})
                .as_object()
                .expect("schema object")
                .clone(),
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        if request.name != "echo" {
            return Err(rmcp::ErrorData::invalid_params(
                "unknown fixture tool",
                None,
            ));
        }
        Ok(CallToolResult::structured(json!({
            "fixture": "echo",
            "arguments": request.arguments.unwrap_or_default(),
        }))
        .into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    latchkey::runtime::install_crypto_provider();
    let address: SocketAddr = std::env::args().nth(1).expect("listen address").parse()?;
    let service = StreamableHttpService::new(
        || Ok(Handler),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_json_response(true),
    );
    let listener = tokio::net::TcpListener::bind(address).await?;
    serve(
        listener,
        Router::new().fallback_service(service).into_make_service(),
    )
    .await?;
    Ok(())
}
