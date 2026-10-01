//! External contract consumer for LATCH-2 (F02).
//!
//! This suite treats `latchkey::contracts` exactly the way later lanes will:
//! it is compiled as a **separate crate** that imports only the library's
//! public surface, implements every frozen port with in-memory domain fakes
//! (no Kubernetes objects or API mocks anywhere), round-trips the JSON
//! request/response types, and proves the sensitive types cannot leak.
//!
//! Compile-time assertions use the workspace test-support stub:
//! `latchkey_test_support::assert_not_serialize!` fails the build if a type
//! ever grows a `serde::Serialize` impl.

use std::collections::BTreeMap;
use std::error::Error as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use latchkey::contracts::{
    CatalogEntry, CatalogRead, CatalogSnapshot, ConfigSnapshot, ConfigSource, DownstreamIo,
    ErrorCategory, ExecContext, ExecRequest, GatewayApi, GatewayError, MetricEvent, NoopTelemetry,
    Operation, Outcome, ReadyReason, ResolvedService, Revision, RouteTarget, SearchHit,
    SearchRequest, SensitiveHeaders, ServiceId, ServiceSpec, ServiceStatus, SourceState,
    StatusSink, Telemetry,
};
use rmcp::model::{CallToolResponse, CallToolResult, ErrorData, Tool};
use serde_json::{Map, Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use url::Url;

// ---------------------------------------------------------------------------
// In-memory domain fakes (ports only; no Kubernetes involved)
// ---------------------------------------------------------------------------

/// Serves one fixed snapshot — the read side of an in-memory catalog.
struct MemoryCatalog {
    snapshot: Arc<CatalogSnapshot>,
}

#[async_trait::async_trait]
impl CatalogRead for MemoryCatalog {
    async fn snapshot(&self) -> Result<Arc<CatalogSnapshot>, GatewayError> {
        Ok(Arc::clone(&self.snapshot))
    }
}

/// Records the last `search`/`exec` requests and answers from memory.
#[derive(Default)]
struct FakeGateway {
    last_search: Mutex<Option<SearchRequest>>,
    hits: Vec<SearchHit>,
    last_exec: Mutex<Option<(ExecRequest, Instant, bool)>>,
}

#[async_trait::async_trait]
impl GatewayApi for FakeGateway {
    async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, GatewayError> {
        *self.last_search.lock().expect("lock") = Some(request);
        Ok(self.hits.clone())
    }

    async fn exec(
        &self,
        request: ExecRequest,
        context: ExecContext,
    ) -> Result<CallToolResponse, GatewayError> {
        *self.last_exec.lock().expect("lock") =
            Some((request, context.deadline, context.is_cancelled()));
        Ok(CallToolResponse::Complete(CallToolResult::default()))
    }
}

/// What the downstream fake remembers about the most recent call.
type RecordedCall = (ServiceId, String, Map<String, Value>);

/// Answers discovery/execution from memory and records what it was asked.
#[derive(Default)]
struct FakeDownstream {
    discovered: Vec<Tool>,
    last_call: Mutex<Option<RecordedCall>>,
}

#[async_trait::async_trait]
impl DownstreamIo for FakeDownstream {
    async fn discover(
        &self,
        service: Arc<ResolvedService>,
        _context: ExecContext,
    ) -> Result<Vec<Tool>, GatewayError> {
        if !service.spec.enabled {
            return Err(GatewayError::Unavailable("service disabled".to_owned()));
        }
        Ok(self.discovered.clone())
    }

    async fn call(
        &self,
        service: Arc<ResolvedService>,
        original_name: String,
        arguments: Map<String, Value>,
        context: ExecContext,
    ) -> Result<CallToolResponse, GatewayError> {
        if context.is_cancelled() {
            return Err(GatewayError::Cancelled);
        }
        *self.last_call.lock().expect("lock") =
            Some((service.spec.id.clone(), original_name, arguments));
        Ok(CallToolResponse::Complete(CallToolResult::default()))
    }
}

/// Publishes one snapshot plus its health state, then blocks until the
/// cancel token fires — the shape every `ConfigSource` implementation has.
#[derive(Clone)]
struct ScriptedConfigSource {
    snapshot: ConfigSnapshot,
    state: SourceState,
}

#[async_trait::async_trait]
impl ConfigSource for ScriptedConfigSource {
    async fn run(
        &self,
        sender: watch::Sender<ConfigSnapshot>,
        state: watch::Sender<SourceState>,
        cancel: CancellationToken,
    ) -> Result<(), GatewayError> {
        sender
            .send(self.snapshot.clone())
            .map_err(|_| GatewayError::Unavailable("snapshot receiver dropped".to_owned()))?;
        state
            .send(self.state)
            .map_err(|_| GatewayError::Unavailable("state receiver dropped".to_owned()))?;
        cancel.cancelled().await;
        Ok(())
    }
}

/// Collects published (service, revision, status) triples in memory.
#[derive(Default)]
struct RecordingStatusSink {
    published: Arc<Mutex<Vec<(ServiceId, Revision, ServiceStatus)>>>,
}

#[async_trait::async_trait]
impl StatusSink for RecordingStatusSink {
    async fn publish(
        &self,
        service: ServiceId,
        revision: Revision,
        status: ServiceStatus,
    ) -> Result<(), GatewayError> {
        self.published
            .lock()
            .expect("lock")
            .push((service, revision, status));
        Ok(())
    }
}

/// Collects telemetry events in memory.
#[derive(Default)]
struct RecordingTelemetry {
    events: Arc<Mutex<Vec<MetricEvent>>>,
}

impl Telemetry for RecordingTelemetry {
    fn record(&self, event: MetricEvent) {
        self.events.lock().expect("lock").push(event);
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn sample_spec() -> ServiceSpec {
    ServiceSpec {
        id: ServiceId::new("anvil"),
        prefix: "anvil".to_owned(),
        endpoint: Url::parse("https://user:secret@example.internal:8443/mcp?token=abc")
            .expect("valid url"),
        enabled: true,
        timeout: Duration::from_secs(180),
        refresh_interval: Duration::from_secs(300),
    }
}

fn sample_revision() -> Revision {
    Revision {
        source_uid: "uid-1".to_owned(),
        generation: 4,
        credential_revision: "cred-rev-2".to_owned(),
    }
}

fn sample_service() -> Arc<ResolvedService> {
    Arc::new(ResolvedService {
        spec: sample_spec(),
        revision: sample_revision(),
        headers: SensitiveHeaders::new(vec![(
            "Authorization".to_owned(),
            "Bearer super-secret-token".to_owned(),
        )]),
    })
}

fn sample_entry() -> CatalogEntry {
    CatalogEntry {
        name: "anvil__session_create".to_owned(),
        service: ServiceId::new("anvil"),
        description: "Create an Anvil worker session".to_owned(),
        input_schema: Map::new(),
        downstream_name: "session_create".to_owned(),
        revision: sample_revision(),
    }
}

fn sample_snapshot() -> Arc<CatalogSnapshot> {
    let mut entries = BTreeMap::new();
    let entry = sample_entry();
    entries.insert(entry.name.clone(), entry);
    let mut routes = BTreeMap::new();
    routes.insert(
        ServiceId::new("anvil"),
        RouteTarget::new(sample_service(), true),
    );
    Arc::new(CatalogSnapshot {
        epoch: 1,
        entries,
        routes,
    })
}

fn empty_context() -> ExecContext {
    ExecContext::new(
        Instant::now() + Duration::from_secs(5),
        CancellationToken::new(),
        opentelemetry::Context::new(),
    )
}

fn disabled_service() -> Arc<ResolvedService> {
    let mut service = (*sample_service()).clone();
    service.spec.enabled = false;
    Arc::new(service)
}

// ---------------------------------------------------------------------------
// Port conformance: every frozen port implemented by an external consumer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalog_read_port_serves_shared_snapshots() {
    // The fake never performs I/O, but the signature is exactly the frozen
    // async one every lane must implement.
    let catalog = MemoryCatalog {
        snapshot: sample_snapshot(),
    };
    let snapshot = catalog.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.epoch, 1);
    assert_eq!(snapshot.tool_count(), 1);
    assert_eq!(snapshot.service_count(), 1);
    let entry = snapshot
        .entries
        .get("anvil__session_create")
        .expect("entry");
    assert_eq!(entry.downstream_name, "session_create");
    assert_eq!(entry.service, ServiceId::new("anvil"));

    // Clones of the snapshot are cheap and consistent.
    let second = catalog.snapshot().await.expect("snapshot");
    assert!(Arc::ptr_eq(&snapshot, &second));
}

#[tokio::test]
async fn gateway_api_search_port_forwards_request_and_returns_hits() {
    let hit = SearchHit {
        name: "anvil__session_create".to_owned(),
        service: "anvil".to_owned(),
        description: "Create an Anvil worker session".to_owned(),
        input_schema: sample_snapshot().entries["anvil__session_create"]
            .input_schema
            .clone(),
    };
    let gateway = FakeGateway {
        last_search: Mutex::new(None),
        hits: vec![hit.clone()],
        last_exec: Mutex::new(None),
    };

    let request = SearchRequest {
        query: "create session".to_owned(),
        service: Some("anvil".to_owned()),
        limit: Some(10),
    };
    let results = gateway.search(request.clone()).await.expect("search");
    assert_eq!(results, vec![hit]);

    let recorded = gateway
        .last_search
        .lock()
        .expect("lock")
        .clone()
        .expect("recorded");
    assert_eq!(recorded, request);
}

#[tokio::test]
async fn gateway_api_exec_port_forwards_request_and_context() {
    let gateway = FakeGateway::default();
    let context = ExecContext::new(
        Instant::now() + Duration::from_secs(7),
        CancellationToken::new(),
        opentelemetry::Context::new(),
    );
    let request = ExecRequest {
        name: "anvil__session_create".to_owned(),
        arguments: {
            let mut args = Map::new();
            args.insert("project".to_owned(), json!("demo"));
            args
        },
    };

    let response = gateway
        .exec(request.clone(), context.clone())
        .await
        .expect("exec");
    assert!(matches!(response, CallToolResponse::Complete(_)));

    let recorded = gateway
        .last_exec
        .lock()
        .expect("lock")
        .clone()
        .expect("recorded");
    assert_eq!(recorded.0, request);
    assert_eq!(recorded.1, context.deadline);
    assert!(!recorded.2, "context was not cancelled yet");
}

#[tokio::test]
async fn downstream_io_port_discovers_tools_and_refuses_disabled_services() {
    let downstream = FakeDownstream {
        discovered: vec![{
            let schema: Map<String, Value> = Map::new();
            Tool::new("session_create", "Create an Anvil worker session", schema)
        }],
        last_call: Mutex::new(None),
    };

    let tools = downstream
        .discover(sample_service(), empty_context())
        .await
        .expect("discover");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "session_create");

    let error = downstream
        .discover(disabled_service(), empty_context())
        .await
        .expect_err("disabled service must be refused");
    assert_eq!(
        error,
        GatewayError::Unavailable("service disabled".to_owned())
    );
    assert_eq!(error.category(), ErrorCategory::Unavailable);
}

