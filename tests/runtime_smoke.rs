//! Process-level standalone smoke test. The MCP protocol path is exercised by
//! the transport tests; this test verifies the actual binary owns the socket
//! and reaches readiness without Kubernetes.

use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;

#[tokio::test]
async fn standalone_binary_serves_health_and_readiness() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve test port");
    let address = listener.local_addr().expect("test address");
    drop(listener);
    let path = std::env::temp_dir().join(format!("latchkey-runtime-{}.toml", std::process::id()));
    std::fs::write(&path, "version = 1\n").expect("write config");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_latchkey"))
        .args([
            "serve",
            "--mode",
            "standalone",
            "--config",
            path.to_str().expect("config path"),
            "--listen",
            &address.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start latchkey");
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let ready = loop {
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&path);
            panic!("standalone process did not become ready");
        }
        if let Ok(response) = client.get(format!("http://{address}/readyz")).send().await {
            if response.status().is_success() { break true; }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(ready);
    assert_eq!(client.get(format!("http://{address}/healthz")).send().await.unwrap().status(), 200);
    child.kill().expect("stop latchkey");
    child.wait().expect("reap latchkey");
    std::fs::remove_file(path).expect("remove config");
}
