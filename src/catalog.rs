//! Atomic, copy-on-write tool catalog.

use std::collections::{BTreeMap, BTreeSet};
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
    next_fence: AtomicU64,
    fences: Mutex<BTreeMap<ServiceId, RevisionFence>>,
}

/// Capability authorizing catalog mutations for one service revision.
///
/// A fence remains recorded after deletion, so an in-flight operation from a
/// removed service cannot recreate it or close a replacement route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionFence {
    service: ServiceId,
    revision: crate::contracts::Revision,
    sequence: u64,
}

impl CatalogStore {
    /// Create an empty catalog.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: ArcSwap::from_pointee(CatalogSnapshot::empty(0)),
            write: Mutex::new(()),
            next_epoch: AtomicU64::new(0),
            next_fence: AtomicU64::new(0),
            fences: Mutex::new(BTreeMap::new()),
        })
    }

    /// Start a new revision and immediately close the previous route.
    pub fn begin_revision(&self, service: Arc<ResolvedService>) -> RevisionFence {
        let _guard = self.write.lock().expect("catalog writer lock poisoned");
        let sequence = self.next_fence.fetch_add(1, Ordering::Relaxed) + 1;
        let fence = RevisionFence {
            service: service.spec.id.clone(),
            revision: service.revision.clone(),
            sequence,
        };
        self.fences
            .lock()
            .expect("catalog fence lock poisoned")
            .insert(fence.service.clone(), fence.clone());
        if let Some(route) = self.snapshot.load().routes.get(&fence.service) {
            route.set_accepting(false);
        }
        fence
    }

    /// Publish one service's discovery result, starting a new fenced revision.
    pub fn publish(
        &self,
        service: Arc<ResolvedService>,
        tools: Vec<Tool>,
    ) -> Result<bool, GatewayError> {
        let fence = self.begin_revision(Arc::clone(&service));
        self.publish_fenced(&fence, service, tools, true)
    }

    /// Publish one service and choose whether its route accepts new calls.
    /// Returns `false` when the discovery completion is stale.
    pub fn publish_fenced(
        &self,
        fence: &RevisionFence,
        service: Arc<ResolvedService>,
        tools: Vec<Tool>,
        accepting: bool,
    ) -> Result<bool, GatewayError> {
        let prepared = prepare_entries(&service, tools)?;
        let _guard = self.write.lock().expect("catalog writer lock poisoned");
        if !self.fence_is_current_locked(fence)
            || fence.service != service.spec.id
            || fence.revision != service.revision
        {
            return Ok(false);
        }
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
        Ok(true)
    }

    /// Remove a service and all of its descriptors if the fence is current.
    pub fn remove_fenced(&self, fence: &RevisionFence) -> bool {
        let _guard = self.write.lock().expect("catalog writer lock poisoned");
        if !self.fence_is_current_locked(fence) {
            return false;
        }
        let current = self.snapshot.load_full();
        let mut next = (*current).clone();
        if next.routes.contains_key(&fence.service) {
            remove_service_from(&mut next, &fence.service);
            self.store_next(next);
        }
        true
    }

    /// Close or open a route only for its current revision fence.
    pub fn set_accepting_fenced(&self, fence: &RevisionFence, accepting: bool) -> bool {
        let _guard = self.write.lock().expect("catalog writer lock poisoned");
        if !self.fence_is_current_locked(fence) {
            return false;
        }
        if let Some(route) = self.snapshot.load().routes.get(&fence.service) {
            route.set_accepting(accepting);
        }
        true
    }

    /// Whether this mutation capability is still the active one.
    pub fn fence_is_current(&self, fence: &RevisionFence) -> bool {
        self.fences
            .lock()
            .expect("catalog fence lock poisoned")
            .get(&fence.service)
            == Some(fence)
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

    fn fence_is_current_locked(&self, fence: &RevisionFence) -> bool {
        self.fences
            .lock()
            .expect("catalog fence lock poisoned")
            .get(&fence.service)
            == Some(fence)
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
            description: tool.description.unwrap_or_default().to_string(),
            input_schema: (*tool.input_schema).clone(),
            downstream_name: tool.name.to_string(),
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
            name.to_owned(),
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
        let fence = catalog.begin_revision(service("one", "one", 1));
        catalog
            .publish_fenced(&fence, service("one", "one", 1), vec![tool("run")], true)
            .unwrap();
        assert!(catalog.set_accepting_fenced(&fence, false));
        let snapshot = catalog.current();
        assert!(!snapshot.routes[&id].is_accepting());
        assert!(snapshot.entries.contains_key("one__run"));
    }

    #[test]
    fn stale_fence_cannot_publish_or_remove_a_recreated_service() {
        let catalog = CatalogStore::new();
        let old = service("one", "one", 1);
        let old_fence = catalog.begin_revision(old.clone());
        let mut recreated = (*service("one", "one", 2)).clone();
        recreated.revision.source_uid = "new-source".to_owned();
        let recreated = Arc::new(recreated);
        let new_fence = catalog.begin_revision(recreated.clone());
        assert!(
            !catalog
                .publish_fenced(&old_fence, old, vec![tool("old")], true)
                .unwrap()
        );
        assert!(
            catalog
                .publish_fenced(&new_fence, recreated, vec![tool("new")], true)
                .unwrap()
        );
        assert!(!catalog.remove_fenced(&old_fence));
        assert!(!catalog.set_accepting_fenced(&old_fence, false));
        let snapshot = catalog.current();
        assert!(snapshot.routes[&ServiceId::new("one")].is_accepting());
        assert!(snapshot.entries.contains_key("one__new"));
        assert!(!snapshot.entries.contains_key("one__old"));
    }

    #[test]
    fn concurrent_publications_keep_unrelated_services() {
        let catalog = CatalogStore::new();
        let mut workers = Vec::new();
        for id in ["one", "two", "three", "four"] {
            let catalog = Arc::clone(&catalog);
            workers.push(std::thread::spawn(move || {
                catalog
                    .publish(service(id, id, 1), vec![tool("run")])
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let snapshot = catalog.current();
        assert_eq!(snapshot.service_count(), 4);
        assert_eq!(snapshot.tool_count(), 4);
        for id in ["one", "two", "three", "four"] {
            assert!(snapshot.entries.contains_key(&format!("{id}__run")));
        }
    }
}