#[tokio::test]
async fn downstream_io_port_forwards_original_name_and_arguments_untouched() {
    let downstream = FakeDownstream::default();
    let mut arguments = Map::new();
    arguments.insert("count".to_owned(), json!(3));
    arguments.insert("big".to_owned(), json!(9_007_199_254_740_993_u64));
    arguments.insert(
        "nested".to_owned(),
        json!({"unicode": "héllo ✓ 日本語", "flag": true}),
    );
    let expected = arguments.clone();

    downstream
        .call(
            sample_service(),
            "session_create".to_owned(),
            arguments,
            empty_context(),
        )
        .await
        .expect("call");

    let recorded = downstream
        .last_call
        .lock()
        .expect("lock")
        .clone()
        .expect("recorded");
    assert_eq!(recorded.0, ServiceId::new("anvil"));
    assert_eq!(recorded.1, "session_create");
    assert_eq!(recorded.2, expected);

    // A cancelled context is refused before any recording happens.
    let context = ExecContext::new(
        Instant::now() + Duration::from_secs(5),
        CancellationToken::new(),
        opentelemetry::Context::new(),
    );
    context.cancellation.cancel();
    let error = downstream
        .call(
            sample_service(),
            "session_create".to_owned(),
            Map::new(),
            context,
        )
        .await
        .expect_err("cancelled context must be refused");
    assert_eq!(error.category(), ErrorCategory::Cancelled);
}

