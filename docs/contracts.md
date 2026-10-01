# Latchkey contracts (LATCH-2 / F02)

Status: **frozen**. This document and `src/contracts.rs` define the module
skeleton, domain types, async ports, error taxonomy, telemetry events, and
exact dependency versions for the whole Latchkey MVP. Later tickets
**implement** these interfaces and must not edit the files this ticket owns.
Where prose and code ever disagree, the compiled exports in
`src/contracts.rs` are authoritative — but both were authored together in
F02 and no signature below is pseudocode or a TODO.

## 1. Owned files (do not edit outside your lane)

| File | Owner | Content |
| --- | --- | --- |
| `src/contracts.rs` | LATCH-2 | everything in sections 3–8 |
| `src/lib.rs` | LATCH-2 | root module declarations (once) + LATCH-1 CLI surface |
| `Cargo.toml`, `Cargo.lock` | LATCH-2 | exact dependency pins (section 9) |
| `tests/contracts.rs` | LATCH-2 | external contract consumer suite |
| `docs/contracts.md` | LATCH-2 | this document |
| `ci/capabilities.toml` | LATCH-2 | capability stages (section 11) |
| `.changes/LATCH-2.toml` | LATCH-2 | changelog fragment |
| `crates/test-support/` | LATCH-2 | test-only workspace stub (compile-time probes) |
| `src/catalog.rs` | catalog lane | empty until its ticket |
| `src/search.rs` | search lane | empty until its ticket |
| `src/reconcile.rs` | reconciler lane | empty until its ticket |
| `src/downstream.rs` | downstream lane | empty until its ticket |
| `src/router.rs` | router lane | empty until its ticket |
| `src/local_config.rs` | local-config lane | empty until its ticket |
| `src/telemetry.rs` | telemetry wiring (D01) | empty until its ticket |
| `src/server.rs` | MCP server lane | empty until its ticket |
| `src/runtime.rs` | runtime lane | empty until its ticket |
| `src/health.rs` | health lane | empty until its ticket |
| `src/kubernetes/mod.rs` | kubernetes lane | declares `crd`, `source`, `status` |
| `src/kubernetes/crd.rs` | kubernetes CRD | empty until its ticket |
| `src/kubernetes/source.rs` | kubernetes source | empty until its ticket |
| `src/kubernetes/status.rs` | kubernetes status | empty until its ticket |
| `src/main.rs` | LATCH-1 | binary entry; behaviour frozen (see section 10) |

## 2. Frozen module skeleton

`src/lib.rs` declares every root module exactly once — the lane set frozen
by F02 (`catalog`, `search`, `reconcile`, `downstream`, `router`,
`local_config`, `telemetry`, `server`, `runtime`, `health`, `contracts`,
`kubernetes`) — so no parallel ticket ever edits the root. `cargo fmt`
(rustfmt `reorder_modules`) keeps the declaration block alphabetized:

```rust
pub mod catalog;
pub mod contracts;
pub mod downstream;
pub mod health;
pub mod kubernetes;
pub mod local_config;
pub mod reconcile;
pub mod router;
pub mod runtime;
pub mod search;
pub mod server;
pub mod telemetry;
```

`src/kubernetes/mod.rs` predeclares the Kubernetes sub-lanes once:

```rust
pub mod crd;
pub mod source;
pub mod status;
```

All lane modules except `contracts` are header-only empty files today.
Empty modules compile warning-free under `clippy --all-targets -D warnings`;
no `todo!`/`panic!` stubs exist anywhere in them.

## 3. Canonical tool names

```rust
pub const TOOL_NAME_SEPARATOR: &str = "__";
pub fn canonical_tool_name(prefix: &str, downstream_name: &str) -> String;
pub fn split_canonical_tool_name(name: &str) -> Option<(&str, &str)>;
```

Canonical names are exact strings of the form `prefix__tool`
(`anvil__session_create`). The catalog stores the original downstream name
separately (`CatalogEntry::downstream_name`). Splits happen on the **first**
separator; a service prefix must never contain `__`.

## 4. Configuration domain types

