use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "latchkey.dev",
    version = "v1alpha1",
    kind = "LatchkeyServer",
    plural = "latchkeyservers",
    namespaced,
    status = "LatchkeyServerStatus"
)]
pub struct LatchkeyServerSpec {
    pub image: String,
    pub replicas: Option<i32>,
    pub transport: Option<String>,
    pub service_port: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LatchkeyServerStatus {
    pub ready_replicas: Option<i32>,
    pub endpoints: Option<Vec<String>>,
    pub conditions: Option<Vec<String>>,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "latchkey.dev",
    version = "v1alpha1",
    kind = "LatchkeyTool",
    plural = "latchkeytools",
    namespaced,
    status = "LatchkeyToolStatus"
)]
pub struct LatchkeyToolSpec {
    pub tool_name: String,
    pub server_ref: String,
    pub tool_selector: Option<String>,
    pub max_payload_bytes: Option<u64>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LatchkeyToolStatus {
    pub resolved_server: Option<String>,
    pub conditions: Option<Vec<String>>,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "latchkey.dev",
    version = "v1alpha1",
    kind = "LatchkeyPrincipal",
    plural = "latchkeyprincipals",
    namespaced,
    status = "LatchkeyPrincipalStatus"
)]
pub struct LatchkeyPrincipalSpec {
    pub principal_id: String,
    pub auth_mode: String,
    pub enabled: bool,
    pub policy_refs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LatchkeyPrincipalStatus {
    pub conditions: Option<Vec<String>>,
}

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "latchkey.dev",
    version = "v1alpha1",
    kind = "LatchkeyPolicy",
    plural = "latchkeypolicies",
    namespaced,
    status = "LatchkeyPolicyStatus"
)]
pub struct LatchkeyPolicySpec {
    pub subjects: Vec<String>,
    pub scopes: Vec<String>,
    pub break_glass: Option<bool>,
    pub audit_level: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LatchkeyPolicyStatus {
    pub conditions: Option<Vec<String>>,
}