#[tokio::test]
async fn config_source_port_publishes_snapshot_and_state_until_cancel() {
    let source = ScriptedConfigSource {
        snapshot: ConfigSnapshot {
            revision: 7,
            services: vec![(*sample_service()).clone()],
        },
        state: SourceState {
            initial_complete: true,
            healthy: true,
        },
    };
    let (snapshot_tx, mut snapshot_rx) = watch::channel(ConfigSnapshot {
        revision: 0,
        services: Vec::new(),
    });
    let (state_tx, mut state_rx) = watch::channel(SourceState::default());
    let cancel = CancellationToken::new();

    let handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { source.run(snapshot_tx, state_tx, cancel).await })
    };

    state_rx.changed().await.expect("state channel open");
    assert!(state_rx.borrow().initial_complete);
    assert!(state_rx.borrow().healthy);

    snapshot_rx.changed().await.expect("snapshot channel open");
    assert_eq!(snapshot_rx.borrow().revision, 7);
    assert_eq!(snapshot_rx.borrow().services.len(), 1);

    cancel.cancel();
    handle
        .await
        .expect("join")
        .expect("ConfigSource::run must return Ok after cancellation");
}

#[tokio::test]
async fn status_sink_port_records_observations() {
    let sink = RecordingStatusSink::default();
    let status = ServiceStatus {
        observed_revision: sample_revision(),
        tool_count: 17,
        last_success: None,
        ready: ReadyReason::Pending,
    };
    sink.publish(ServiceId::new("anvil"), sample_revision(), status.clone())
        .await
        .expect("publish");

    let published = sink.published.lock().expect("lock");
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].0, ServiceId::new("anvil"));
    assert_eq!(published[0].1.generation, 4);
    assert_eq!(published[0].2, status);
    assert_eq!(published[0].2.tool_count, 17);
    assert!(!published[0].2.ready.is_ready());
}

