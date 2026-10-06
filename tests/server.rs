//! Official rmcp client coverage for the standalone public MCP server.

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::http::header::HOST;
use latchkey::contracts::{
    ExecContext, ExecRequest, GatewayApi, GatewayError, SearchHit, SearchRequest,
};
use latchkey::server::{AuthConfig, McpServer};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientInfo,
    ErrorCode, Implementation, MetaObject, ProtocolVersion,
};
use rmcp::service::{ServiceError, serve_client, serve_client_with_ct};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use serde_json::{Map, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct FakeGateway {
    calls: Arc<Mutex<Vec<String>>>,
    delay: Option<Duration>,
    error: Option<GatewayError>,
    cancelled: Option<Arc<AtomicBool>>,
}

#[async_trait]
impl GatewayApi for FakeGateway {
    async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, GatewayError> {
        self.calls.lock().expect("calls lock").push(request.query);
        Ok(vec![SearchHit {
            name: "anvil__session_create".to_owned(),
            service: "anvil".to_owned(),
            description: "Create a session".to_owned(),
            input_schema: {
                let mut schema = Map::new();
                schema.insert("type".to_owned(), json!("object"));
                schema.insert("required".to_owned(), json!(["project"]));
                schema.insert(
                    "properties".to_owned(),
                    json!({"project": {"type": "string"}}),
                );
                schema
            },
        }])
    }

    async fn exec(
        &self,
        _request: ExecRequest,
        context: ExecContext,
    ) -> Result<CallToolResponse, GatewayError> {
        if let Some(delay) = self.delay {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = context.cancellation.cancelled() => {
                    if let Some(cancelled) = &self.cancelled {
                        cancelled.store(true, Ordering::Release);
                    }
                    return Err(GatewayError::Cancelled);
                }
            }
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let mut meta = Map::new();
        meta.insert("fixture".to_owned(), json!(true));
        Ok(CallToolResult::structured(json!({"created": true}))
            .with_meta(Some(MetaObject(meta)))
            .into())
    }
}

async fn start_server(
    gateway: FakeGateway,
    auth: AuthConfig,
    hosts: Vec<String>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let router = McpServer::new(Arc::new(gateway), auth, hosts)
        .with_deadline(Duration::from_millis(50))
        .router();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    (format!("http://{address}/mcp"), task)
}

fn install_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("install ring crypto provider");
    });
}

async fn client_for(
    uri: &str,
    version: ProtocolVersion,
    token: Option<&str>,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ClientInfo> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(uri);
    config.allow_stateless = true;
    config.auth_header = token.map(str::to_owned);
    let transport = StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
    let info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("latchkey-test-client", "1"),
    )
    .with_protocol_version(version);
    serve_client(info, transport)
        .await
        .expect("official rmcp client initialize")
}

#[tokio::test]
async fn official_client_negotiates_all_supported_protocol_versions() {
    install_crypto_provider();
    for version in [
        ProtocolVersion::V_2026_07_28,
        ProtocolVersion::V_2025_11_25,
        ProtocolVersion::V_2025_06_18,
    ] {
        let (uri, task) = start_server(
            FakeGateway::default(),
            AuthConfig::unauthenticated(),
            vec!["127.0.0.1".to_owned()],
        )
        .await;
        let client = client_for(&uri, version.clone(), None).await;
        let tools = client.list_tools(None).await.expect("list tools");
        assert_eq!(
            tools
                .tools
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            ["search", "exec"]
        );
        let search_tool = tools
            .tools
            .iter()
            .find(|tool| tool.name == "search")
            .expect("search schema");
        assert_eq!(
            search_tool.schema_as_json_value()["properties"]["query"]["type"],
            "string"
        );
        let result = client
            .call_tool(
                CallToolRequestParams::new("search")
                    .with_arguments(json!({"query": "session"}).as_object().unwrap().clone()),
            )
            .await
            .expect("search");
        assert!(result.structured_content.is_some());
        assert!(!result.content.is_empty());
        drop(client);
        task.abort();
    }
}

#[tokio::test]
async fn bearer_and_allowed_host_controls_are_enforced() {
    install_crypto_provider();
    let (uri, task) = start_server(
        FakeGateway::default(),
        AuthConfig::bearer("top-secret"),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let response = reqwest::Client::new()
        .get(&uri)
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);

    let client = client_for(&uri, ProtocolVersion::V_2025_11_25, Some("top-secret")).await;
    assert_eq!(
        client
            .list_tools(None)
            .await
            .expect("authorized list")
            .tools
            .len(),
        2
    );
    drop(client);

    let response = reqwest::Client::new()
        .get(&uri)
        .header(HOST, "evil.example")
        .header("authorization", "Bearer top-secret")
        .send()
        .await
        .expect("host request");
    assert!(response.status().is_client_error());
    task.abort();
}

async fn get_with_host(uri: &str, host: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(uri)
        .header(HOST, host)
        .send()
        .await
        .expect("host request")
        .status()
}

