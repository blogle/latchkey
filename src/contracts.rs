//! Frozen domain types, ports, error taxonomy, and telemetry events for the
//! Latchkey MVP (LATCH-2 / F02).
//!
//! This module is the **only** shared contract surface in the gateway:
//!
//! * every lane module implements the object-safe ports below and must not
//!   edit this file;
//! * `docs/contracts.md` records the same signatures in prose, the owned
//!   files per lane, the frozen constructor names, and the exact pinned
//!   dependency versions;
//! * nothing here performs I/O, holds locks, or knows about Kubernetes —
//!   [`ServiceStatus`] and the [`StatusSink`] port are deliberately
//!   independent of Kubernetes objects so both configuration sources feed
//!   one reconciler.
//!
//! # Secret-handling rules encoded in these types
//!
//! * [`SensitiveHeaders`] has a redacting [`Debug`](fmt::Debug) and
//!   deliberately does **not** implement `serde::Serialize`; nothing that
//!   holds it can be serialized or logged by accident.
//! * [`ServiceSpec`]'s `Debug` prints only the scheme/host/port of
//!   [`ServiceSpec::endpoint`] via [`redacted_endpoint`] — userinfo, path,
//!   query, and fragment never reach a log or trace.
//! * [`GatewayError`]'s `Display`/`Debug` never carry credentials, URLs,
//!   argument values, or raw transport error text; see the type docs for the
//!   exact policy and [`GatewayError::category`] for the logging helper.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use rmcp::model::{CallToolResponse, ErrorData, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use url::Url;

// ---------------------------------------------------------------------------
// Canonical tool names
// ---------------------------------------------------------------------------

/// Separator between a service prefix and a downstream tool name in the
/// canonical gateway tool name: `<prefix>__<downstream-tool-name>`.
///
/// Example: `anvil__session_create`.
pub const TOOL_NAME_SEPARATOR: &str = "__";

/// Build the canonical gateway tool name for one downstream tool.
///
/// The catalog stores the original downstream name separately (see
/// [`CatalogEntry::downstream_name`]); this function is the single place the
/// canonical form is produced.
///
/// # Contract
///
/// A service prefix must never contain [`TOOL_NAME_SEPARATOR`] (service
/// prefixes are DNS labels in Kubernetes mode and identifiers in standalone
/// mode, neither of which may contain `_`). Downstream tool names may
/// contain it: [`split_canonical_tool_name`] splits on the **first**
/// separator, so `anvil__session__create` splits into `anvil` and
/// `session__create`.
pub fn canonical_tool_name(prefix: &str, downstream_name: &str) -> String {
    format!("{prefix}{TOOL_NAME_SEPARATOR}{downstream_name}")
}

/// Split a canonical gateway tool name into `(prefix, downstream_name)`.
///
/// Returns `None` when the name contains no separator (it is not a canonical
/// name). Splits on the first separator only; see [`canonical_tool_name`].
pub fn split_canonical_tool_name(name: &str) -> Option<(&str, &str)> {
    name.split_once(TOOL_NAME_SEPARATOR)
}

// ---------------------------------------------------------------------------
// Configuration domain types
// ---------------------------------------------------------------------------

/// Stable identifier of one configured downstream service.
///
/// In Kubernetes mode this is the `MCPService` object name; in standalone
/// mode it is the service's `id` in the local configuration file. Not
/// sensitive: it is safe in logs, traces, metrics labels, and JSON.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceId(String);

impl ServiceId {
    /// Create a service id from any string-like value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ServiceId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ServiceId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Identity of the configuration and credential material a resolved service
/// was built from.
///
/// Used to detect stale work: an in-flight discovery that finishes after the
/// source produced a newer revision (or newer credentials) must not publish
/// its result. `credential_revision` is an **opaque, non-secret identity**
/// (for example a Secret `uid`/`resourceVersion`, or a digest of resolved
/// header material) — never the credential value itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Revision {
    /// UID of the configuration object (or synthetic uid for local files)
    /// that produced this revision.
    pub source_uid: String,
    /// Generation of the configuration object the revision corresponds to.
    pub generation: u64,
    /// Opaque identity of the credential material (never a secret value).
    pub credential_revision: String,
}

/// Everything needed to reach one downstream service, minus discovery data.
///
/// The `Debug` impl redacts the endpoint to `scheme://host[:port]/`; see
/// [`redacted_endpoint`].
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Stable service id.
    pub id: ServiceId,
    /// Tool-name prefix; forms canonical names as `<prefix>__<tool>`.
    pub prefix: String,
    /// Downstream Streamable HTTP MCP endpoint.
    pub endpoint: Url,
    /// Whether the service participates in discovery and execution.
    pub enabled: bool,
    /// Per-request timeout for calls to this service.
    pub timeout: Duration,
    /// How often discovery re-runs as a safety net.
    pub refresh_interval: Duration,
}

