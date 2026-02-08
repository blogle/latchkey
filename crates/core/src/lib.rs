use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildInfo {
    pub service: &'static str,
    pub version: &'static str,
}

impl BuildInfo {
    pub const fn new(service: &'static str, version: &'static str) -> Self {
        Self { service, version }
    }
}
