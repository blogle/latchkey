//! Atomic, copy-on-write tool catalog.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use rmcp::model::Tool;

use crate::contracts::{
    CatalogEntry, CatalogRead, CatalogSnapshot, GatewayError, ResolvedService, RouteTarget,
    ServiceId, canonical_tool_name,
};

/// The in-memory catalog publisher. Readers only load an `Arc`; the short
/// writer lock covers construction of the next complete snapshot, never I/O.
pub struct CatalogStore {
    snapshot: ArcSwap<CatalogSnapshot>,
    write: Mutex<()>,
    next_epoch: AtomicU64,
}

impl CatalogStore {
    /// Create an empty catalog.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: ArcSwap::from_pointee(CatalogSnapshot::empty(0)),
            write: Mutex::new(()),
            next_epoch: AtomicU64::new(0),
        })
    }

    /// Publish one service's discovery result atomically.
    pub fn publish(
        &self,
        service: Arc<ResolvedService>,
        tools: Vec<Tool>,
    ) -> Result<(), GatewayError> {
        self.publish_with_admission(service, tools, true)
    }

    /// Publish one service and choose whether its route accepts new calls.
    pub fn publish_with_admission(
        &self,
        service: Arc<ResolvedService>,
        tools: Vec<Tool>,
        accepting: bool,
    ) -> Result<(), GatewayError> {
        let prepared = prepare_entries(&service, tools)?;
        let _guard = self.write.lock().expect("catalog writer lock poisoned");
        let current = self.snapshot.load_full();
        let mut next = (*current).clone();
        remove_service_from(&mut next, &service.spec.id);

        if next
            .routes
            .values()
            .any(|route| route.service.spec.prefix == service.spec.prefix)
        {
            return Err(GatewayError::InvalidInput(
                "service prefix is already owned by another service".to_owned(),
            ));
        }

        for entry in &prepared {
            if let Some(existing) = next.entries.get(&entry.name) {
                return Err(GatewayError::InvalidInput(format!(
                    "duplicate canonical tool name owned by multiple services ({} and {})",
                    existing.service, entry.service
                )));
            }
        }
        next.routes.insert(
            service.spec.id.clone(),
            RouteTarget::new(service, accepting),
        );
        for entry in prepared {
            next.entries.insert(entry.name.clone(), entry);
        }
        self.store_next(next);
        Ok(())
    }

    /// Remove a service and all of its descriptors.
    pub fn remove(&self, service: &ServiceId) {
        let _guard = self.write.lock().expect("catalog writer lock poisoned");
        let current = self.snapshot.load_full();
        let mut next = (*current).clone();
        if next.routes.contains_key(service) {
            remove_service_from(&mut next, service);
            self.store_next(next);
        }
    }

    /// Close an existing route immediately, preserving last-known descriptors.
    pub fn set_accepting(&self, service: &ServiceId, accepting: bool) {
        if let Some(route) = self.snapshot.load().routes.get(service) {
            route.set_accepting(accepting);
        }
    }

    /// Return a stable copy of the current snapshot for tests and composition.
    pub fn current(&self) -> Arc<CatalogSnapshot> {
        self.snapshot.load_full()
    }

    #[cfg(test)]
    pub(crate) fn set_snapshot_for_test(&self, snapshot: CatalogSnapshot) {
        self.next_epoch.store(snapshot.epoch, Ordering::Relaxed);
        self.snapshot.store(Arc::new(snapshot));
    }

    fn store_next(&self, mut next: CatalogSnapshot) {
        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        next.epoch = epoch;
        self.snapshot.store(Arc::new(next));
    }
}

#[async_trait]
impl CatalogRead for CatalogStore {
    async fn snapshot(&self) -> Result<Arc<CatalogSnapshot>, GatewayError> {
        Ok(self.current())
    }
}

fn remove_service_from(snapshot: &mut CatalogSnapshot, service: &ServiceId) {
    snapshot
        .entries
        .retain(|_, entry| &entry.service != service);
    snapshot.routes.remove(service);
}