#[tokio::test]
async fn default_and_explicit_host_allowlists_are_enforced() {
    install_crypto_provider();

    let (uri, task) = start_server(
        FakeGateway::default(),
        AuthConfig::unauthenticated(),
        Vec::new(),
    )
    .await;
    assert_eq!(
        get_with_host(&uri, "localhost").await,
        reqwest::StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        get_with_host(&uri, "unlisted.example").await,
        reqwest::StatusCode::FORBIDDEN
    );
    task.abort();

    let (uri, task) = start_server(
        FakeGateway::default(),
        AuthConfig::unauthenticated(),
        vec!["cluster.example".to_owned(), "ingress.example".to_owned()],
    )
    .await;
    for host in ["cluster.example", "ingress.example"] {
        assert_eq!(
            get_with_host(&uri, host).await,
            reqwest::StatusCode::METHOD_NOT_ALLOWED,
            "explicit host {host} should be allowed"
        );
    }
    assert_eq!(
        get_with_host(&uri, "unlisted.example").await,
        reqwest::StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn structured_results_downstream_errors_and_deadlines_survive_the_adapter() {
    install_crypto_provider();
    let (uri, task) = start_server(
        FakeGateway::default(),
        AuthConfig::unauthenticated(),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let client = client_for(&uri, ProtocolVersion::V_2026_07_28, None).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("exec").with_arguments(
                json!({"name": "anvil__session_create"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("exec");
    assert_eq!(result.structured_content, Some(json!({"created": true})));
    assert_eq!(
        result.meta.and_then(|meta| meta.0.get("fixture").cloned()),
        Some(json!(true))
    );
    assert_eq!(result.is_error, Some(false));
    drop(client);
    task.abort();

    let (uri, task) = start_server(
        FakeGateway::default(),
        AuthConfig::unauthenticated(),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let client = client_for(&uri, ProtocolVersion::V_2025_11_25, None).await;
    let error = client
        .call_tool(CallToolRequestParams::new("missing"))
        .await
        .expect_err("unknown tool");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
            assert_eq!(error.data.expect("reason data")["category"], "unknown-tool");
        }
        other => panic!("unexpected client error: {other:?}"),
    }
    let error = client
        .call_tool(CallToolRequestParams::new("exec"))
        .await
        .expect_err("invalid input");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(
                error.data.expect("reason data")["category"],
                "invalid-input"
            );
        }
        other => panic!("unexpected client error: {other:?}"),
    }
    drop(client);
    task.abort();

    let (uri, task) = start_server(
        FakeGateway {
            delay: Some(Duration::from_secs(1)),
            ..Default::default()
        },
        AuthConfig::unauthenticated(),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let client = client_for(&uri, ProtocolVersion::V_2025_06_18, None).await;
    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("exec").with_arguments(
                    json!({"name": "anvil__session_create"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
            )
            .await
            .is_err()
    );
    drop(client);
    task.abort();

    let downstream = GatewayError::Downstream(rmcp::model::ErrorData::new(
        ErrorCode::INVALID_PARAMS,
        "downstream rejected request",
        Some(json!({"field": "project"})),
    ));
    let (uri, task) = start_server(
        FakeGateway {
            error: Some(downstream),
            ..Default::default()
        },
        AuthConfig::unauthenticated(),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let client = client_for(&uri, ProtocolVersion::V_2025_11_25, None).await;
    let error = client
        .call_tool(
            CallToolRequestParams::new("exec").with_arguments(
                json!({"name": "anvil__session_create"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect_err("error");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(error.message, "downstream rejected request");
        }
        other => panic!("unexpected client error: {other:?}"),
    }
    drop(client);
    task.abort();

    let (uri, task) = start_server(
        FakeGateway {
            error: Some(GatewayError::UnknownTool(
                "anvil__missing_operation".to_owned(),
            )),
            ..Default::default()
        },
        AuthConfig::unauthenticated(),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let client = client_for(&uri, ProtocolVersion::V_2025_11_25, None).await;
    let error = client
        .call_tool(
            CallToolRequestParams::new("exec").with_arguments(
                json!({"name": "anvil__missing_operation"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect_err("unknown downstream tool");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(error.message, "unknown downstream tool");
            assert_eq!(
                error.data.expect("reason data")["category"],
                "unknown-downstream-tool"
            );
        }
        other => panic!("unexpected client error: {other:?}"),
    }
    drop(client);
    task.abort();

    let (uri, task) = start_server(
        FakeGateway {
            error: Some(GatewayError::UnsupportedCapability(
                "downstream streaming".to_owned(),
            )),
            ..Default::default()
        },
        AuthConfig::unauthenticated(),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let client = client_for(&uri, ProtocolVersion::V_2025_11_25, None).await;
    let error = client
        .call_tool(
            CallToolRequestParams::new("exec").with_arguments(
                json!({"name": "anvil__session_create"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect_err("unsupported downstream capability");
    match error {
        ServiceError::McpError(error) => {
            assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
            assert_eq!(error.message, "unsupported downstream capability");
            assert_eq!(
                error.data.expect("reason data")["category"],
                "unsupported-downstream-capability"
            );
        }
        other => panic!("unexpected client error: {other:?}"),
    }
    drop(client);
    task.abort();
}

#[tokio::test]
async fn official_client_cancellation_ends_pending_call() {
    install_crypto_provider();
    let cancelled = Arc::new(AtomicBool::new(false));
    let (uri, task) = start_server(
        FakeGateway {
            delay: Some(Duration::from_secs(10)),
            cancelled: Some(cancelled.clone()),
            ..Default::default()
        },
        AuthConfig::unauthenticated(),
        vec!["127.0.0.1".to_owned()],
    )
    .await;
    let mut config = StreamableHttpClientTransportConfig::with_uri(uri.as_str());
    config.allow_stateless = true;
    let transport = StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
    let info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("latchkey-cancellation-client", "1"),
    )
    .with_protocol_version(ProtocolVersion::V_2026_07_28);
    let client_cancel = CancellationToken::new();
    let client = serve_client_with_ct(info, transport, client_cancel.clone())
        .await
        .expect("client initialize");
    let call = tokio::spawn(async move {
        client
            .call_tool(
                CallToolRequestParams::new("exec").with_arguments(
                    json!({"name": "anvil__session_create"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    client_cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("client cancellation")
        .expect("client task");
    task.abort();
}
