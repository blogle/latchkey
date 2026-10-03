//! Deterministic local lexical search over one catalog snapshot.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use crate::contracts::{
    CatalogRead, GatewayApi, GatewayError, MetricEvent, SearchHit, SearchRequest, Telemetry,
};

/// Search implementation. It never calls a downstream port.
pub struct SearchService {
    catalog: Arc<dyn CatalogRead>,
    telemetry: Arc<dyn Telemetry>,
    default_limit: usize,
    maximum_limit: usize,
}

impl SearchService {
    /// Construct the search service with the contract's conservative limits.
    pub fn new(catalog: Arc<dyn CatalogRead>, telemetry: Arc<dyn Telemetry>) -> Arc<Self> {
        Arc::new(Self {
            catalog,
            telemetry,
            default_limit: 10,
            maximum_limit: 50,
        })
    }

    /// Search the local catalog. Filtering occurs before ranking.
    pub async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, GatewayError> {
        let started = Instant::now();
        self.telemetry.record(MetricEvent::Started {
            operation: crate::contracts::Operation::Search,
            service: request.service.as_deref().map(Into::into),
        });

        let limit = match request.limit {
            Some(limit) if limit > self.maximum_limit => {
                return Err(GatewayError::InvalidInput(
                    "field `limit` must be <= 50".to_owned(),
                ));
            }
            Some(limit) => limit,
            None => self.default_limit,
        };
        let query = normalize(&request.query);
        let tokens = tokens(&query);
        let snapshot = self.catalog.snapshot().await?;
        let mut ranked = Vec::new();

        for entry in snapshot.entries.values() {
            if request
                .service
                .as_deref()
                .is_some_and(|service| service != entry.service.as_str())
            {
                continue;
            }
            let name = normalize(&entry.name);
            let description = normalize(&entry.description);
            let (tier, matched) = if query.is_empty() {
                (0, 0)
            } else if name == query {
                (3, tokens.len())
            } else {
                let name_matches = tokens.iter().filter(|token| name.contains(*token)).count();
                let description_matches = tokens
                    .iter()
                    .filter(|token| description.contains(*token))
                    .count();
                if name_matches > 0 {
                    (2, name_matches)
                } else if description_matches > 0 {
                    (1, description_matches)
                } else {
                    continue;
                }
            };
            ranked.push((
                tier,
                matched,
                entry.name.clone(),
                SearchHit {
                    name: entry.name.clone(),
                    service: entry.service.to_string(),
                    description: entry.description.clone(),
                    input_schema: entry.input_schema.clone(),
                },
            ));
        }

        ranked.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.cmp(&right.2))
        });
        let results = ranked.into_iter().take(limit).map(|item| item.3).collect();
        self.telemetry.record(MetricEvent::Finished {
            operation: crate::contracts::Operation::Search,
            service: request.service.map(Into::into),
            outcome: crate::contracts::Outcome::Success,
            elapsed: started.elapsed(),
        });
        Ok(results)
    }
}

#[async_trait]
impl GatewayApi for SearchService {
    async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, GatewayError> {
        SearchService::search(self, request).await
    }

    async fn exec(
        &self,
        _request: crate::contracts::ExecRequest,
        _context: crate::contracts::ExecContext,
    ) -> Result<rmcp::model::CallToolResponse, GatewayError> {
        Err(GatewayError::UnsupportedCapability(
            "exec composition".to_owned(),
        ))
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn tokens(value: &str) -> Vec<&str> {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CatalogStore;
    use crate::contracts::{CatalogEntry, CatalogSnapshot, NoopTelemetry, Revision, ServiceId};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    fn catalog() -> Arc<CatalogStore> {
        let store = CatalogStore::new();
        let revision = Revision {
            source_uid: "u".to_owned(),
            generation: 1,
            credential_revision: "c".to_owned(),
        };
        let mut entries = BTreeMap::new();
        for (name, description) in [
            ("anvil__session_create", "Create an Anvil worker session"),
            ("anvil__session_delete", "Delete an Anvil worker session"),
            ("github__file_read", "Read a file from GitHub"),
        ] {
            let service = name.split("__").next().unwrap();
            entries.insert(
                name.to_owned(),
                CatalogEntry {
                    name: name.to_owned(),
                    service: ServiceId::new(service),
                    description: description.to_owned(),
                    input_schema: serde_json::Map::from_iter([(
                        String::from("properties"),
                        json!({"x": {"type": "string"}}),
                    )]),
                    downstream_name: name.split("__").nth(1).unwrap().to_owned(),
                    revision: revision.clone(),
                },
            );
        }
        store.set_snapshot_for_test(CatalogSnapshot {
            epoch: 1,
            entries,
            routes: BTreeMap::new(),
        });
        store
    }

    #[tokio::test]
    async fn ranks_exact_name_then_name_then_description_and_keeps_schema() {
        let search = SearchService::new(catalog(), Arc::new(NoopTelemetry));
        let hits = search
            .search(SearchRequest {
                query: "anvil__session_create".to_owned(),
                service: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(hits[0].name, "anvil__session_create");
        assert_eq!(
            hits[0].input_schema["properties"]["x"]["type"],
            json!("string")
        );
        let hits = search
            .search(SearchRequest {
                query: "worker session".to_owned(),
                service: Some("anvil".to_owned()),
                limit: Some(1),
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].name.starts_with("anvil__"));
    }

    #[tokio::test]
    async fn empty_query_is_stable_and_limit_is_bounded() {
        let search = SearchService::new(catalog(), Arc::new(NoopTelemetry));
        let hits = search
            .search(SearchRequest {
                query: String::new(),
                service: None,
                limit: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(
            hits.iter().map(|hit| hit.name.as_str()).collect::<Vec<_>>(),
            ["anvil__session_create", "anvil__session_delete"]
        );
        assert!(
            search
                .search(SearchRequest {
                    query: "x".to_owned(),
                    service: None,
                    limit: Some(51)
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn telemetry_is_recorded_without_query_contents() {
        #[derive(Default)]
        struct Recorder(Mutex<Vec<MetricEvent>>);
        impl Telemetry for Recorder {
            fn record(&self, event: MetricEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let telemetry = Arc::new(Recorder::default());
        let search = SearchService::new(catalog(), telemetry.clone());
        search
            .search(SearchRequest {
                query: "secret".to_owned(),
                service: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(telemetry.0.lock().unwrap().len(), 2);
    }
}