```rust
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceId(String);
impl ServiceId {
    pub fn new(value: impl Into<String>) -> Self;
    pub fn as_str(&self) -> &str;
}
impl fmt::Display for ServiceId;            // writes the raw id
impl From<&str> for ServiceId;
impl From<String> for ServiceId;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Revision {
    pub source_uid: String,
    pub generation: u64,
    pub credential_revision: String,        // opaque, never a secret value
}

#[derive(Clone, PartialEq, Eq)]             // manual Debug: endpoint redacted
pub struct ServiceSpec {
    pub id: ServiceId,
    pub prefix: String,
    pub endpoint: Url,
    pub enabled: bool,
    pub timeout: Duration,
    pub refresh_interval: Duration,
}

pub fn redacted_endpoint(url: &Url) -> String;   // "scheme://host[:port]/"

#[derive(Clone, PartialEq, Eq)]             // manual Debug: values redacted
pub struct SensitiveHeaders(Vec<(String, String)>);
impl SensitiveHeaders {
    pub fn new(headers: Vec<(String, String)>) -> Self;
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
// Debug prints `name: [REDACTED]` per header. NO Serialize, NO Deserialize.

#[derive(Clone, PartialEq, Eq)]             // manual Debug (redacted via fields)
pub struct ResolvedService {
    pub spec: ServiceSpec,
    pub revision: Revision,
    pub headers: SensitiveHeaders,
}

#[derive(Clone, PartialEq, Eq)]             // manual Debug
pub struct ConfigSnapshot {
    pub revision: u64,
    pub services: Vec<ResolvedService>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceState {
    pub initial_complete: bool,
    pub healthy: bool,
}
```

Secret-handling rules encoded above:

* `SensitiveHeaders` cannot be serialized at all (no impl exists; the
  contract suite fails the build if one appears) and its `Debug` shows
  `[REDACTED]` instead of values.
* `ServiceSpec`'s `Debug` (and therefore `ResolvedService`'s and
  `ConfigSnapshot`'s) prints only `redacted_endpoint(endpoint)` — userinfo,
  path, query, and fragment never reach a log, span, or panic message.
* `Revision::credential_revision` is an opaque identity (Secret
  uid/resourceVersion or a digest), never credential material.

## 5. Status publication (Kubernetes-independent)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyReason { Ready, Pending, Disabled, Unreachable, InvalidConfig }
impl ReadyReason {
    pub const fn is_ready(self) -> bool;
    pub const fn as_str(self) -> &'static str;   // "Ready", "Pending", …
}
impl fmt::Display for ReadyReason;

#[derive(Clone, PartialEq, Eq)]             // manual Debug
pub struct ServiceStatus {
    pub observed_revision: Revision,        // mirrors the publish() revision
    pub tool_count: u64,
    pub last_success: Option<SystemTime>,
    pub ready: ReadyReason,
}
```

No Kubernetes type appears here; `kubernetes/status.rs` maps this shape onto
the CRD `Ready` condition (`observedGeneration`, `toolCount`,
`lastDiscoveredAt`).

## 6. Catalog, routing, and JSON wire types

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,                       // canonical prefix__tool
    pub service: ServiceId,
    pub description: String,                // "" when downstream omits one
    pub input_schema: Map<String, Value>,   // full serde_json fidelity
    pub downstream_name: String,            // original downstream tool name
    pub revision: Revision,
}

#[derive(Clone, Debug)]                     // manual Clone semantics via Arc
pub struct RouteTarget {
    pub service: Arc<ResolvedService>,
    pub accepting: Arc<AtomicBool>,         // clones share this flag
}
impl RouteTarget {
    pub fn new(service: Arc<ResolvedService>, accepting: bool) -> Self;
    pub fn is_accepting(&self) -> bool;     // Acquire
    pub fn set_accepting(&self, accepting: bool);   // Release
}

#[derive(Clone, Debug)]
pub struct CatalogSnapshot {
    pub epoch: u64,
    pub entries: BTreeMap<String, CatalogEntry>,    // keyed by canonical name
    pub routes: BTreeMap<ServiceId, RouteTarget>,
}
impl CatalogSnapshot {
    pub fn empty(epoch: u64) -> Self;
    pub fn tool_count(&self) -> u64;
    pub fn service_count(&self) -> u64;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub name: String,
    pub service: String,
    pub description: String,
    pub input_schema: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecRequest {
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

#[derive(Clone)]
pub struct ExecContext {
    pub deadline: Instant,
    pub cancellation: CancellationToken,
    pub trace: opentelemetry::Context,
}
impl ExecContext {
    pub fn new(deadline: Instant, cancellation: CancellationToken, trace: opentelemetry::Context) -> Self;
    pub fn is_expired(&self) -> bool;
    pub fn is_cancelled(&self) -> bool;
}
// Manual Debug prints deadline + cancelled state only (never the trace).
```