fn prepare_entries(
    service: &ResolvedService,
    tools: Vec<Tool>,
) -> Result<Vec<CatalogEntry>, GatewayError> {
    validate_prefix(&service.spec.prefix)?;
    let mut names = BTreeSet::new();
    let mut entries = Vec::with_capacity(tools.len());
    for tool in tools {
        if tool.name.is_empty() || tool.name.contains('\0') {
            return Err(GatewayError::InvalidInput(
                "downstream tool name must be non-empty and contain no NUL".to_owned(),
            ));
        }
        let name = canonical_tool_name(&service.spec.prefix, &tool.name);
        if !names.insert(name.clone()) {
            return Err(GatewayError::InvalidInput(
                "duplicate downstream tool name in discovery result".to_owned(),
            ));
        }
        entries.push(CatalogEntry {
            name,
            service: service.spec.id.clone(),
            description: tool.description.unwrap_or_default(),
            input_schema: tool.input_schema,
            downstream_name: tool.name,
            revision: service.revision.clone(),
        });
    }
    Ok(entries)
}

fn validate_prefix(prefix: &str) -> Result<(), GatewayError> {
    let valid = !prefix.is_empty()
        && prefix.len() <= 63
        && !prefix.contains("__")
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && prefix.as_bytes()[0].is_ascii_alphanumeric()
        && prefix.as_bytes()[prefix.len() - 1].is_ascii_alphanumeric();
    if valid {
        Ok(())
    } else {
        Err(GatewayError::InvalidInput(
            "service prefix must be a non-empty DNS-style label".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use url::Url;

    fn service(id: &str, prefix: &str, generation: u64) -> Arc<ResolvedService> {
        Arc::new(ResolvedService {
            spec: crate::contracts::ServiceSpec {
                id: ServiceId::new(id),
                prefix: prefix.to_owned(),
                endpoint: Url::parse("http://example.test/mcp").unwrap(),
                enabled: true,
                timeout: Duration::from_secs(1),
                refresh_interval: Duration::from_secs(1),
            },
            revision: crate::contracts::Revision {
                source_uid: id.to_owned(),
                generation,
                credential_revision: format!("c{generation}"),
            },
            headers: crate::contracts::SensitiveHeaders::new(Vec::new()),
        })
    }

    fn tool(name: &str) -> Tool {
        Tool::new(
            name,
            "description",
            serde_json::Map::from_iter([(String::from("type"), json!("object"))]),
        )
    }

    #[tokio::test]
    async fn publishes_entries_and_route_from_one_revision() {
        let catalog = CatalogStore::new();
        let svc = service("one", "one", 4);
        catalog.publish(svc.clone(), vec![tool("run")]).unwrap();
        let snapshot = catalog.snapshot().await.unwrap();
        assert_eq!(snapshot.epoch, 1);
        assert_eq!(snapshot.entries["one__run"].revision, svc.revision);
        assert_eq!(
            snapshot.routes[&ServiceId::new("one")].service.revision,
            snapshot.entries["one__run"].revision
        );
    }

    #[test]
    fn invalid_or_duplicate_publication_leaves_previous_snapshot() {
        let catalog = CatalogStore::new();
        catalog
            .publish(service("one", "one", 1), vec![tool("run")])
            .unwrap();
        let before = catalog.current();
        assert!(
            catalog
                .publish(service("two", "one", 1), vec![tool("run")])
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &catalog.current()));
        assert!(
            catalog
                .publish(service("bad", "bad__prefix", 1), vec![])
                .is_err()
        );
    }

    #[test]
    fn disabling_route_preserves_descriptors_but_closes_admission() {
        let catalog = CatalogStore::new();
        let id = ServiceId::new("one");
        catalog
            .publish(service("one", "one", 1), vec![tool("run")])
            .unwrap();
        catalog.set_accepting(&id, false);
        let snapshot = catalog.current();
        assert!(!snapshot.routes[&id].is_accepting());
        assert!(snapshot.entries.contains_key("one__run"));
    }
}