impl fmt::Debug for ServiceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceSpec")
            .field("id", &self.id)
            .field("prefix", &self.prefix)
            .field("endpoint", &redacted_endpoint(&self.endpoint))
            .field("enabled", &self.enabled)
            .field("timeout", &self.timeout)
            .field("refresh_interval", &self.refresh_interval)
            .finish()
    }
}

/// Render a URL for logging and traces: `scheme://host[:port]/` only.
///
/// Userinfo (`user:password@`), path, query, and fragment are dropped, so
/// the result is always safe to put in a log line, span attribute, or error
/// message. Use this (or [`ServiceSpec`]'s `Debug`) whenever an endpoint
/// must be mentioned outside the transport itself.
pub fn redacted_endpoint(url: &Url) -> String {
    let authority = match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    };
    format!("{}://{authority}/", url.scheme())
}

/// Static header name/value pairs for one downstream service, resolved from
/// a Secret (Kubernetes mode) or a file/environment reference (standalone
/// mode).
///
/// * `Debug` prints each header as `name: [REDACTED]` — values never appear
///   in logs, panics, or formatted errors.
/// * Deliberately **no** `Serialize` implementation: configuration snapshots
///   that contain headers cannot be serialized by accident. The
///   `assert_not_serialize!` probe in `tests/contracts.rs` fails the build
///   if an implementation ever appears.
///
/// Values are constructed only from already-resolved secret material;
/// CRDs and tracked local files contain references, never values.
#[derive(Clone, PartialEq, Eq)]
pub struct SensitiveHeaders(Vec<(String, String)>);

impl SensitiveHeaders {
    /// Wrap resolved header name/value pairs.
    pub fn new(headers: Vec<(String, String)>) -> Self {
        Self(headers)
    }

    /// Iterate the header pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Number of header pairs.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no headers are configured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SensitiveHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted: Vec<String> = self
            .0
            .iter()
            .map(|(name, _)| format!("{name}: [REDACTED]"))
            .collect();
        f.debug_struct("SensitiveHeaders")
            .field("len", &self.0.len())
            .field("headers", &redacted)
            .finish()
    }
}

/// A service as the rest of the gateway sees it: identity, reachability,
/// revision, and static credentials.
///
/// Cheap to clone and share across tasks ([`Arc`] it) — clones reference the
/// same credential material rather than copying it.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedService {
    /// Identity and transport settings.
    pub spec: ServiceSpec,
    /// Configuration/credential revision this resolution is based on.
    pub revision: Revision,
    /// Static headers for this service (redacted `Debug`, no `Serialize`).
    pub headers: SensitiveHeaders,
}

impl fmt::Debug for ResolvedService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedService")
            .field("spec", &self.spec)
            .field("revision", &self.revision)
            .field("headers", &self.headers)
            .finish()
    }
}

/// One complete, validated view of the configuration produced by a
/// [`ConfigSource`] and consumed by the shared reconciler.
///
/// `revision` increases monotonically per source; the reconciler ignores
/// snapshots older than the one it has applied.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigSnapshot {
    /// Monotonic revision of this snapshot within its source.
    pub revision: u64,
    /// All services currently present, in source order.
    pub services: Vec<ResolvedService>,
}

impl fmt::Debug for ConfigSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigSnapshot")
            .field("revision", &self.revision)
            .field("services", &self.services)
            .finish()
    }
}

/// Health of a [`ConfigSource`] itself (not of any downstream service).
///
/// [`SourceState::initial_complete`] flips to `true` once the source has
/// delivered its first complete snapshot; readiness requires it (plus the
/// reconciler functioning), never universal downstream health.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceState {
    /// Whether the first complete snapshot has been published.
    pub initial_complete: bool,
    /// Whether the source itself is currently healthy (file parses, watch
    /// connected, API reachable).
    pub healthy: bool,
}

// ---------------------------------------------------------------------------
// Status publication (Kubernetes-independent)
// ---------------------------------------------------------------------------

/// Why a service is (or is not) ready to accept traffic.
///
/// The Kubernetes status lane maps this onto the CRD `Ready` condition;
/// the values themselves know nothing about Kubernetes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyReason {
    /// Discovery succeeded and the service is accepting executions.
    Ready,
    /// Initial discovery has not completed yet.
    Pending,
    /// The service is explicitly disabled by configuration.
    Disabled,
    /// Discovery/reachability is failing (last-known catalog may still
    /// serve search, but execution is refused).
    Unreachable,
    /// The configuration itself is malformed or rejected.
    InvalidConfig,
}

impl ReadyReason {
    /// Whether this reason maps to a positive `Ready` condition.
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Stable reason token (CamelCase, suitable for CRD conditions).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Pending => "Pending",
            Self::Disabled => "Disabled",
            Self::Unreachable => "Unreachable",
            Self::InvalidConfig => "InvalidConfig",
        }
    }
}

