//! Official rmcp Streamable HTTP downstream implementation.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use opentelemetry::trace::TraceContextExt;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, RequestParamsMeta, Tool,
};
use rmcp::service::ServiceExt;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use serde_json::{Map, Value};
use tokio::time::Instant as TokioInstant;

use crate::contracts::{
    DownstreamIo, ExecContext, GatewayError, MetricEvent, Operation, ResolvedService, Telemetry,
};

/// rmcp-owned protocol headers must not be overridden by service credentials.
const RESERVED_HEADERS: &[&str] = &[
    "accept",
    "content-type",
    "mcp-protocol-version",
    "mcp-session-id",
    "last-event-id",
];

/// The concrete downstream client. It is intentionally stateless: each call
/// owns its SDK service and therefore cannot serialize unrelated executions.
pub struct SdkDownstream {
    http: reqwest::Client,
    telemetry: Arc<dyn Telemetry>,
}

impl SdkDownstream {
    /// Construct an rmcp-backed downstream port.
    pub fn new(http: reqwest::Client, telemetry: Arc<dyn Telemetry>) -> Self {
        Self { http, telemetry }
    }

    fn headers(
        service: &ResolvedService,
    ) -> Result<HashMap<HeaderName, HeaderValue>, GatewayError> {
        let mut headers = HashMap::new();
        for (name, value) in service.headers.iter() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| GatewayError::InvalidInput("invalid downstream header name".into()))?;
            if RESERVED_HEADERS
                .iter()
                .any(|reserved| name.as_str() == *reserved)
            {
                return Err(GatewayError::InvalidInput(
                    "downstream header overrides a reserved protocol header".into(),
                ));
            }
            let value = HeaderValue::from_str(value).map_err(|_| {
                GatewayError::InvalidInput("invalid downstream header value".into())
            })?;
            headers.insert(name, value);
        }
        Ok(headers)
    }

    fn client_info() -> ClientInfo {
        ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("latchkey", env!("CARGO_PKG_VERSION")),
        )
    }

    fn traceparent(context: &ExecContext) -> Option<String> {
        let span = context.trace.span();
        let span_context = span.span_context();
        span_context.is_valid().then(|| {
            format!(
                "00-{}-{}-{:02x}",
                span_context.trace_id(),
                span_context.span_id(),
                span_context.trace_flags().to_u8()
            )
        })
    }

    async fn with_deadline<T, F>(context: &ExecContext, future: F) -> Result<T, GatewayError>
    where
        F: std::future::Future<Output = Result<T, rmcp::service::ServiceError>>,
    {
        if context.is_cancelled() {
            return Err(GatewayError::Cancelled);
        }
        if context.is_expired() {
            return Err(GatewayError::Timeout);
        }
        tokio::pin!(future);
        tokio::select! {
            result = &mut future => result.map_err(|error| match error {
                rmcp::service::ServiceError::McpError(error) => GatewayError::Downstream(error),
                rmcp::service::ServiceError::Timeout { .. } => GatewayError::Timeout,
                _ => GatewayError::Unavailable("downstream request failed".into()),
            }),
            _ = context.cancellation.cancelled() => Err(GatewayError::Cancelled),
            _ = tokio::time::sleep_until(TokioInstant::from_std(context.deadline)) => Err(GatewayError::Timeout),
        }
    }

    async fn run_client<T, F>(
        &self,
        service: Arc<ResolvedService>,
        context: &ExecContext,
        operation: F,
    ) -> Result<T, GatewayError>
    where
        F: for<'a> FnOnce(
            &'a rmcp::service::Peer<rmcp::service::RoleClient>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<T, rmcp::service::ServiceError>>
                    + Send
                    + 'a,
            >,
        >,
    {
        let headers = Self::headers(&service)?;
        let mut effective_context = context.clone();
        let service_deadline = std::time::Instant::now()
            .checked_add(service.spec.timeout)
            .unwrap_or(context.deadline);
        if service_deadline < effective_context.deadline {
            effective_context.deadline = service_deadline;
        }
        if effective_context.is_expired() {
            return Err(GatewayError::Timeout);
        }
        let config =
            StreamableHttpClientTransportConfig::with_uri(service.spec.endpoint.to_string())
                .custom_headers(headers)
                .reinit_on_expired_session(false);
        let transport = StreamableHttpClientTransport::with_client(self.http.clone(), config);
        let handshake = Self::client_info().serve(transport);
        tokio::pin!(handshake);
        let client = tokio::select! {
            result = &mut handshake => result.map_err(|_| GatewayError::Unavailable("downstream handshake failed".into()))?,
            _ = effective_context.cancellation.cancelled() => return Err(GatewayError::Cancelled),
            _ = tokio::time::sleep_until(TokioInstant::from_std(effective_context.deadline)) => return Err(GatewayError::Timeout),
        };
        let result = Self::with_deadline(&effective_context, operation(client.peer())).await;
        let mut client = client;
        let _ = client.close().await;
        result
    }
}