#[test]
fn telemetry_port_records_bounded_events() {
    let telemetry = RecordingTelemetry::default();
    telemetry.record(MetricEvent::Started {
        operation: Operation::McpRequest,
        service: None,
    });
    telemetry.record(MetricEvent::Finished {
        operation: Operation::DownstreamCall,
        service: Some(ServiceId::new("anvil")),
        outcome: Outcome::Timeout,
        elapsed: Duration::from_millis(42),
    });
    telemetry.record(MetricEvent::CatalogSize {
        services: 1,
        tools: 1,
    });

    let events = telemetry.events.lock().expect("lock");
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        MetricEvent::Started {
            operation: Operation::McpRequest,
            service: None
        }
    ));
    match &events[1] {
        MetricEvent::Finished {
            operation,
            service,
            outcome,
            elapsed,
        } => {
            assert_eq!(*operation, Operation::DownstreamCall);
            assert_eq!(service.as_ref(), Some(&ServiceId::new("anvil")));
            assert_eq!(*outcome, Outcome::Timeout);
            assert_eq!(*elapsed, Duration::from_millis(42));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert_eq!(
        &events[2],
        &MetricEvent::CatalogSize {
            services: 1,
            tools: 1
        }
    );

    // The library-provided no-op implementation accepts every event shape.
    let noop = NoopTelemetry;
    noop.record(MetricEvent::Started {
        operation: Operation::Discover,
        service: Some(ServiceId::new("anvil")),
    });
}

// ---------------------------------------------------------------------------
// JSON round trips (serde) for the search/exec wire types
// ---------------------------------------------------------------------------

#[test]
fn search_request_round_trips_through_json() {
    let request = SearchRequest {
        query: "create an anvil session".to_owned(),
        service: Some("anvil".to_owned()),
        limit: Some(10),
    };
    let json = serde_json::to_string(&request).expect("serialize");
    assert_eq!(
        json,
        r#"{"query":"create an anvil session","service":"anvil","limit":10}"#
    );
    let back: SearchRequest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, request);

    // Absent optionals serialize away and deserialize back to None.
    let minimal = SearchRequest {
        query: "session".to_owned(),
        service: None,
        limit: None,
    };
    let json = serde_json::to_string(&minimal).expect("serialize");
    assert_eq!(json, r#"{"query":"session"}"#);
    let back: SearchRequest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, minimal);

    // Unknown fields are ignored; missing query is an error.
    assert!(
        serde_json::from_str::<SearchRequest>(r#"{"limit":3}"#).is_err(),
        "query is required"
    );
    let with_extra: SearchRequest =
        serde_json::from_str(r#"{"query":"x","unknown":true}"#).expect("extra field");
    assert_eq!(with_extra.limit, None);
}

#[test]
fn search_hit_round_trips_through_json_with_full_schema_fidelity() {
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert(
        "properties".to_owned(),
        json!({"project": {"type": "string"}, "count": {"type": "integer"}}),
    );
    schema.insert("required".to_owned(), json!(["project"]));
    let hit = SearchHit {
        name: "anvil__session_create".to_owned(),
        service: "anvil".to_owned(),
        description: "Create an Anvil worker session".to_owned(),
        input_schema: schema.clone(),
    };

    let json = serde_json::to_string(&hit).expect("serialize");
    let back: SearchHit = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, hit);
    assert_eq!(back.input_schema, schema);

    let value: Value = serde_json::from_str(&json).expect("json");
    assert_eq!(value["name"], json!("anvil__session_create"));
    assert_eq!(value["service"], json!("anvil"));
    assert_eq!(
        value["input_schema"]["properties"]["count"]["type"],
        json!("integer")
    );
}

#[test]
fn exec_request_round_trips_through_json_without_narrowing_numbers() {
    let mut arguments = Map::new();
    arguments.insert("project".to_owned(), json!("demo"));
    // 2^53 + 1 survives exactly (beyond f64 precision).
    arguments.insert("big".to_owned(), json!(9_007_199_254_740_993_u64));
    arguments.insert("negative".to_owned(), json!(-9_007_199_254_740_993_i64));
    arguments.insert("float".to_owned(), json!(1.5));
    arguments.insert("unicode".to_owned(), json!("héllo ✓ 日本語"));
    arguments.insert(
        "nested".to_owned(),
        json!({"list": [1, 2, 3], "flag": false}),
    );
    arguments.insert("null".to_owned(), Value::Null);

    let request = ExecRequest {
        name: "anvil__session_create".to_owned(),
        arguments: arguments.clone(),
    };
    let json = serde_json::to_string(&request).expect("serialize");
    assert!(
        json.contains("9007199254740993"),
        "exact integer text: {json}"
    );
    let back: ExecRequest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, request);
    assert_eq!(back.arguments, arguments);

    // `arguments` defaults to an empty object when absent.
    let minimal: ExecRequest = serde_json::from_str(r#"{"name":"anvil__x"}"#).expect("defaults");
    assert!(minimal.arguments.is_empty());
    assert_eq!(
        serde_json::to_string(&minimal).expect("serialize"),
        r#"{"name":"anvil__x","arguments":{}}"#
    );
}

// ---------------------------------------------------------------------------
// Sensitive types cannot leak: compile-time Serialize absence + Debug
// ---------------------------------------------------------------------------

#[test]
fn sensitive_types_do_not_implement_serialize() {
    // Compile-time: each expansion fails to build if the type ever grows a
    // serde::Serialize impl (ambiguous `verdict` resolution, E0034).
    latchkey_test_support::assert_not_serialize!(SensitiveHeaders);
    latchkey_test_support::assert_not_serialize!(ResolvedService);
    latchkey_test_support::assert_not_serialize!(ConfigSnapshot);
}

#[test]
fn sensitive_headers_debug_redacts_values_but_keeps_names() {
    let headers = SensitiveHeaders::new(vec![
        (
            "Authorization".to_owned(),
            "Bearer super-secret-token".to_owned(),
        ),
        ("X-Api-Key".to_owned(), "sk-live-abcdef".to_owned()),
    ]);
    let debug = format!("{headers:?}");
    assert!(debug.contains("[REDACTED]"), "got: {debug}");
    assert!(
        debug.contains("Authorization"),
        "names stay visible: {debug}"
    );
    assert!(debug.contains("X-Api-Key"), "names stay visible: {debug}");
    assert!(!debug.contains("super-secret-token"), "leaked: {debug}");
    assert!(!debug.contains("sk-live-abcdef"), "leaked: {debug}");
    assert_eq!(headers.len(), 2);

    // The redaction survives every container that can hold headers.
    let service = ResolvedService {
        spec: sample_spec(),
        revision: sample_revision(),
        headers,
    };
    let snapshot = ConfigSnapshot {
        revision: 3,
        services: vec![service.clone()],
    };
    for debug in [format!("{service:?}"), format!("{snapshot:?}")] {
        assert!(!debug.contains("super-secret-token"), "leaked: {debug}");
        assert!(!debug.contains("sk-live-abcdef"), "leaked: {debug}");
        assert!(!debug.contains("user:secret@"), "endpoint leaked: {debug}");
        assert!(!debug.contains("token=abc"), "query leaked: {debug}");
        assert!(
            debug.contains("[REDACTED]"),
            "redaction marker missing: {debug}"
        );
    }
}

#[test]
fn gateway_errors_never_format_credentials_urls_or_transport_text() {
    let errors = vec![
        GatewayError::InvalidInput("field `limit` out of range (got 10)".to_owned()),
        GatewayError::UnknownTool("anvil__missing".to_owned()),
        GatewayError::Unavailable("transport closed".to_owned()),
        GatewayError::Timeout,
        GatewayError::Cancelled,
        GatewayError::Overloaded,
        GatewayError::UnsupportedCapability("stdio transport".to_owned()),
        GatewayError::Downstream(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "bad argument",
            None,
        )),
    ];
    let secrets = [
        "super-secret-token",
        "user:secret@",
        "token=abc",
        "hyper::Error",
        "connection reset by peer",
    ];
    for error in errors {
        let display = format!("{error}");
        let debug = format!("{error:?}");
        for secret in secrets {
            assert!(
                !display.contains(secret),
                "Display leak ({secret}): {display}"
            );
            assert!(!debug.contains(secret), "Debug leak ({secret}): {debug}");
        }
        assert!(error.source().is_none(), "no source chain");
        // The category helper is total and bounded.
        let category = error.category();
        let label = category.as_str();
        assert!(!label.is_empty());
    }

    // Known MCP errors keep their code.
    let downstream = GatewayError::Downstream(ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "bad argument",
        None,
    ));
    assert!(downstream.to_string().contains("-32602"));
    assert_eq!(downstream.category(), ErrorCategory::Downstream);
}

#[test]
fn route_targets_share_admission_and_service_state() {
    let route = RouteTarget::new(sample_service(), true);
    let clone = route.clone();
    assert!(Arc::ptr_eq(&route.accepting, &clone.accepting));
    assert!(Arc::ptr_eq(&route.service, &clone.service));
    clone.set_accepting(false);
    assert!(!route.is_accepting());
    route.set_accepting(true);
    assert!(clone.is_accepting());
}