impl fmt::Display for ReadyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Observed state of one service, published through [`StatusSink`].
///
/// Contains no Kubernetes types so the local configuration source can reuse
/// the same shape (for `/readyz` detail) and so lane tests need no
/// Kubernetes objects or mocks.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    /// The revision this status was observed at (mirrors the revision
    /// passed to [`StatusSink::publish`], which the Kubernetes lane writes
    /// into `observedGeneration`).
    pub observed_revision: Revision,
    /// Number of tools discovered for the service at observation time.
    pub tool_count: u64,
    /// When discovery last succeeded for this service.
    pub last_success: Option<SystemTime>,
    /// Why the service is (not) ready.
    pub ready: ReadyReason,
}

impl fmt::Debug for ServiceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceStatus")
            .field("observed_revision", &self.observed_revision)
            .field("tool_count", &self.tool_count)
            .field("last_success", &self.last_success)
            .field("ready", &self.ready)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Catalog and routing types
// ---------------------------------------------------------------------------

/// One discoverable downstream tool in the catalog.
///
/// `name` is the exact canonical `prefix__tool` string clients invoke;
/// `downstream_name` keeps the original downstream tool name so exec can
/// forward it unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Canonical gateway tool name (`<prefix>__<downstream-tool-name>`).
    pub name: String,
    /// Service that owns the tool.
    pub service: ServiceId,
    /// Tool description; empty string when the downstream omits one.
    pub description: String,
    /// JSON Schema for the tool's arguments, exactly as advertised
    /// downstream (full `serde_json` fidelity — never narrowed).
    pub input_schema: Map<String, Value>,
    /// Original (un-prefixed) downstream tool name.
    pub downstream_name: String,
    /// Revision of the service configuration this entry was discovered at.
    pub revision: Revision,
}

/// Routing target for one service.
///
/// Clones share the admission flag: marking a service as not accepting new
/// executions flips the flag for every existing clone immediately (no lock
/// is held across downstream I/O).
#[derive(Clone, Debug)]
pub struct RouteTarget {
    /// Resolved service to call.
    pub service: Arc<ResolvedService>,
    /// Shared admission flag: new executions are refused when `false`.
    pub accepting: Arc<AtomicBool>,
}

impl RouteTarget {
    /// Create a route for a resolved service with an initial admission
    /// flag. Clones of the returned value share the flag.
    pub fn new(service: Arc<ResolvedService>, accepting: bool) -> Self {
        Self {
            service,
            accepting: Arc::new(AtomicBool::new(accepting)),
        }
    }

    /// Whether new executions are currently admitted for this route.
    ///
    /// Uses acquire ordering so a `false` written before admission is
    /// observed by every subsequent reader.
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }

    /// Open or close the route to new executions (release ordering).
    pub fn set_accepting(&self, accepting: bool) {
        self.accepting.store(accepting, Ordering::Release);
    }
}

/// A consistent, immutable view of everything the gateway can route.
///
/// Published atomically (arc-swap) so search and exec each load one
/// coherent snapshot; readers never hold a lock across downstream I/O.
#[derive(Clone, Debug)]
pub struct CatalogSnapshot {
    /// Publication counter; increases on every successful catalog update.
    pub epoch: u64,
    /// All known tools keyed by canonical name (sorted, deterministic
    /// iteration for stable search ordering).
    pub entries: BTreeMap<String, CatalogEntry>,
    /// All known services keyed by id.
    pub routes: BTreeMap<ServiceId, RouteTarget>,
}

impl CatalogSnapshot {
    /// An empty catalog at the given epoch (initial state before the first
    /// discovery completes).
    pub fn empty(epoch: u64) -> Self {
        Self {
            epoch,
            entries: BTreeMap::new(),
            routes: BTreeMap::new(),
        }
    }

    /// Number of tools in the snapshot.
    pub fn tool_count(&self) -> u64 {
        self.entries.len() as u64
    }

    /// Number of services (routes) in the snapshot.
    pub fn service_count(&self) -> u64 {
        self.routes.len() as u64
    }
}

// ---------------------------------------------------------------------------
// Request/response types (JSON round-trippable)
// ---------------------------------------------------------------------------

/// Input to [`GatewayApi::search`].
///
/// Serializes to exactly the JSON shape clients send for the `search` tool;
/// `service` and `limit` are optional and absent when unset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchRequest {
    /// Free-text query matched against names, prefixes, and descriptions.
    pub query: String,
    /// Restrict results to one service id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Maximum number of hits (bounded by the search lane's configured
    /// upper limit; absent means the lane default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// One search result: enough for a caller to invoke the tool without a
/// second discovery round trip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Canonical gateway tool name.
    pub name: String,
    /// Owning service id (as a plain string for JSON clients).
    pub service: String,
    /// Tool description; empty string when the downstream omits one.
    pub description: String,
    /// The tool's argument JSON Schema, exactly as catalogued.
    pub input_schema: Map<String, Value>,
}