#[async_trait]
impl DownstreamIo for SdkDownstream {
    async fn discover(
        &self,
        service: Arc<ResolvedService>,
        context: ExecContext,
    ) -> Result<Vec<Tool>, GatewayError> {
        if !service.spec.enabled {
            return Err(GatewayError::Unavailable("service disabled".into()));
        }
        self.telemetry.record(MetricEvent::Started {
            operation: Operation::Discover,
            service: Some(service.spec.id.clone()),
        });
        self.run_client(service.clone(), &context, |peer| {
            Box::pin(async move { peer.list_all_tools().await })
        })
        .await
    }

    async fn call(
        &self,
        service: Arc<ResolvedService>,
        original_name: String,
        arguments: Map<String, Value>,
        context: ExecContext,
    ) -> Result<rmcp::model::CallToolResponse, GatewayError> {
        if !service.spec.enabled {
            return Err(GatewayError::Unavailable("service disabled".into()));
        }
        self.telemetry.record(MetricEvent::Started {
            operation: Operation::DownstreamCall,
            service: Some(service.spec.id.clone()),
        });
        let traceparent = Self::traceparent(&context);
        self.run_client(service, &context, move |peer| {
            Box::pin(async move {
                let mut params =
                    CallToolRequestParams::new(original_name).with_arguments(arguments);
                if let Some(traceparent) = traceparent {
                    params.set_traceparent(&traceparent);
                }
                peer.call_tool_once(params).await
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{SensitiveHeaders, ServiceId, ServiceSpec};
    use std::time::Duration;
    use url::Url;

    fn service(headers: Vec<(&str, &str)>) -> ResolvedService {
        ResolvedService {
            spec: ServiceSpec {
                id: ServiceId::new("fixture"),
                prefix: "fixture".into(),
                endpoint: Url::parse("http://127.0.0.1:1/mcp").unwrap(),
                enabled: true,
                timeout: Duration::from_secs(1),
                refresh_interval: Duration::from_secs(1),
            },
            revision: crate::contracts::Revision {
                source_uid: "test".into(),
                generation: 1,
                credential_revision: "test".into(),
            },
            headers: SensitiveHeaders::new(
                headers
                    .into_iter()
                    .map(|(n, v)| (n.into(), v.into()))
                    .collect(),
            ),
        }
    }

    #[test]
    fn reserved_headers_are_rejected_before_transport() {
        let error = SdkDownstream::headers(&service(vec![("Mcp-Session-Id", "bad")]))
            .expect_err("reserved header");
        assert!(matches!(error, GatewayError::InvalidInput(_)));
    }

    #[test]
    fn ordinary_headers_are_converted_without_logging_values() {
        let headers = SdkDownstream::headers(&service(vec![("X-Fixture", "sentinel")])).unwrap();
        assert_eq!(headers[&HeaderName::from_static("x-fixture")], "sentinel");
    }
}
