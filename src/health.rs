//! Process liveness and readiness endpoints.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};

#[derive(Clone, Default)]
pub struct HealthState {
    alive: Arc<AtomicBool>,
    initialized: Arc<AtomicBool>,
    source_healthy: Arc<AtomicBool>,
    reconciled: Arc<AtomicBool>,
    accepting: Arc<AtomicBool>,
}

impl HealthState {
    pub fn new() -> Self {
        let state = Self::default();
        state.alive.store(true, Ordering::Release);
        state
    }
    pub fn set_initialized(&self, value: bool) {
        self.initialized.store(value, Ordering::Release);
    }
    pub fn set_source_healthy(&self, value: bool) {
        self.source_healthy.store(value, Ordering::Release);
    }
    pub fn set_reconciled(&self, value: bool) {
        self.reconciled.store(value, Ordering::Release);
    }
    pub fn set_accepting(&self, value: bool) {
        self.accepting.store(value, Ordering::Release);
    }
    pub fn stop(&self) {
        self.accepting.store(false, Ordering::Release);
        self.alive.store(false, Ordering::Release);
    }
    pub fn ready(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
            && self.source_healthy.load(Ordering::Acquire)
            && self.reconciled.load(Ordering::Acquire)
            && self.accepting.load(Ordering::Acquire)
    }
    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(|| async { StatusCode::OK }))
            .route(
                "/readyz",
                get({
                    let state = self.clone();
                    move || {
                        let state = state.clone();
                        async move {
                            if state.ready() {
                                StatusCode::OK
                            } else {
                                StatusCode::SERVICE_UNAVAILABLE
                            }
                        }
                    }
                }),
            )
    }
}

impl IntoResponse for HealthState {
    fn into_response(self) -> axum::response::Response {
        StatusCode::OK.into_response()
    }
}
