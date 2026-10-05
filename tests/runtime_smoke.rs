//! Real-process standalone search -> exec smoke test.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientInfo,
    Implementation};
use rmcp::service::serve_client;
use rmcp::transport::streamable_http_client::{StreamableHttpClientTransport,
    StreamableHttpClientTransportConfig};
use serde_json::Value;

struct Reaped(Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() { let _ = self.0.kill(); }
        let _ = self.0.wait();
    }
}

fn free_address() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    listener.local_addr().expect("address").to_string()
}

fn start_fixture(address: &str) -> Reaped {
    Reaped(Command::new(env!("CARGO_BIN_EXE_latchkey-fixture"))
        .arg(address).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("fixture"))
}

async fn wait_http(address: &str, path: &str, expected: reqwest::StatusCode) {
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() >= deadline { panic!("{path} did not reach {expected}"); }
        if let Ok(response) = client.get(format!("http://{address}{path}")).send().await {
            if response.status() == expected { return; }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn client(address: &str) -> rmcp::service::RunningService<rmcp::service::RoleClient, ClientInfo> {
    let transport = StreamableHttpClientTransport::with_client(
        reqwest::Client::new(),
        StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp")),
    );
    serve_client(
        ClientInfo::new(ClientCapabilities::default(), Implementation::new("runtime-smoke", "0.1")),
        transport,
    ).await.expect("official rmcp client connection")
}

#[tokio::test]
async fn real_process_search_exec_isolated_from_unavailable_service() {
    let fixture_address = free_address();
    let _fixture = start_fixture(&fixture_address);
    wait_http(&fixture_address, "/mcp", reqwest::StatusCode::METHOD_NOT_ALLOWED).await;

    let gateway_address = free_address();
    let config = std::env::temp_dir().join(format!("latchkey-runtime-{}.toml", std::process::id()));
    let config_text = format!(
        "version = 1\n[[services]]\nid = 'healthy'\nprefix = 'healthy'\nendpoint = 'http://{fixture_address}/mcp'\ntimeout = '5s'\n[[services]]\nid = 'broken'\nprefix = 'broken'\nendpoint = 'http://127.0.0.1:9/mcp'\ntimeout = '100ms'\n"
    );
    std::fs::write(&config, config_text).expect("config");
    let mut gateway = Reaped(Command::new(env!("CARGO_BIN_EXE_latchkey"))
        .args(["serve", "--mode", "standalone", "--config", config.to_str().unwrap(), "--listen", &gateway_address])
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("latchkey"));
    wait_http(&gateway_address, "/readyz", reqwest::StatusCode::OK).await;

    let service = client(&gateway_address).await;
    let mut search_args = BTreeMap::new();
    search_args.insert("query".to_owned(), Value::String("fixture message".to_owned()));
    let search = service.call_tool(CallToolRequestParams::new("search").with_arguments(search_args.into_iter().collect())).await.expect("search");
    let hits = search.structured_content.expect("search structured result");
    let hit = hits.as_array().expect("search array").iter().find(|hit| hit["name"] == "healthy__echo").expect("healthy fixture hit");
    assert_eq!(hit["input_schema"]["type"], "object");

    let mut exec_args = BTreeMap::new();
    exec_args.insert("name".to_owned(), Value::String(hit["name"].as_str().unwrap().to_owned()));
    exec_args.insert("arguments".to_owned(), json_object([(String::from("message"), Value::String(String::from("through gateway"))]));
    let result = service.call_tool(CallToolRequestParams::new("exec").with_arguments(exec_args.into_iter().collect())).await.expect("exec");
    assert_eq!(result.structured_content.expect("exec result")["fixture"], "echo");
    service.close().await.expect("close client");
    let _ = std::process::Command::new("kill").args(["-TERM", &gateway.0.id().to_string()]).status();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    while gateway.0.try_wait().expect("wait status").is_none() {
        assert!(tokio::time::Instant::now() < deadline, "gateway did not shut down boundedly");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    std::fs::remove_file(config).expect("remove config");
}

fn json_object<const N: usize>(items: [(String, Value); N]) -> Value {
    Value::Object(items.into_iter().collect())
}