/// Input to [`GatewayApi::exec`].
///
/// `name` must be an exact canonical tool name; `arguments` are forwarded
/// downstream without lossy transformation (numbers keep full
/// `serde_json` fidelity).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecRequest {
    /// Canonical gateway tool name to execute.
    pub name: String,
    /// Arguments exactly as received from the client.
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// Per-execution context threaded through search-independent exec paths.
///
/// Cloning shares the cancellation token (clones observe the same
/// cancellation) and the trace context; the deadline is copied.
#[derive(Clone)]
pub struct ExecContext {
    /// Absolute time by which the whole exec (including connection and
    /// handshake work) must complete.
    pub deadline: Instant,
    /// Cancellation tied to the upstream request.
    pub cancellation: CancellationToken,
    /// OpenTelemetry context for this execution (W3C propagation source).
    pub trace: opentelemetry::Context,
}

impl ExecContext {
    /// Build an execution context.
    pub fn new(
        deadline: Instant,
        cancellation: CancellationToken,
        trace: opentelemetry::Context,
    ) -> Self {
        Self {
            deadline,
            cancellation,
            trace,
        }
    }

    /// Whether the deadline has already passed.
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// Whether upstream cancellation has been observed.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl fmt::Debug for ExecContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecContext")
            .field("deadline", &self.deadline)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

/// Bounded, credential-free category of a [`GatewayError`], safe to use as
/// a log field or metric label without further sanitization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    /// Caller-supplied input failed validation.
    InvalidInput,
    /// Canonical tool name is not in the current catalog.
    UnknownTool,
    /// A downstream service cannot serve the request right now.
    Unavailable,
    /// Deadline elapsed before completion.
    Timeout,
    /// Upstream cancellation observed.
    Cancelled,
    /// Load shedding / concurrency limits hit.
    Overloaded,
    /// A capability outside the MVP surface was requested.
    UnsupportedCapability,
    /// A structured MCP error from a downstream service.
    Downstream,
}

impl ErrorCategory {
    /// Stable, machine-friendly label (`"invalid-input"`, `"unknown-tool"`,
    /// …, `"downstream"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid-input",
            Self::UnknownTool => "unknown-tool",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Overloaded => "overloaded",
            Self::UnsupportedCapability => "unsupported-capability",
            Self::Downstream => "downstream",
        }
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The gateway's single error type crossing every port boundary.
///
/// # Sanitization policy
///
/// `Display` and `Debug` must never reveal credentials, URLs containing
/// userinfo or query strings, raw tool argument values, or raw transport
/// error text. The variants enforce this structurally:
///
/// * [`InvalidInput`](Self::InvalidInput) carries a caller-facing validation
///   description that names fields, never their values.
/// * [`UnknownTool`](Self::UnknownTool) carries the canonical tool name
///   (public, already the caller's own input).
/// * [`Unavailable`](Self::Unavailable) carries a fixed-vocabulary reason
///   (e.g. `"discovery incomplete"`, `"service disabled"`) — never a
///   transport error string, never a URL.
/// * [`Timeout`](Self::Timeout), [`Cancelled`](Self::Cancelled), and
///   [`Overloaded`](Self::Overloaded) are context-free.
/// * [`UnsupportedCapability`](Self::UnsupportedCapability) names the
///   capability (e.g. `"stdio transport"`).
/// * [`Downstream`](Self::Downstream) wraps a **structured MCP error**
///   ([`rmcp::model::ErrorData`]): known MCP errors retain their code and
///   data. Transport-level failures are never wrapped here — they map to
///   `Unavailable`/`Timeout` with a sanitized reason instead.
///
/// [`std::error::Error::source`] always returns `None`: a raw transport
/// error can never be pulled into a log chain through this type. Callers
/// that log errors should log [`GatewayError::category`] (and `Display`)
/// rather than inspecting payloads.
#[derive(Clone, Debug, PartialEq)]
pub enum GatewayError {
    /// Caller-supplied input failed validation (description only).
    InvalidInput(String),
    /// Unknown canonical tool name.
    UnknownTool(String),
    /// A downstream service is unavailable (sanitized reason).
    Unavailable(String),
    /// The configured deadline elapsed.
    Timeout,
    /// The upstream request was cancelled.
    Cancelled,
    /// The gateway shed the request to protect itself.
    Overloaded,
    /// A capability outside the MVP surface was requested.
    UnsupportedCapability(String),
    /// A structured MCP error from a downstream service (code/message/data
    /// retained).
    Downstream(ErrorData),
}

