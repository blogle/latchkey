//! Shared asynchronous configuration reconciler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;

use crate::catalog::CatalogStore;
use crate::contracts::{
    ConfigSnapshot, ConfigSource, DownstreamIo, ExecContext, GatewayError, MetricEvent,
    ReadyReason, ResolvedService, ServiceId, ServiceStatus, StatusSink, Telemetry,
};

const DISCOVERY_CONCURRENCY: usize = 16;

/// Applies complete configuration snapshots and fences all asynchronous work
/// by the revision that started it.
pub struct Reconciler {
    catalog: Arc<CatalogStore>,
    downstream: Arc<dyn DownstreamIo>,
    status: Arc<dyn StatusSink>,
    telemetry: Arc<dyn Telemetry>,
    semaphore: Arc<Semaphore>,
    current: Mutex<HashMap<ServiceId, crate::catalog::RevisionFence>>,
    config_revision: Mutex<u64>,
    desired: Mutex<Option<ConfigSnapshot>>,
}

impl Reconciler {
    /// Construct a reconciler with bounded concurrent discovery.
    pub fn new(
        catalog: Arc<CatalogStore>,
        downstream: Arc<dyn DownstreamIo>,
        status: Arc<dyn StatusSink>,
        telemetry: Arc<dyn Telemetry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            catalog,
            downstream,
            status,
            telemetry,
            semaphore: Arc::new(Semaphore::new(DISCOVERY_CONCURRENCY)),
            current: Mutex::new(HashMap::new()),
            config_revision: Mutex::new(0),
            desired: Mutex::new(None),
        })
    }

    /// Reconcile one complete desired snapshot. Discovery tasks for unrelated
    /// services run concurrently, while publication remains per-service.
    pub async fn reconcile(&self, desired: ConfigSnapshot) {
        {
            let mut applied = self
                .config_revision
                .lock()
                .expect("reconciler config lock poisoned");
            if desired.revision < *applied {
                return;
            }
            *applied = desired.revision;
        }
        *self
            .desired
            .lock()
            .expect("reconciler desired lock poisoned") = Some(desired.clone());
        let desired_ids: std::collections::BTreeSet<_> = desired
            .services
            .iter()
            .map(|service| service.spec.id.clone())
            .collect();
        {
            let mut current = self.current.lock().expect("reconciler lock poisoned");
            let removed: Vec<_> = current
                .keys()
                .filter(|id| !desired_ids.contains(*id))
                .cloned()
                .collect();
            for id in removed {
                if let Some(fence) = current.remove(&id) {
                    self.catalog.remove_fenced(&fence);
                }
            }
            for service in &desired.services {
                let service = Arc::new(service.clone());
                let fence = self.catalog.begin_revision(Arc::clone(&service));
                current.insert(service.spec.id.clone(), fence);
            }
        }

        let mut tasks = Vec::new();
        for service in desired.services {
            let fence = self
                .current
                .lock()
                .expect("reconciler lock poisoned")
                .get(&service.spec.id)
                .cloned();
            let Some(fence) = fence else { continue };
            if !service.spec.enabled {
                if self.catalog.fence_is_current(&fence) {
                    let count = self
                        .catalog
                        .current()
                        .entries
                        .values()
                        .filter(|entry| entry.service == service.spec.id)
                        .count() as u64;
                    self.publish_status(&fence, &service, count, None, ReadyReason::Disabled)
                        .await;
                }
                continue;
            }
            let permit = Arc::clone(&self.semaphore).acquire_owned().await;
            let downstream = Arc::clone(&self.downstream);
            let service = Arc::new(service);
            let timeout = service.spec.timeout;
            tasks.push(tokio::spawn(async move {
                let _permit = permit.expect("reconciler semaphore closed");
                let context = ExecContext::new(
                    Instant::now() + timeout,
                    CancellationToken::new(),
                    opentelemetry::Context::new(),
                );
                let result = downstream.discover(Arc::clone(&service), context).await;
                (fence, service, result)
            }));
        }

        for task in tasks {
            let Ok((fence, service, result)) = task.await else {
                continue;
            };
            if !self.catalog.fence_is_current(&fence) {
                continue;
            }
            match result {
                Ok(tools) => {
                    let count = tools.len() as u64;
                    if self
                        .catalog
                        .publish_fenced(&fence, service.clone(), tools, true)
                        .is_ok_and(|published| published)
                    {
                        self.publish_status(
                            &fence,
                            &service,
                            count,
                            Some(SystemTime::now()),
                            ReadyReason::Ready,
                        )
                        .await;
                    }
                }
                Err(_error) => {
                    self.catalog.set_accepting_fenced(&fence, false);
                    let reason = ReadyReason::Unreachable;
                    let count = self
                        .catalog
                        .current()
                        .entries
                        .values()
                        .filter(|entry| entry.service == service.spec.id)
                        .count() as u64;
                    self.publish_status(&fence, &service, count, None, reason)
                        .await;
                }
            }
        }
    }

    /// Consume source snapshots until cancellation. A failed service does not
    /// prevent the initial snapshot from completing.
    pub async fn run(
        &self,
        mut snapshots: watch::Receiver<ConfigSnapshot>,
        cancel: CancellationToken,
    ) -> Result<(), GatewayError> {
        self.reconcile(snapshots.borrow().clone()).await;
        let refresh = tokio::time::sleep(self.next_refresh_delay());
        tokio::pin!(refresh);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                changed = snapshots.changed() => {
                    changed.map_err(|_| GatewayError::Unavailable("configuration source stopped".to_owned()))?;
                    self.reconcile(snapshots.borrow_and_update().clone()).await;
                    refresh
                        .as_mut()
                        .reset(tokio::time::Instant::now() + self.next_refresh_delay());
                }
                _ = &mut refresh => {
                    let desired = self.desired.lock().expect("reconciler desired lock poisoned").clone();
                    if let Some(desired) = desired {
                        self.reconcile(desired).await;
                    }
                    refresh
                        .as_mut()
                        .reset(tokio::time::Instant::now() + self.next_refresh_delay());
                }
            }
        }
    }

    /// Convenience composition for a source and its output channels.
    pub async fn run_source(
        &self,
        source: Arc<dyn ConfigSource>,
        sender: watch::Sender<ConfigSnapshot>,
        state: watch::Sender<crate::contracts::SourceState>,
        cancel: CancellationToken,
    ) -> Result<(), GatewayError> {
        let receiver = sender.subscribe();
        let source_task = source.run(sender, state, cancel.clone());
        tokio::join!(self.run(receiver, cancel), source_task).0
    }

    async fn publish_status(
        &self,
        fence: &crate::catalog::RevisionFence,
        service: &ResolvedService,
        tool_count: u64,
        last_success: Option<SystemTime>,
        ready: ReadyReason,
    ) {
        if !self.catalog.fence_is_current(fence) {
            return;
        }
        let status = ServiceStatus {
            observed_revision: service.revision.clone(),
            tool_count,
            last_success,
            ready,
        };
        let _ = self
            .status
            .publish(service.spec.id.clone(), service.revision.clone(), status)
            .await;
        self.telemetry.record(MetricEvent::CatalogSize {
            services: self.catalog.current().service_count(),
            tools: self.catalog.current().tool_count(),
        });
    }

    fn next_refresh_delay(&self) -> Duration {
        self.desired
            .lock()
            .expect("reconciler desired lock poisoned")
            .as_ref()
            .and_then(|desired| {
                desired
                    .services
                    .iter()
                    .filter(|service| service.spec.enabled)
                    .map(|service| service.spec.refresh_interval)
                    .min()
            })
            .unwrap_or(Duration::from_secs(60))
            .max(Duration::from_millis(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{NoopTelemetry, Revision, SensitiveHeaders, ServiceSpec};
    use async_trait::async_trait;
    use rmcp::model::Tool;
    use serde_json::Map;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use url::Url;

    struct Downstream {
        calls: AtomicUsize,
        delay: Duration,
        fail: bool,
        stale_generation_delay: bool,
    }
    #[async_trait]
    impl DownstreamIo for Downstream {
        async fn discover(
            &self,
            _service: Arc<ResolvedService>,
            _context: ExecContext,
        ) -> Result<Vec<Tool>, GatewayError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let delay =
                if self.stale_generation_delay && _service.revision.credential_revision == "1" {
                    Duration::from_millis(30)
                } else {
                    self.delay
                };
            tokio::time::sleep(delay).await;
            if self.fail {
                return Err(GatewayError::Unavailable("discovery failed".to_owned()));
            }
            Ok(vec![Tool::new("run", "run", Map::new())])
        }
        async fn call(
            &self,
            _service: Arc<ResolvedService>,
            _name: String,
            _args: Map<String, serde_json::Value>,
            _context: ExecContext,
        ) -> Result<rmcp::model::CallToolResponse, GatewayError> {
            Err(GatewayError::UnsupportedCapability(
                "test downstream calls".to_owned(),
            ))
        }
    }
    struct Status {
        values: Mutex<Vec<ReadyReason>>,
    }
    #[async_trait]
    impl StatusSink for Status {
        async fn publish(
            &self,
            _service: ServiceId,
            _revision: Revision,
            status: ServiceStatus,
        ) -> Result<(), GatewayError> {
            self.values.lock().unwrap().push(status.ready);
            Ok(())
        }
    }
    fn service(id: &str, generation: u64, enabled: bool) -> ResolvedService {
        ResolvedService {
            spec: ServiceSpec {
                id: ServiceId::new(id),
                prefix: id.to_owned(),
                endpoint: Url::parse("http://example.test").unwrap(),
                enabled,
                timeout: Duration::from_secs(1),
                refresh_interval: Duration::from_secs(1),
            },
            revision: Revision {
                source_uid: id.to_owned(),
                generation,
                credential_revision: generation.to_string(),
            },
            headers: SensitiveHeaders::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn failures_are_isolated_and_initial_reconcile_completes() {
        let catalog = CatalogStore::new();
        let status = Arc::new(Status {
            values: Mutex::new(Vec::new()),
        });
        let reconciler = Reconciler::new(
            catalog.clone(),
            Arc::new(Downstream {
                calls: AtomicUsize::new(0),
                delay: Duration::ZERO,
                fail: false,
                stale_generation_delay: false,
            }),
            status.clone(),
            Arc::new(NoopTelemetry),
        );
        reconciler
            .reconcile(ConfigSnapshot {
                revision: 1,
                services: vec![service("one", 1, true), service("two", 1, false)],
            })
            .await;
        assert!(catalog.current().entries.contains_key("one__run"));
        assert_eq!(
            status.values.lock().unwrap().as_slice(),
            &[ReadyReason::Disabled, ReadyReason::Ready]
        );
    }

    #[tokio::test]
    async fn unrelated_discoveries_run_concurrently() {
        let catalog = CatalogStore::new();
        let downstream = Arc::new(Downstream {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(20),
            fail: false,
            stale_generation_delay: false,
        });
        let reconciler = Reconciler::new(
            catalog.clone(),
            downstream.clone(),
            Arc::new(Status {
                values: Mutex::new(Vec::new()),
            }),
            Arc::new(NoopTelemetry),
        );
        let start = Instant::now();
        reconciler
            .reconcile(ConfigSnapshot {
                revision: 1,
                services: (0..3).map(|i| service(&format!("s{i}"), 1, true)).collect(),
            })
            .await;
        assert!(start.elapsed() < Duration::from_millis(55));
        assert_eq!(downstream.calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn stale_discovery_cannot_republish_an_old_revision() {
        let catalog = CatalogStore::new();
        let reconciler = Reconciler::new(
            catalog.clone(),
            Arc::new(Downstream {
                calls: AtomicUsize::new(0),
                delay: Duration::ZERO,
                fail: false,
                stale_generation_delay: true,
            }),
            Arc::new(Status {
                values: Mutex::new(Vec::new()),
            }),
            Arc::new(crate::contracts::NoopTelemetry),
        );
        let first = Arc::clone(&reconciler);
        let old = tokio::spawn(async move {
            first
                .reconcile(ConfigSnapshot {
                    revision: 1,
                    services: vec![service("one", 1, true)],
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        reconciler
            .reconcile(ConfigSnapshot {
                revision: 2,
                services: vec![service("one", 2, true)],
            })
            .await;
        old.await.unwrap();
        assert_eq!(catalog.current().entries["one__run"].revision.generation, 2);
        assert!(catalog.current().routes[&ServiceId::new("one")].is_accepting());
    }

    #[tokio::test]
    async fn credential_rotation_fences_old_completion_and_acceptance() {
        let catalog = CatalogStore::new();
        let reconciler = Reconciler::new(
            catalog.clone(),
            Arc::new(Downstream {
                calls: AtomicUsize::new(0),
                delay: Duration::ZERO,
                fail: false,
                stale_generation_delay: true,
            }),
            Arc::new(Status {
                values: Mutex::new(Vec::new()),
            }),
            Arc::new(NoopTelemetry),
        );
        let first = Arc::clone(&reconciler);
        let old = tokio::spawn(async move {
            first
                .reconcile(ConfigSnapshot {
                    revision: 1,
                    services: vec![service("one", 1, true)],
                })
                .await;
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        let mut rotated = service("one", 1, true);
        rotated.revision.credential_revision = "2".to_owned();
        reconciler
            .reconcile(ConfigSnapshot {
                revision: 2,
                services: vec![rotated],
            })
            .await;
        old.await.unwrap();
        assert_eq!(
            catalog.current().entries["one__run"]
                .revision
                .credential_revision,
            "2"
        );
        assert!(catalog.current().routes[&ServiceId::new("one")].is_accepting());
    }

    #[tokio::test]
    async fn discovery_failure_closes_route_without_erasing_descriptors() {
        let catalog = CatalogStore::new();
        let service = Arc::new(service("one", 1, true));
        catalog
            .publish(service.clone(), vec![Tool::new("run", "run", Map::new())])
            .unwrap();
        let reconciler = Reconciler::new(
            catalog.clone(),
            Arc::new(Downstream {
                calls: AtomicUsize::new(0),
                delay: Duration::ZERO,
                fail: true,
                stale_generation_delay: false,
            }),
            Arc::new(Status {
                values: Mutex::new(Vec::new()),
            }),
            Arc::new(crate::contracts::NoopTelemetry),
        );
        reconciler
            .reconcile(ConfigSnapshot {
                revision: 1,
                services: vec![(*service).clone()],
            })
            .await;
        assert!(!catalog.current().routes[&ServiceId::new("one")].is_accepting());
        assert!(catalog.current().entries.contains_key("one__run"));
    }
}