Notes:

* `SearchRequest`/`ExecRequest` also derive `schemars::JsonSchema` so the
  server lane can generate the gateway's own `search`/`exec` tool schemas
  from these frozen types (`serde_json::Map<String, Value>` has a schemars
  impl that renders as an object schema).
* JSON numbers are never narrowed: `ExecRequest::arguments` round-trips
  `serde_json::Value` exactly (e.g. `2^53+1` stays an exact integer).
* Wire JSON round trips are asserted by `tests/contracts.rs`.

## 7. Ports (object-safe, `async_trait`)

All ports are `Send + Sync` supertraits and take `&self`.

```rust
#[async_trait]
pub trait CatalogRead: Send + Sync {
    async fn snapshot(&self) -> Result<Arc<CatalogSnapshot>, GatewayError>;
}

#[async_trait]
pub trait GatewayApi: Send + Sync {
    async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, GatewayError>;
    async fn exec(&self, request: ExecRequest, context: ExecContext)
        -> Result<rmcp::model::CallToolResponse, GatewayError>;
}

#[async_trait]
pub trait DownstreamIo: Send + Sync {
    async fn discover(&self, service: Arc<ResolvedService>, context: ExecContext)
        -> Result<Vec<rmcp::model::Tool>, GatewayError>;
    async fn call(
        &self,
        service: Arc<ResolvedService>,
        original_name: String,
        arguments: Map<String, Value>,
        context: ExecContext,
    ) -> Result<rmcp::model::CallToolResponse, GatewayError>;
}

#[async_trait]
pub trait ConfigSource: Send + Sync {
    async fn run(
        &self,
        sender: watch::Sender<ConfigSnapshot>,
        state: watch::Sender<SourceState>,
        cancel: CancellationToken,
    ) -> Result<(), GatewayError>;
}

#[async_trait]
pub trait StatusSink: Send + Sync {
    async fn publish(
        &self,
        service: ServiceId,
        revision: Revision,
        status: ServiceStatus,
    ) -> Result<(), GatewayError>;
}

pub trait Telemetry: Send + Sync {
    fn record(&self, event: MetricEvent);   // sync, infallible by design
}
```

Design rules for implementers:

* `CatalogRead`/`GatewayApi` keep `Catalog::search` and `Router::exec`
  independent of MCP handlers; the server lane is a thin adapter over
  `GatewayApi`.
* `DownstreamIo::call` receives the **original** (un-prefixed) tool name and
  untouched arguments; transport failures must be mapped to sanitized
  `GatewayError` variants before leaving the port.
* `ConfigSource::run` publishes complete snapshots and its own health until
  `cancel` fires, then returns `Ok(())`; local and Kubernetes sources are
  interchangeable implementations.
* `Telemetry::record` is synchronous and non-blocking on purpose: no port
  await, no lock across I/O; production buffering is D01's concern.
  `NoopTelemetry` (provided) discards events for domain tests.

## 8. Error taxonomy and telemetry events

```rust
pub enum ErrorCategory {
    InvalidInput, UnknownTool, Unavailable, Timeout, Cancelled,
    Overloaded, UnsupportedCapability, Downstream,
}
impl ErrorCategory {
    pub const fn as_str(self) -> &'static str;   // "invalid-input", … , "downstream"
}

pub enum GatewayError {
    InvalidInput(String),                 // field-level description, no values
    UnknownTool(String),                  // canonical tool name
    Unavailable(String),                  // fixed-vocabulary reason, no URL/transport text
    Timeout,                              // context-free
    Cancelled,
    Overloaded,
    UnsupportedCapability(String),        // capability name, e.g. "stdio transport"
    Downstream(rmcp::model::ErrorData),   // known MCP error: code/message/data retained
}
impl GatewayError {
    pub const fn category(&self) -> ErrorCategory;   // sanitized logging helper
}
impl fmt::Display for GatewayError;                  // sanitized by construction
impl std::error::Error for GatewayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { None }
}
impl From<rmcp::model::ErrorData> for GatewayError;
```