impl GatewayError {
    /// Sanitized category for logging and metrics; safe by construction.
    pub const fn category(&self) -> ErrorCategory {
        match self {
            Self::InvalidInput(_) => ErrorCategory::InvalidInput,
            Self::UnknownTool(_) => ErrorCategory::UnknownTool,
            Self::Unavailable(_) => ErrorCategory::Unavailable,
            Self::Timeout => ErrorCategory::Timeout,
            Self::Cancelled => ErrorCategory::Cancelled,
            Self::Overloaded => ErrorCategory::Overloaded,
            Self::UnsupportedCapability(_) => ErrorCategory::UnsupportedCapability,
            Self::Downstream(_) => ErrorCategory::Downstream,
        }
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(detail) => write!(f, "invalid input: {detail}"),
            Self::UnknownTool(name) => write!(f, "unknown tool: {name}"),
            Self::Unavailable(reason) => write!(f, "unavailable: {reason}"),
            Self::Timeout => f.write_str("timed out"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Overloaded => f.write_str("overloaded"),
            Self::UnsupportedCapability(capability) => {
                write!(f, "unsupported capability: {capability}")
            }
            Self::Downstream(error) => write!(
                f,
                "downstream MCP error (code {}): {}",
                error.code.0, error.message
            ),
        }
    }
}

impl std::error::Error for GatewayError {
    /// Always `None`: raw transport errors must never surface through an
    /// error chain (see the type docs).
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl From<ErrorData> for GatewayError {
    fn from(error: ErrorData) -> Self {
        Self::Downstream(error)
    }
}

// ---------------------------------------------------------------------------
// Async ports (object-safe)
// ---------------------------------------------------------------------------

/// Read access to the published catalog.
///
/// Implemented by the catalog lane's store; used by search, exec, and the
/// MCP tool handlers. Loading a snapshot is the only read primitive — no
/// reader ever holds a lock across downstream I/O.
#[async_trait]
pub trait CatalogRead: Send + Sync {
    /// Load the current immutable catalog snapshot.
    async fn snapshot(&self) -> Result<Arc<CatalogSnapshot>, GatewayError>;
}

/// The gateway's two public operations, independent of any MCP handler.
///
/// The MCP server lane is a thin adapter over this port; future internal
/// callers (e.g. a Code Mode tool) use the same port unchanged.
#[async_trait]
pub trait GatewayApi: Send + Sync {
    /// Search the local catalog (no downstream network calls).
    async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, GatewayError>;

    /// Execute one canonical tool, routing to its downstream service.
    async fn exec(
        &self,
        request: ExecRequest,
        context: ExecContext,
    ) -> Result<CallToolResponse, GatewayError>;
}

/// All downstream I/O: discovery and tool execution.
///
/// Implementations must support concurrent calls (no shared client mutex,
/// no serialized execution) and must map transport failures to sanitized
/// [`GatewayError`] variants — raw transport errors never leave this port.
#[async_trait]
pub trait DownstreamIo: Send + Sync {
    /// Discover the tool list of one service (paginated inside the
    /// implementation).
    async fn discover(
        &self,
        service: Arc<ResolvedService>,
        context: ExecContext,
    ) -> Result<Vec<Tool>, GatewayError>;

    /// Invoke one downstream tool under its **original** (un-prefixed) name
    /// with untouched arguments.
    async fn call(
        &self,
        service: Arc<ResolvedService>,
        original_name: String,
        arguments: Map<String, Value>,
        context: ExecContext,
    ) -> Result<CallToolResponse, GatewayError>;
}

/// A configuration source: local file (standalone) or Kubernetes watch.
///
/// The source publishes normalized [`ConfigSnapshot`]s and its own
/// [`SourceState`] over watch channels and returns when `cancel` is
/// cancelled. Both modes feed the same reconciler through this port;
/// neither mode knows about the other.
#[async_trait]
pub trait ConfigSource: Send + Sync {
    /// Run the source until cancelled: send each complete snapshot on
    /// `sender`, each health update on `state`, and return `Ok(())` after
    /// `cancel` fires. Errors are sanitized [`GatewayError`]s.
    async fn run(
        &self,
        sender: watch::Sender<ConfigSnapshot>,
        state: watch::Sender<SourceState>,
        cancel: CancellationToken,
    ) -> Result<(), GatewayError>;
}

/// Publishes observed service state to whoever owns status reporting
/// (the Kubernetes CRD status subresource in cluster mode; a no-op or
/// in-memory sink elsewhere).
///
/// The signature carries no Kubernetes types on purpose — status
/// publication is the sole configuration boundary alongside
/// [`ConfigSource`].
#[async_trait]
pub trait StatusSink: Send + Sync {
    /// Publish the observed status of one service at one revision.
    async fn publish(
        &self,
        service: ServiceId,
        revision: Revision,
        status: ServiceStatus,
    ) -> Result<(), GatewayError>;
}

// ---------------------------------------------------------------------------
// Telemetry port
// ---------------------------------------------------------------------------

/// Bounded operation names for telemetry events (also the span names used
/// by the lanes: `mcp.request`, `gateway.search`, `gateway.exec`,
/// `downstream.discover`, `downstream.call`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
    /// One inbound MCP request.
    McpRequest,
    /// A gateway `search` call.
    Search,
    /// A gateway `exec` call.
    Exec,
    /// One discovery pass for a service.
    Discover,
    /// One downstream tool invocation.
    DownstreamCall,
}

