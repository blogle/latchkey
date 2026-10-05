//! Public MCP server adapter. Protocol framing and Streamable HTTP are owned by rmcp.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::contracts::{ExecContext, ExecRequest, GatewayApi, GatewayError, SearchRequest};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any_service;
#[allow(deprecated)]
use ring::constant_time::verify_slices_are_equal;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    ProtocolVersion, ServerCapabilities, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use schemars::schema_for;
use serde_json::{Map, Value};

/// Optional process-level authentication. The token is retained only in memory
/// and is never included in Debug output or error responses.
#[derive(Clone, Default)]
pub struct AuthConfig {
    bearer_token: Option<Arc<str>>,
}

impl AuthConfig {
    pub fn unauthenticated() -> Self {
        Self::default()
    }

    pub fn bearer(token: impl Into<Arc<str>>) -> Self {
        Self {
            bearer_token: Some(token.into()),
        }
    }
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Public MCP server configuration and router factory.
#[derive(Clone)]
pub struct McpServer {
    gateway: Arc<dyn GatewayApi>,
    auth: AuthConfig,
    hosts: Vec<String>,
    deadline: Duration,
}

impl McpServer {
    pub fn new(gateway: Arc<dyn GatewayApi>, auth: AuthConfig, hosts: Vec<String>) -> Self {
        Self {
            gateway,
            auth,
            hosts,
            deadline: Duration::from_secs(180),
        }
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Build the `/mcp` Streamable HTTP endpoint. rmcp owns all MCP framing,
    /// version negotiation, SSE/JSON behavior, and cancellation plumbing.
    pub fn router(&self) -> Router {
        let handler = GatewayHandler {
            gateway: Arc::clone(&self.gateway),
            deadline: self.deadline,
        };
        let mut config = StreamableHttpServerConfig::default().with_legacy_session_mode(false);
        if !self.hosts.is_empty() {
            config = config.with_allowed_hosts(self.hosts.clone());
        }
        let service = StreamableHttpService::new(
            move || Ok(handler.clone()),
            Arc::new(NeverSessionManager::default()),
            config,
        );
        let auth = self.auth.clone();
        Router::new()
            .route("/mcp", any_service(service))
            .layer(middleware::from_fn(move |request, next| {
                authenticate(request, next, auth.clone())
            }))
    }

    pub async fn serve(self, listeners: Vec<SocketAddr>) -> Result<(), std::io::Error> {
        let router = self.router();
        let listener = tokio::net::TcpListener::bind(
            listeners
                .first()
                .copied()
                .unwrap_or(([127, 0, 0, 1], 8080).into()),
        )
        .await?;
        axum::serve(listener, router).await
    }
}

#[derive(Clone)]
struct GatewayHandler {
    gateway: Arc<dyn GatewayApi>,
    deadline: Duration,
}

impl GatewayHandler {
    fn tools() -> Vec<Tool> {
        let search_schema = serde_json::to_value(schema_for!(SearchRequest))
            .expect("SearchRequest schema is serializable");
        let exec_schema = serde_json::to_value(schema_for!(ExecRequest))
            .expect("ExecRequest schema is serializable");
        vec![
            Tool::new(
                "search",
                "Search the current downstream tool catalog.",
                object_schema(search_schema),
            ),
            Tool::new(
                "exec",
                "Execute one fully qualified downstream tool.",
                object_schema(exec_schema),
            ),
        ]
    }
}

impl ServerHandler for GatewayHandler {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![
            ProtocolVersion::V_2026_07_28,
            ProtocolVersion::V_2025_11_25,
            ProtocolVersion::V_2025_06_18,
        ])
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("latchkey", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(Self::tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let name = request.name.to_string();
        let arguments = request.arguments.unwrap_or_default();
        let exec_context = ExecContext::new(
            Instant::now() + self.deadline,
            context.ct.clone(),
            opentelemetry::Context::current(),
        );
        let result = if name == "search" {
            let request = serde_json::from_value::<SearchRequest>(Value::Object(arguments))
                .map_err(|_| {
                    stable_error(
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        "invalid-input",
                        "invalid search arguments",
                    )
                })?;
            tokio::select! {
                _ = context.ct.cancelled() => Err(GatewayError::Cancelled),
                result = tokio::time::timeout(self.deadline, self.gateway.search(request)) => {
                    match result {
                        Ok(result) => result.map(search_result),
                        Err(_) => Err(GatewayError::Timeout),
                    }
                },
            }
        } else if name == "exec" {
            let request =
                serde_json::from_value::<ExecRequest>(Value::Object(arguments)).map_err(|_| {
                    stable_error(
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        "invalid-input",
                        "invalid exec arguments",
                    )
                })?;
            tokio::select! {
                _ = context.ct.cancelled() => Err(GatewayError::Cancelled),
                result = tokio::time::timeout(self.deadline, self.gateway.exec(request, exec_context)) => {
                    match result {
                        Ok(result) => result,
                        Err(_) => Err(GatewayError::Timeout),
                    }
                },
            }
        } else {
            return Err(stable_error(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                "unknown-tool",
                "unknown gateway tool",
            ));
        };
        result.map_err(to_mcp_error)
    }
}

fn object_schema(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn search_result(hits: Vec<crate::contracts::SearchHit>) -> CallToolResponse {
    let value = serde_json::to_value(hits).unwrap_or_else(|_| Value::Array(Vec::new()));
    CallToolResponse::Complete(CallToolResult::structured(value))
}

fn to_mcp_error(error: GatewayError) -> rmcp::ErrorData {
    match error {
        GatewayError::Downstream(error) => error,
        GatewayError::InvalidInput(_) => stable_error(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "invalid-input",
            "invalid gateway input",
        ),
        GatewayError::UnknownTool(_) => stable_error(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "unknown-downstream-tool",
            "unknown downstream tool",
        ),
        GatewayError::Timeout => stable_error(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "timeout",
            "gateway deadline exceeded",
        ),
        GatewayError::Cancelled => stable_error(
            rmcp::model::ErrorCode::INVALID_REQUEST,
            "cancelled",
            "request cancelled",
        ),
        GatewayError::Overloaded => stable_error(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "overloaded",
            "gateway request unavailable",
        ),
        GatewayError::Unavailable(_) => stable_error(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "unavailable",
            "gateway request unavailable",
        ),
        GatewayError::UnsupportedCapability(_) => stable_error(
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "unsupported-downstream-capability",
            "unsupported downstream capability",
        ),
    }
}

fn stable_error(
    code: rmcp::model::ErrorCode,
    category: &'static str,
    message: &'static str,
) -> rmcp::ErrorData {
    rmcp::ErrorData::new(
        code,
        message,
        Some(serde_json::json!({ "category": category })),
    )
}

async fn authenticate(request: Request<Body>, next: Next, auth: AuthConfig) -> Response {
    let Some(expected) = auth.bearer_token else {
        return next.run(request).await;
    };
    let valid = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| constant_time_equal(value.as_bytes(), expected.as_bytes()));
    if valid {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

#[allow(deprecated)]
fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    verify_slices_are_equal(left, right).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertises_exactly_two_tools_with_complete_schemas() {
        let tools = GatewayHandler::tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            ["search", "exec"]
        );
        for tool in tools {
            assert_eq!(tool.schema_as_json_value()["type"], "object");
            assert!(tool.schema_as_json_value()["properties"].is_object());
        }
    }

    #[test]
    fn auth_debug_redacts_token() {
        let debug = format!("{:?}", AuthConfig::bearer("secret-token"));
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("REDACTED"));
    }
}