Guarantees (asserted by tests in `src/contracts.rs` and `tests/contracts.rs`):

* `Display`/`Debug` never contain credentials, URLs with userinfo/query,
  argument values, or raw transport error text.
* Known MCP errors keep their code and message; transport failures never
  become `Downstream` — they map to `Unavailable`/`Timeout`.
* `Error::source()` is always `None`, so no transport error can be pulled
  into a log chain.
* Log `error.category()` plus `Display`; never format arbitrary payloads.

Telemetry port events:

```rust
pub enum Operation { McpRequest, Search, Exec, Discover, DownstreamCall }
// as_str(): "mcp.request", "gateway.search", "gateway.exec",
//           "downstream.discover", "downstream.call"
pub enum Outcome { Success, Error, Timeout, Cancelled, Overloaded }

pub enum MetricEvent {
    Started   { operation: Operation, service: Option<ServiceId> },
    Finished  { operation: Operation, service: Option<ServiceId>,
                outcome: Outcome, elapsed: Duration },
    CatalogSize { services: u64, tools: u64 },
}

pub struct NoopTelemetry;   // Telemetry impl that discards every event
```

Labels are bounded by construction (operation, outcome, service id) — never
arguments, URLs, or error strings. Lanes emit the named tracing spans
directly and record these events; D01 only wires the exporter side, so no
lane file is edited by telemetry wiring.

## 9. Frozen constructor names and composition

Later tickets create these types with exactly these constructors (signatures
are theirs to finish inside their own files; the names are frozen here):

| Constructor | Lane file | Arguments |
| --- | --- | --- |
| `CatalogStore::new` | `src/catalog.rs` | (no arguments frozen; returns the `CatalogRead` store) |
| `SearchService::new(catalog, telemetry)` | `src/search.rs` | a `CatalogRead` handle, a `Telemetry` handle |
| `Reconciler::new(catalog, downstream, status, telemetry)` | `src/reconcile.rs` | catalog publisher, `DownstreamIo`, `StatusSink`, `Telemetry` |
| `SdkDownstream::new(http, telemetry)` | `src/downstream.rs` | `reqwest::Client`, `Telemetry` |
| `Router::new(catalog, downstream, telemetry, limits)` | `src/router.rs` | catalog, `DownstreamIo`, `Telemetry`, exec limits |
| `LocalSource::new(path)` | `src/local_config.rs` | path to the local configuration file |
| `KubernetesSource::new(client, namespace)` | `src/kubernetes/source.rs` | `kube::Client`, namespace |
| `McpServer::new(gateway, auth, hosts)` | `src/server.rs` | `GatewayApi` handle, auth config, listen hosts |
| `Runtime::serve(config)` | `src/runtime.rs` | assembled configuration; runs until shutdown |

Composition note: in R01, `GatewayFacade` composes `SearchService` and
`Router` behind the single `GatewayApi` port (search half + exec half); the
MCP server sees only `GatewayApi`.

## 10. Frozen `just` suite and lane names

Ticket text must use these invocations verbatim:

| Invocation | Meaning |
| --- | --- |
| `just test-integration contracts` | `cargo test --locked --test contracts` (`tests/contracts.rs`; fails on 0 tests) |
| `just test-unit <lane>` | `cargo test --locked --lib <lane>::`; lanes are the module names: `catalog`, `search`, `reconcile`, `downstream`, `router`, `local_config`, `telemetry`, `server`, `runtime`, `health`, `contracts`, `kubernetes` (plus root `tests` for the LATCH-1 CLI) |
| `just fmt-check`, `just lint`, `just build` | formatting, `clippy --all-targets -- -D warnings`, dev build |
| `just pr-check` | fmt-check + lint + script-test + test-unit |
| `just candidate-check` | pr-check (ci profile) + `nix flake check` + `nix build .#package .#oci` + native/OCI smoke |