impl Operation {
    /// Stable operation label (matches the corresponding span name).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::McpRequest => "mcp.request",
            Self::Search => "gateway.search",
            Self::Exec => "gateway.exec",
            Self::Discover => "downstream.discover",
            Self::DownstreamCall => "downstream.call",
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bounded outcomes for finished telemetry events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Completed successfully.
    Success,
    /// Failed (see the error category carried in logs, not here).
    Error,
    /// Deadline elapsed.
    Timeout,
    /// Cancelled upstream.
    Cancelled,
    /// Shed due to load.
    Overloaded,
}

impl Outcome {
    /// Stable outcome label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Overloaded => "overloaded",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Events recorded through the [`Telemetry`] port.
///
/// Labels are bounded by construction: a service id (bounded by configured
/// services) but never arbitrary arguments, URLs, or error strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetricEvent {
    /// An operation started.
    Started {
        /// What is starting.
        operation: Operation,
        /// Target service, when the operation has one.
        service: Option<ServiceId>,
    },
    /// An operation finished.
    Finished {
        /// What finished.
        operation: Operation,
        /// Target service, when the operation had one.
        service: Option<ServiceId>,
        /// How it ended.
        outcome: Outcome,
        /// Wall-clock duration of the operation.
        elapsed: Duration,
    },
    /// A catalog size gauge observation.
    CatalogSize {
        /// Number of services in the catalog.
        services: u64,
        /// Number of tools in the catalog.
        tools: u64,
    },
}

/// Telemetry sink port.
///
/// Deliberately synchronous and infallible: recording never blocks on
/// downstream I/O (implementations buffer or drop, they do not wait), so
/// lanes can record from any context without holding locks. Production
/// wiring (OTel exporters, bounded queues) is D01's telemetry lane; domain
/// tests use [`NoopTelemetry`] or their own recorder.
pub trait Telemetry: Send + Sync {
    /// Record one bounded telemetry event.
    fn record(&self, event: MetricEvent);
}

/// A [`Telemetry`] implementation that discards every event.
///
/// Used by domain tests that must not depend on production wiring. The
/// production implementation is built by the telemetry lane (D01).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopTelemetry;

impl Telemetry for NoopTelemetry {
    fn record(&self, _event: MetricEvent) {}
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

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

    fn sample_headers() -> SensitiveHeaders {
        SensitiveHeaders::new(vec![(
            "Authorization".to_owned(),
            "Bearer super-secret-token".to_owned(),
        )])
    }

    #[test]
    fn canonical_names_round_trip() {
        let name = canonical_tool_name("anvil", "session_create");
        assert_eq!(name, "anvil__session_create");
        assert_eq!(
            split_canonical_tool_name(&name),
            Some(("anvil", "session_create"))
        );

        // Downstream names may themselves contain the separator: only the
        // first one splits.
        let nested = canonical_tool_name("anvil", "session__create");
        assert_eq!(nested, "anvil__session__create");
        assert_eq!(
            split_canonical_tool_name(&nested),
            Some(("anvil", "session__create"))
        );

        assert_eq!(split_canonical_tool_name("no-separator"), None);
    }

    #[test]
    fn sensitive_headers_debug_is_fully_redacted() {
        let headers = sample_headers();
        let debug = format!("{headers:?}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(
            !debug.contains("super-secret-token"),
            "secret leaked in Debug: {debug}"
        );
        assert!(
            debug.contains("Authorization"),
            "names stay visible: {debug}"
        );
        assert_eq!(headers.len(), 1);
        assert!(!headers.is_empty());
        let collected: Vec<(&str, &str)> = headers.iter().collect();
        assert_eq!(collected, [("Authorization", "Bearer super-secret-token")]);
    }

    #[test]
    fn empty_sensitive_headers_debug_is_empty() {
        let headers = SensitiveHeaders::new(Vec::new());
        assert!(headers.is_empty());
        assert_eq!(headers.len(), 0);
        assert!(!format!("{headers:?}").contains("Bearer"));
    }

    #[test]
    fn service_spec_debug_redacts_endpoint() {
        let spec = sample_spec();
        let debug = format!("{spec:?}");
        assert!(
            debug.contains("https://example.internal:8443/"),
            "authority should remain visible for debugging: {debug}"
        );
        assert!(!debug.contains("user:secret@"), "userinfo leaked: {debug}");
        assert!(!debug.contains("?token=abc"), "query leaked: {debug}");
        assert!(!debug.contains("/mcp"), "path leaked: {debug}");
        assert_eq!(
            redacted_endpoint(&spec.endpoint),
            "https://example.internal:8443/"
        );
    }

