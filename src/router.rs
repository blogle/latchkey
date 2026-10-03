//! Exact-snapshot execution routing and bounded admission.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::contracts::{
    CatalogRead, ExecContext, ExecRequest, GatewayApi, GatewayError, MetricEvent, Operation,
    SearchHit, SearchRequest, Telemetry,
};

// CallToolResponse is re-exported locally only to keep the implementation's
// signatures readable; rmcp remains the source of the wire result.
use rmcp::model::CallToolResponse;

/// Router admission and deadline policy.
#[derive(Clone, Debug)]
pub struct ExecLimits {
    pub max_in_flight: usize,
    pub default_timeout: Duration,
}

impl Default for ExecLimits {
    fn default() -> Self {
        Self {
            max_in_flight: 128,
            default_timeout: Duration::from_secs(180),
        }
    }
}

/// Routes against one immutable catalog snapshot and never locks across I/O.
pub struct Router {
    catalog: Arc<dyn CatalogRead>,
    downstream: Arc<dyn crate::contracts::DownstreamIo>,
    telemetry: Arc<dyn Telemetry>,
    permits: Arc<Semaphore>,
    limits: ExecLimits,
}

impl Router {
    pub fn new(
        catalog: Arc<dyn CatalogRead>,
        downstream: Arc<dyn crate::contracts::DownstreamIo>,
        telemetry: Arc<dyn Telemetry>,
        limits: ExecLimits,
    ) -> Self {
        let capacity = limits.max_in_flight.max(32);
        Self {
            catalog,
            downstream,
            telemetry,
            permits: Arc::new(Semaphore::new(capacity)),
            limits,
        }
    }

    async fn admit(&self) -> Result<OwnedSemaphorePermit, GatewayError> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| GatewayError::Overloaded)
    }
}

#[async_trait]
impl GatewayApi for Router {
    async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, GatewayError> {
        let snapshot = self.catalog.snapshot().await?;
        let query = request.query.to_ascii_lowercase();
        let limit = request.limit.unwrap_or(10).min(50);
        let mut hits: Vec<_> = snapshot
            .entries
            .values()
            .filter(|entry| {
                request
                    .service
                    .as_deref()
                    .is_none_or(|s| entry.service.as_str() == s)
            })
            .filter_map(|entry| {
                let name = entry.name.to_ascii_lowercase();
                let description = entry.description.to_ascii_lowercase();
                let score = if name == query {
                    3
                } else if name.starts_with(&query) {
                    2
                } else if name.contains(&query) || description.contains(&query) {
                    1
                } else {
                    0
                };
                (score > 0).then(|| (score, entry))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        Ok(hits
            .into_iter()
            .take(limit)
            .map(|(_, entry)| SearchHit {
                name: entry.name.clone(),
                service: entry.service.to_string(),
                description: entry.description.clone(),
                input_schema: entry.input_schema.clone(),
            })
            .collect())
    }

    async fn exec(
        &self,
        request: ExecRequest,
        mut context: ExecContext,
    ) -> Result<CallToolResponse, GatewayError> {
        if context.is_expired() {
            return Err(GatewayError::Timeout);
        }
        if context.is_cancelled() {
            return Err(GatewayError::Cancelled);
        }
        let permit = self.admit().await?;
        let snapshot = self.catalog.snapshot().await?;
        let entry = snapshot
            .entries
            .get(&request.name)
            .ok_or_else(|| GatewayError::UnknownTool(request.name.clone()))?;
        let route = snapshot
            .routes
            .get(&entry.service)
            .ok_or_else(|| GatewayError::Unavailable("service route missing".into()))?;
        if !route.is_accepting() {
            return Err(GatewayError::Unavailable(
                "service not accepting executions".into(),
            ));
        }
        if context.is_expired() || context.is_cancelled() {
            return if context.is_cancelled() {
                Err(GatewayError::Cancelled)
            } else {
                Err(GatewayError::Timeout)
            };
        }
        let service_deadline = std::time::Instant::now()
            .checked_add(route.service.spec.timeout.min(self.limits.default_timeout))
            .unwrap_or(context.deadline);
        if service_deadline < context.deadline {
            context.deadline = service_deadline;
        }
        self.telemetry.record(MetricEvent::Started {
            operation: Operation::Exec,
            service: Some(entry.service.clone()),
        });
        let result = self
            .downstream
            .call(
                Arc::clone(&route.service),
                entry.downstream_name.clone(),
                request.arguments,
                context,
            )
            .await;
        drop(permit);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_admission_is_bounded_and_at_least_thirty_two() {
        assert!(ExecLimits::default().max_in_flight >= 32);
    }
}
