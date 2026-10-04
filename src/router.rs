//! Exact-snapshot execution routing and bounded admission.

use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

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
    admission: Arc<Admission>,
}

struct Admission {
    accepting: AtomicBool,
    next_id: AtomicU64,
    children: Mutex<BTreeMap<u64, CancellationToken>>,
    changed: Notify,
}

struct ExecutionGuard {
    admission: Arc<Admission>,
    id: u64,
}

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        if let Ok(mut children) = self.admission.children.lock() {
            children.remove(&self.id);
        }
        self.admission.changed.notify_waiters();
    }
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
            admission: Arc::new(Admission {
                accepting: AtomicBool::new(true),
                next_id: AtomicU64::new(1),
                children: Mutex::new(BTreeMap::new()),
                changed: Notify::new(),
            }),
        }
    }

    async fn admit(&self) -> Result<OwnedSemaphorePermit, GatewayError> {
        if !self.admission.accepting.load(Ordering::Acquire) {
            return Err(GatewayError::Unavailable("gateway shutting down".into()));
        }
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| GatewayError::Overloaded)
    }

    fn register(&self, cancellation: CancellationToken) -> Result<ExecutionGuard, GatewayError> {
        if !self.admission.accepting.load(Ordering::Acquire) {
            return Err(GatewayError::Unavailable("gateway shutting down".into()));
        }
        let id = self.admission.next_id.fetch_add(1, Ordering::Relaxed);
        let mut children = self
            .admission
            .children
            .lock()
            .map_err(|_| GatewayError::Unavailable("gateway admission unavailable".into()))?;
        if !self.admission.accepting.load(Ordering::Acquire) {
            return Err(GatewayError::Unavailable("gateway shutting down".into()));
        }
        children.insert(id, cancellation);
        Ok(ExecutionGuard {
            admission: Arc::clone(&self.admission),
            id,
        })
    }

    /// Stop accepting new executions, drain existing executions, and then
    /// cancel children that did not finish within `grace`.
    pub async fn shutdown(&self, grace: Duration) {
        self.admission.accepting.store(false, Ordering::Release);
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            let empty = self
                .admission
                .children
                .lock()
                .map(|children| children.is_empty())
                .unwrap_or(true);
            if empty {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                _ = self.admission.changed.notified() => {}
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        if let Ok(children) = self.admission.children.lock() {
            for child in children.values() {
                child.cancel();
            }
        }
    }

    pub fn is_accepting(&self) -> bool {
        self.admission.accepting.load(Ordering::Acquire)
    }

    pub fn active_count(&self) -> usize {
        self.admission
            .children
            .lock()
            .map(|children| children.len())
            .unwrap_or(0)
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
                (score > 0).then_some((score, entry))
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
        let child_cancellation = context.cancellation.child_token();
        let guard = self.register(child_cancellation.clone())?;
        context.cancellation = child_cancellation;
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
        drop(guard);
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