    #[test]
    fn resolved_service_debug_redacts_headers_and_endpoint() {
        let service = ResolvedService {
            spec: sample_spec(),
            revision: Revision {
                source_uid: "uid-1".to_owned(),
                generation: 4,
                credential_revision: "cred-rev-2".to_owned(),
            },
            headers: sample_headers(),
        };
        let debug = format!("{service:?}");
        assert!(debug.contains("[REDACTED]"), "got: {debug}");
        assert!(!debug.contains("super-secret-token"), "leaked: {debug}");
        assert!(!debug.contains("user:secret@"), "leaked: {debug}");
        // The whole thing round-trips through ConfigSnapshot Debug too.
        let snapshot = ConfigSnapshot {
            revision: 7,
            services: vec![service],
        };
        let debug = format!("{snapshot:?}");
        assert!(!debug.contains("super-secret-token"), "leaked: {debug}");
    }

    #[test]
    fn error_category_and_display_stay_sanitized() {
        let secret = "super-secret-token";
        let cases = [
            GatewayError::InvalidInput("field `limit` must be <= 50".to_owned()),
            GatewayError::UnknownTool("anvil__missing".to_owned()),
            GatewayError::Unavailable("discovery incomplete".to_owned()),
            GatewayError::Timeout,
            GatewayError::Cancelled,
            GatewayError::Overloaded,
            GatewayError::UnsupportedCapability("stdio transport".to_owned()),
        ];
        let expected = [
            ErrorCategory::InvalidInput,
            ErrorCategory::UnknownTool,
            ErrorCategory::Unavailable,
            ErrorCategory::Timeout,
            ErrorCategory::Cancelled,
            ErrorCategory::Overloaded,
            ErrorCategory::UnsupportedCapability,
        ];
        for (error, category) in cases.into_iter().zip(expected) {
            assert_eq!(error.category(), category);
            let display = format!("{error}");
            let debug = format!("{error:?}");
            assert!(!display.contains(secret), "leak in Display: {display}");
            assert!(!debug.contains(secret), "leak in Debug: {debug}");
            assert!(
                !display.contains("http://") && !display.contains("https://"),
                "URL in Display: {display}"
            );
            assert!(
                error.source().is_none(),
                "no error source chain may be exposed"
            );
        }

        assert_eq!(
            GatewayError::InvalidInput("x".to_owned()).to_string(),
            "invalid input: x"
        );
        assert_eq!(
            GatewayError::UnknownTool("anvil__gone".to_owned()).to_string(),
            "unknown tool: anvil__gone"
        );
        assert_eq!(GatewayError::Timeout.to_string(), "timed out");
        assert_eq!(GatewayError::Cancelled.to_string(), "cancelled");
        assert_eq!(GatewayError::Overloaded.to_string(), "overloaded");
        assert_eq!(
            GatewayError::Unavailable("service disabled".to_owned()).to_string(),
            "unavailable: service disabled"
        );
        assert_eq!(
            GatewayError::UnsupportedCapability("stdio transport".to_owned()).to_string(),
            "unsupported capability: stdio transport"
        );
    }

    #[test]
    fn downstream_errors_retain_mcp_code_and_data() {
        let error = GatewayError::Downstream(ErrorData::new(
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "bad argument",
            Some(serde_json::json!({"field": "project"})),
        ));
        assert_eq!(error.category(), ErrorCategory::Downstream);
        let display = error.to_string();
        assert!(display.contains("-32602"), "code retained: {display}");
        assert!(
            display.contains("bad argument"),
            "message retained: {display}"
        );
        // Round-trips through From as well.
        let from: GatewayError =
            ErrorData::new(rmcp::model::ErrorCode::INTERNAL_ERROR, "boom", None).into();
        assert_eq!(from.category(), ErrorCategory::Downstream);
    }

    #[test]
    fn error_categories_have_stable_labels() {
        assert_eq!(ErrorCategory::InvalidInput.as_str(), "invalid-input");
        assert_eq!(ErrorCategory::UnknownTool.as_str(), "unknown-tool");
        assert_eq!(ErrorCategory::Unavailable.as_str(), "unavailable");
        assert_eq!(ErrorCategory::Timeout.as_str(), "timeout");
        assert_eq!(ErrorCategory::Cancelled.as_str(), "cancelled");
        assert_eq!(ErrorCategory::Overloaded.as_str(), "overloaded");
        assert_eq!(
            ErrorCategory::UnsupportedCapability.as_str(),
            "unsupported-capability"
        );
        assert_eq!(ErrorCategory::Downstream.as_str(), "downstream");
    }