New integration suites are added as `tests/<suite>.rs` (and, per
`docs/development.md`, an owner entry in `lk_suite_owner`); the invocation
form stays `just test-integration <suite>`.

The LATCH-1 binary behaviour in `src/lib.rs`/`src/main.rs` stays intact:
`--help`/`--version` succeed, any other invocation (notably `serve`) fails
closed with exit code 2.

## 11. Exact dependency pins (frozen; resolved in `Cargo.lock`)

Every direct dependency is pinned with `=` to an exact version. The lock
resolves 301 packages total (299 transitive + the two workspace members)
and builds on the pinned Rust 1.96.0 toolchain (resolver 3, MSRV-aware).

| Crate | Exact version | Features as frozen | Why (lane) |
| --- | --- | --- | --- |
| `rmcp` | `=3.0.1` | `default-features = false`; `server`, `client`, `macros`, `transport-streamable-http-server`, `transport-streamable-http-client-reqwest` | MCP SDK: framing, Streamable HTTP server (upstream) and reqwest client (downstream). **No** `auth`/`auth-client-credentials-jwt` (OAuth), **no** `transport-child-process`/`transport-io` (stdio) |
| `tokio` | `=1.53.1` | `macros`, `rt-multi-thread`, `sync`, `time`, `net`, `signal`, `io-util` | runtime, watch channels, deadlines, listeners, SIGTERM drain |
| `tokio-util` | `=0.7.19` | `rt` | `CancellationToken` (always available) + task tracking for bounded concurrency |
| `axum` | `=0.8.9` | defaults (`form`, `http1`, `json`, `matched-path`, `original-uri`, `query`, `tokio`, `tower-log`, `tracing`) + `http2` | `/healthz`, `/readyz`, MCP HTTP mounting |
| `reqwest` | `=0.13.5` | `default-features = false`; `rustls-no-provider` | downstream HTTP; **no** native-tls, **no** bundled crypto provider (provider chosen via `rustls` below) |
| `serde` | `=1.0.229` | `derive` | domain types, wire types |
| `serde_json` | `=1.0.151` | defaults (BTreeMap-backed `Map`; no `preserve_order`) | `input_schema`, `arguments`, local configuration |
| `schemars` | `=1.2.2` | defaults (`derive`, `std`) | `JsonSchema` for `SearchRequest`/`ExecRequest` tool schemas |
| `arc-swap` | `=1.9.2` | defaults | atomic catalog snapshot publication |
| `async-trait` | `=0.1.92` | defaults | object-safe async ports |
| `kube` | `=4.2.0` | defaults (`client`, `rustls-tls`, `ring`) + `derive`, `runtime` | Kubernetes source/CRD/status; `ring` makes the TLS provider explicit |
| `k8s-openapi` | `=0.28.0` | defaults + `latest` (= v1_36, frozen by the exact pin) | kube's API types |
| `tracing` | `=0.1.44` | defaults (`std`, `attributes`) | named spans in every lane |
| `tracing-subscriber` | `=0.3.23` | defaults (`fmt`, `ansi`, `tracing-log`, `std`, `smallvec`) + `json`, `env-filter` | structured JSON logs + `RUST_LOG` filtering (required by the `tracing` interface; also the registry `tracing-opentelemetry` layers attach to) |
| `opentelemetry` | `=0.33.0` | `trace`, `metrics`, `logs` (+ defaults) | `ExecContext::trace`, metrics API |
| `opentelemetry_sdk` | `=0.33.0` | defaults (`trace`, `metrics`, `logs`, `internal-logs`) | `opentelemetry-otlp`'s public builders return sdk types (`SdkTracerProvider`); required by the named OTLP interface |
| `opentelemetry-otlp` | `=0.33.0` | defaults (`http-proto`, `reqwest-blocking-client`, `trace`, `metrics`, `logs`, `internal-logs`) — **no** `grpc-tonic`, **no** TLS features | OTLP export (HTTP/protobuf); a collector outage must not block serving |
| `tracing-opentelemetry` | `=0.34.0` | defaults (`tracing-log`, `metrics`) | tracing → OTel span layer |
| `url` | `=2.5.8` | defaults | `ServiceSpec::endpoint` |
| `humantime` | `=2.4.0` | defaults | human duration strings in configuration |
| `rustls` | `=0.23.45` | `default-features = false`; `ring`, `std`, `tls12` | pins the TLS crypto provider to **ring** (exactly one provider ⇒ rustls auto-installs it); aws-lc-rs can never enter the graph |