    #[test]
    fn route_target_clones_share_the_admission_flag() {
        let service = Arc::new(ResolvedService {
            spec: sample_spec(),
            revision: Revision {
                source_uid: "uid-1".to_owned(),
                generation: 1,
                credential_revision: "cred-rev-1".to_owned(),
            },
            headers: sample_headers(),
        });
        let route = RouteTarget::new(service, true);
        assert!(route.is_accepting());

        let clone = route.clone();
        assert!(Arc::ptr_eq(&route.accepting, &clone.accepting));
        clone.set_accepting(false);
        assert!(!route.is_accepting(), "clones must share the flag");
        assert!(!clone.is_accepting());

        // The service arc is shared too — one credential copy, many readers.
        assert!(Arc::ptr_eq(&route.service, &clone.service));
    }

    #[test]
    fn source_state_defaults_and_reports_readiness() {
        let state = SourceState::default();
        assert!(!state.initial_complete);
        assert!(!state.healthy);

        assert!(ReadyReason::Ready.is_ready());
        assert!(!ReadyReason::Pending.is_ready());
        assert!(!ReadyReason::Disabled.is_ready());
        assert!(!ReadyReason::Unreachable.is_ready());
        assert!(!ReadyReason::InvalidConfig.is_ready());
        assert_eq!(ReadyReason::Unreachable.as_str(), "Unreachable");
        assert_eq!(ReadyReason::Unreachable.to_string(), "Unreachable");
    }

    #[test]
    fn service_status_shape_is_kubernetes_independent() {
        let status = ServiceStatus {
            observed_revision: Revision {
                source_uid: "uid-1".to_owned(),
                generation: 9,
                credential_revision: "cred-rev-3".to_owned(),
            },
            tool_count: 17,
            last_success: None,
            ready: ReadyReason::Pending,
        };
        assert_eq!(status.tool_count, 17);
        assert_eq!(status.ready, ReadyReason::Pending);
        assert!(!status.ready.is_ready());
        assert_eq!(status.observed_revision.generation, 9);
    }

    #[test]
    fn catalog_snapshot_counts_and_helpers() {
        let snapshot = CatalogSnapshot::empty(3);
        assert_eq!(snapshot.epoch, 3);
        assert_eq!(snapshot.tool_count(), 0);
        assert_eq!(snapshot.service_count(), 0);

        let mut entries = BTreeMap::new();
        entries.insert(
            "anvil__session_create".to_owned(),
            CatalogEntry {
                name: "anvil__session_create".to_owned(),
                service: ServiceId::new("anvil"),
                description: "Create a session".to_owned(),
                input_schema: Map::new(),
                downstream_name: "session_create".to_owned(),
                revision: Revision {
                    source_uid: "uid-1".to_owned(),
                    generation: 1,
                    credential_revision: "cred-rev-1".to_owned(),
                },
            },
        );
        let mut snapshot = snapshot;
        snapshot.entries = entries;
        assert_eq!(snapshot.tool_count(), 1);
        assert_eq!(snapshot.service_count(), 0);
    }

    #[test]
    fn exec_context_tracks_deadline_and_cancellation() {
        let cancellation = CancellationToken::new();
        let context = ExecContext::new(
            Instant::now() + Duration::from_secs(5),
            cancellation.clone(),
            opentelemetry::Context::new(),
        );
        assert!(!context.is_expired());
        assert!(!context.is_cancelled());

        // Clones share cancellation state and their own deadline copy.
        let clone = context.clone();
        cancellation.cancel();
        assert!(context.is_cancelled());
        assert!(clone.is_cancelled());
        assert!(!clone.is_expired(), "five-second deadline still open");

        let expired = ExecContext::new(
            Instant::now() - Duration::from_millis(1),
            CancellationToken::new(),
            opentelemetry::Context::new(),
        );
        assert!(expired.is_expired());

        // Debug prints deadline/cancellation but never the trace payload.
        let debug = format!("{context:?}");
        assert!(debug.contains("ExecContext"), "got: {debug}");
        assert!(
            !debug.contains("trace"),
            "trace payload must stay hidden: {debug}"
        );
    }

    #[test]
    fn telemetry_events_and_noop_recorder_work() {
        let telemetry = NoopTelemetry;
        telemetry.record(MetricEvent::Started {
            operation: Operation::Search,
            service: None,
        });
        telemetry.record(MetricEvent::Finished {
            operation: Operation::Exec,
            service: Some(ServiceId::new("anvil")),
            outcome: Outcome::Success,
            elapsed: Duration::from_millis(12),
        });
        telemetry.record(MetricEvent::CatalogSize {
            services: 2,
            tools: 9,
        });

        // Labels stay bounded and stable.
        assert_eq!(Operation::McpRequest.as_str(), "mcp.request");
        assert_eq!(Operation::Search.as_str(), "gateway.search");
        assert_eq!(Operation::Exec.as_str(), "gateway.exec");
        assert_eq!(Operation::Discover.as_str(), "downstream.discover");
        assert_eq!(Operation::DownstreamCall.as_str(), "downstream.call");
        assert_eq!(Outcome::Overloaded.as_str(), "overloaded");
        assert_eq!(Operation::Search.to_string(), Operation::Search.as_str());
    }
}