Dev-dependency (tests only, never linked into the binary):

| Crate | Version | Purpose |
| --- | --- | --- |
| `latchkey-test-support` | path `crates/test-support`, `0.0.0` | `assert_not_serialize!` compile-time probe (depends only on `serde =1.0.229`) |

Workspace: `resolver = "3"` (MSRV-aware against `rust-version = 1.96`),
members `["crates/test-support"]`, both packages `publish = false`,
`version = 0.0.0`, `edition = "2024"`.

**Verified absent from the whole graph** (feature/`cargo tree` checks):
`aws-lc-sys`, `aws-lc-rs`, `native-tls`, `openssl-sys`, `hyper-tls`,
`cmake`. TLS everywhere is `rustls 0.23.45` + `ring 0.17.14`, which keeps
the static musl OCI build to plain `cc` compilation.

### Consequences frozen for later tickets

These follow from the dependency set and are binding decisions, not
preferences:

* Local configuration files are **JSON** (`serde_json`) — no YAML/TOML
  parser exists in the frozen graph.
* Local file reload uses polling (`tokio` time) — no `notify`/inotify crate.
* Runtime structure uses `tokio`/`tokio-util` only — no `futures`, `tower`,
  or `async-channel` direct dependency (use `tokio` primitives or the
  re-exports from `rmcp`/`axum`).
* OTLP transports available: HTTP/protobuf only (no gRPC/tonic, no OTLP TLS
  features). OTLP endpoints that require TLS are out of MVP wiring scope.
* Outbound TLS uses the process system trust store via
  `rustls-platform-verifier` (the OCI image ships `cacert`).
* HTTP/2 is enabled for the axum server side; the reqwest client speaks
  HTTP/1.1 (no `http2` feature on `reqwest`).

## 12. Capability stages (`ci/capabilities.toml`)

Data-only, self-describing file consumed by later CI tickets. Three stages —
`foundation`, `standalone`, `kubernetes` — each listing its **cumulative**
mandatory gate set; a transition may only add gates, never remove or weaken
one. See the file itself for the per-stage gate lists and the invariant
(`gates_are_cumulative = true`).

## 13. Verification recorded for this ticket

All commands run on the pinned Rust 1.96.0 toolchain through the cached dev
environment (`just setup true` after the manifest/lock change):

| Command | Result |
| --- | --- |
| `just fmt-check` | pass |
| `just lint` (`clippy --all-targets --locked -- -D warnings`) | pass |
| `just test-integration contracts` | pass — **15** tests |
| `just test-unit contracts` | pass — **14** tests (filter `contracts::`) |
| `just test-unit` (all lib tests) | pass — 22 tests (14 contracts + 8 LATCH-1 CLI) |
| `just build` | pass (`--locked`) |
| `just pr-check` | pass |
| `just candidate-check` | pass (flake check, `nix build .#package .#oci`, native + OCI `--help`/`--version`/serve-refusal smoke) |
| `cargo tree --depth 1` | exact pins listed in section 11 |

Contract-suite coverage (`tests/contracts.rs`, external consumer): all six
ports implemented by in-memory fakes (no Kubernetes objects or API mocks),
JSON round trips for `SearchRequest`/`SearchHit`/`ExecRequest` (including
number fidelity), compile-time Serialize-absence probes for
`SensitiveHeaders`/`ResolvedService`/`ConfigSnapshot`, runtime Debug
redaction assertions, sanitized `GatewayError` formatting, and shared
`RouteTarget` admission-flag semantics.

The Serialize-absence probe is a real compile-time check: expanding
`assert_not_serialize!` for a type that implements `Serialize` (e.g.
`String`) fails the build with `E0034` (ambiguous `verdict` method); for
`SensitiveHeaders` it compiles and asserts the "no Serialize" resolution at
runtime.
