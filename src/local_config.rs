//! Standalone TOML configuration source.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use ring::hmac;
use ring::rand::SecureRandom;
use serde::Deserialize;
use tokio::sync::watch;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::contracts::{
    ConfigSnapshot, ConfigSource, GatewayError, ResolvedService, Revision, SensitiveHeaders,
    ServiceId, ServiceSpec, SourceState,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_REFRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A local configuration file. Credentials are references, never values.
#[derive(Clone, Debug)]
pub struct LocalSource {
    path: PathBuf,
}

impl LocalSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self, revision: u64) -> Result<(ConfigSnapshot, bool), GatewayError> {
        let text = std::fs::read_to_string(&self.path)
            .map_err(|_| GatewayError::Unavailable("configuration unreadable".to_owned()))?;
        let raw: RawConfig = toml::from_str(&text)
            .map_err(|_| GatewayError::InvalidInput("configuration is invalid".to_owned()))?;
        normalize(raw, &self.path, revision)
    }
}

#[async_trait]
impl ConfigSource for LocalSource {
    async fn run(
        &self,
        sender: watch::Sender<ConfigSnapshot>,
        state: watch::Sender<SourceState>,
        cancel: CancellationToken,
    ) -> Result<(), GatewayError> {
        let mut revision = 1;
        let mut last_good: Option<ConfigSnapshot> = None;
        let mut attempted = false;

        let mut poll = interval(POLL_INTERVAL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            match self.load(revision) {
                Ok((snapshot, healthy)) => {
                    let unchanged = last_good.as_ref().is_some_and(|previous| {
                        previous
                            .services
                            .iter()
                            .map(|service| (&service.spec, &service.headers))
                            .eq(snapshot
                                .services
                                .iter()
                                .map(|service| (&service.spec, &service.headers)))
                    });
                    let changed = !unchanged;
                    if changed {
                        let mut published = snapshot;
                        published.revision = revision;
                        last_good = Some(published.clone());
                        let _ = sender.send(published);
                        revision += 1;
                    }
                    let _ = state.send(SourceState {
                        initial_complete: true,
                        healthy,
                    });
                }
                Err(_error) => {
                    // A malformed replacement never destroys the last good view.
                    let _ = state.send(SourceState {
                        initial_complete: last_good.is_some(),
                        healthy: false,
                    });
                    if !attempted {
                        let _ = sender.send(ConfigSnapshot {
                            revision: 0,
                            services: Vec::new(),
                        });
                    }
                }
            }
            attempted = true;
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = poll.tick() => {}
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    #[serde(default)]
    services: Vec<RawService>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawService {
    id: String,
    #[serde(default)]
    prefix: Option<String>,
    endpoint: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    timeout: String,
    #[serde(default = "default_refresh")]
    refresh_interval: String,
    #[serde(default)]
    headers: Vec<RawHeader>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHeader {
    name: String,
    #[serde(default)]
    value_file: Option<String>,
    #[serde(default)]
    value_env: Option<String>,
}

fn default_enabled() -> bool {
    true
}

fn default_refresh() -> String {
    "5m".to_owned()
}

fn normalize(
    raw: RawConfig,
    config_path: &Path,
    revision: u64,
) -> Result<(ConfigSnapshot, bool), GatewayError> {
    if raw.version != 1 {
        return Err(GatewayError::InvalidInput(
            "configuration version must be 1".to_owned(),
        ));
    }
    let mut ids = HashSet::new();
    let mut prefixes = HashSet::new();
    let mut services = Vec::with_capacity(raw.services.len());
    let mut healthy = true;
    for raw_service in raw.services {
        let id = validate_identifier(&raw_service.id, "service id")?;
        let prefix = raw_service.prefix.as_deref().unwrap_or(&raw_service.id);
        validate_identifier(prefix, "service prefix")?;
        if !ids.insert(raw_service.id.clone()) {
            return Err(GatewayError::InvalidInput(
                "service ids must be unique".to_owned(),
            ));
        }
        if !prefixes.insert(prefix.to_owned()) {
            return Err(GatewayError::InvalidInput(
                "service prefixes must be unique".to_owned(),
            ));
        }
        let endpoint = Url::parse(&raw_service.endpoint)
            .map_err(|_| GatewayError::InvalidInput("service endpoint is invalid".to_owned()))?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(GatewayError::InvalidInput(
                "service endpoint must be http or https".to_owned(),
            ));
        }
        let timeout = parse_duration(&raw_service.timeout, "timeout", MAX_TIMEOUT)?;
        let refresh = parse_duration(
            &raw_service.refresh_interval,
            "refresh interval",
            MAX_REFRESH,
        )?;
        let (headers, service_healthy) = resolve_headers(&raw_service.headers, config_path)?;
        healthy &= service_healthy;
        let Some(headers) = headers else {
            continue;
        };
        let credential_revision = header_revision(&headers);
        services.push(ResolvedService {
            spec: ServiceSpec {
                id: ServiceId::new(id),
                prefix: prefix.to_owned(),
                endpoint,
                enabled: raw_service.enabled,
                timeout,
                refresh_interval: refresh,
            },
            revision: Revision {
                source_uid: config_path.to_string_lossy().into_owned(),
                generation: revision,
                credential_revision,
            },
            headers,
        });
    }
    Ok((ConfigSnapshot { revision, services }, healthy))
}

fn validate_identifier(value: &str, field: &str) -> Result<String, GatewayError> {
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || value.starts_with('-')
        || value.ends_with('-')
        || value.contains("__")
    {
        return Err(GatewayError::InvalidInput(format!("{field} is invalid")));
    }
    Ok(value.to_owned())
}

fn parse_duration(value: &str, field: &str, max: Duration) -> Result<Duration, GatewayError> {
    let duration = humantime::parse_duration(value)
        .map_err(|_| GatewayError::InvalidInput(format!("{field} is invalid")))?;
    if duration.is_zero() || duration > max {
        return Err(GatewayError::InvalidInput(format!(
            "{field} is out of range"
        )));
    }
    Ok(duration)
}

fn resolve_headers(
    headers: &[RawHeader],
    config_path: &Path,
) -> Result<(Option<SensitiveHeaders>, bool), GatewayError> {
    let base = config_path.parent().unwrap_or_else(|| Path::new("."));
    let mut resolved = Vec::with_capacity(headers.len());
    let mut healthy = true;
    let mut names = HashSet::new();
    for header in headers {
        if header.name.is_empty() || !names.insert(header.name.to_ascii_lowercase()) {
            return Err(GatewayError::InvalidInput(
                "header names must be unique and non-empty".to_owned(),
            ));
        }
        if header.value_file.is_some() == header.value_env.is_some() {
            return Err(GatewayError::InvalidInput(
                "each header needs one file or environment reference".to_owned(),
            ));
        }
        let value = if let Some(file) = &header.value_file {
            std::fs::read(base.join(file)).map(|value| strip_one_line_ending(&value))
        } else {
            std::env::var_os(header.value_env.as_deref().unwrap_or_default())
                .map(|value| value.to_string_lossy().as_bytes().to_vec())
                .ok_or_else(|| std::io::Error::other("missing"))
        };
        match value {
            Ok(value) if !value.is_empty() && !contains_line_break(&value) => {
                match String::from_utf8(value) {
                    Ok(value) => resolved.push((header.name.clone(), value)),
                    Err(_) => healthy = false,
                }
            }
            _ => healthy = false,
        }
    }
    if healthy {
        Ok((Some(SensitiveHeaders::new(resolved)), true))
    } else {
        Ok((None, false))
    }
}

fn header_revision(headers: &SensitiveHeaders) -> String {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    let key = KEY.get_or_init(|| {
        let mut key = [0_u8; 32];
        let rng = ring::rand::SystemRandom::new();
        if rng.fill(&mut key).is_err() {
            // This branch is not expected on supported platforms. The key is
            // process-local and the revision remains opaque either way.
            key.copy_from_slice(b"latchkey-revision-key-fallback-1");
        }
        key
    });
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut input = Vec::new();
    headers.iter().for_each(|(name, value)| {
        input.extend_from_slice(name.as_bytes());
        input.push(0);
        input.extend_from_slice(value.as_bytes());
        input.push(0);
    });
    hmac::sign(&signing_key, &input)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn strip_one_line_ending(value: &[u8]) -> Vec<u8> {
    if value.ends_with(b"\r\n") {
        value[..value.len() - 2].to_vec()
    } else if value.ends_with(b"\n") {
        value[..value.len() - 1].to_vec()
    } else {
        value.to_vec()
    }
}

fn contains_line_break(value: &[u8]) -> bool {
    value.iter().any(|byte| *byte == b'\r' || *byte == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("latchkey-{name}-{}", std::process::id()))
    }

    #[test]
    fn parses_toml_and_resolves_relative_file_headers() {
        let dir = temp_path("config");
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("local.toml");
        fs::write(dir.join("token"), "Bearer secret\r\n").expect("secret");
        fs::write(&path, "version = 1\n[[services]]\nid = 'anvil'\nendpoint = 'https://anvil.test/mcp'\ntimeout = '30s'\n[[services.headers]]\nname = 'Authorization'\nvalue_file = 'token'\n").expect("config");
        let source = LocalSource::new(&path);
        let (snapshot, healthy) = source.load(1).expect("load");
        assert!(healthy);
        assert_eq!(snapshot.services[0].spec.prefix, "anvil");
        assert_eq!(
            snapshot.services[0].headers.iter().next().unwrap().1,
            "Bearer secret"
        );
        assert!(!format!("{:?}", snapshot).contains("Bearer secret"));
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(dir.join("token"));
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn invalid_credential_does_not_discard_other_services() {
        let path = temp_path("partial");
        fs::write(&path, "version = 1\n[[services]]\nid = 'good'\nendpoint = 'http://good.test/mcp'\ntimeout = '1s'\n[[services]]\nid = 'bad'\nendpoint = 'http://bad.test/mcp'\ntimeout = '1s'\n[[services.headers]]\nname = 'Authorization'\nvalue_env = 'LATCHKEY_MISSING_TEST_ENV'\n").expect("config");
        let (snapshot, healthy) = LocalSource::new(&path).load(4).expect("load");
        assert!(!healthy);
        assert_eq!(snapshot.services.len(), 1);
        assert_eq!(snapshot.services[0].spec.id, ServiceId::new("good"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn identifiers_and_endpoints_are_validated() {
        let raw = RawConfig {
            version: 1,
            services: vec![RawService {
                id: "Bad".to_owned(),
                prefix: None,
                endpoint: "file:///secret".to_owned(),
                enabled: true,
                timeout: "1s".to_owned(),
                refresh_interval: "5m".to_owned(),
                headers: Vec::new(),
            }],
        };
        assert!(normalize(raw, Path::new("config.toml"), 1).is_err());
    }

    #[test]
    fn rejects_userinfo_and_embedded_header_line_breaks() {
        let path = temp_path("newline");
        fs::write(&path, "version = 1\n[[services]]\nid = 'anvil'\nendpoint = 'https://user:secret@anvil.test/mcp'\ntimeout = '1s'\n").expect("config");
        assert!(LocalSource::new(&path).load(1).is_err());
        fs::write(&path, "version = 1\n[[services]]\nid = 'anvil'\nendpoint = 'https://anvil.test/mcp'\ntimeout = '1s'\n[[services.headers]]\nname = 'Authorization'\nvalue_env = 'LATCHKEY_NEWLINE_TEST'\n").expect("config");
        unsafe { std::env::set_var("LATCHKEY_NEWLINE_TEST", "Bearer bad\nvalue") };
        let (snapshot, healthy) = LocalSource::new(&path).load(1).expect("load");
        assert!(!healthy);
        assert!(snapshot.services.is_empty());
        unsafe { std::env::remove_var("LATCHKEY_NEWLINE_TEST") };
        let _ = fs::remove_file(path);
    }

    #[test]
    fn standalone_loading_ignores_kube_configuration() {
        let path = temp_path("no-kube");
        fs::write(
            &path,
            "version = 1\n[[services]]\nid = 'anvil'\nendpoint = 'http://anvil.test/mcp'\ntimeout = '1s'\n",
        )
        .expect("config");
        unsafe { std::env::set_var("KUBECONFIG", "/path/that/does/not/exist") };
        let (snapshot, healthy) = LocalSource::new(&path).load(1).expect("local load");
        assert!(healthy);
        assert_eq!(snapshot.services.len(), 1);
        unsafe { std::env::remove_var("KUBECONFIG") };
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn reload_retains_last_good_and_detects_disable_and_rotation() {
        let dir = temp_path("reload");
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("local.toml");
        let credential = dir.join("credential");
        fs::write(&credential, "Bearer first\n").expect("credential");
        let config = |enabled: bool| {
            format!(
                "version = 1\n[[services]]\nid = 'anvil'\nenabled = {enabled}\nendpoint = 'http://anvil.test/mcp'\ntimeout = '1s'\n[[services.headers]]\nname = 'Authorization'\nvalue_file = 'credential'\n"
            )
        };
        fs::write(&path, config(true)).expect("config");
        let (snapshot_tx, mut snapshot_rx) = watch::channel(ConfigSnapshot {
            revision: 0,
            services: Vec::new(),
        });
        let (state_tx, mut state_rx) = watch::channel(SourceState::default());
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            let source = LocalSource::new(&path);
            async move { source.run(snapshot_tx, state_tx, cancel).await }
        });
        tokio::time::timeout(Duration::from_secs(2), snapshot_rx.changed())
            .await
            .expect("initial timeout")
            .expect("initial snapshot");
        tokio::time::timeout(Duration::from_secs(2), state_rx.changed())
            .await
            .expect("initial state timeout")
            .expect("initial state");
        assert!(state_rx.borrow().healthy);
        let first_revision = snapshot_rx.borrow().revision;
        assert_eq!(
            snapshot_rx.borrow().services[0]
                .headers
                .iter()
                .next()
                .unwrap()
                .1,
            "Bearer first"
        );

        fs::write(&path, "version = ").expect("invalid config");
        tokio::time::timeout(Duration::from_secs(2), state_rx.changed())
            .await
            .expect("invalid timeout")
            .expect("invalid state");
        assert!(!state_rx.borrow().healthy);
        assert_eq!(snapshot_rx.borrow().revision, first_revision);

        fs::write(&credential, "Bearer second\n").expect("rotated credential");
        fs::write(&path, config(false)).expect("disabled config");
        let (direct, direct_healthy) = LocalSource::new(&path).load(9).expect("updated config");
        assert!(direct_healthy);
        assert!(!direct.services[0].spec.enabled);
        assert!(!task.is_finished(), "source stopped before reload");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                snapshot_rx.changed().await.expect("reload snapshot");
                if !snapshot_rx.borrow().services[0].spec.enabled {
                    break;
                }
            }
        })
        .await
        .expect("reload timeout");
        assert!(!snapshot_rx.borrow().services[0].spec.enabled);
        assert_eq!(
            snapshot_rx.borrow().services[0]
                .headers
                .iter()
                .next()
                .unwrap()
                .1,
            "Bearer second"
        );

        cancel.cancel();
        task.await.expect("join").expect("source shutdown");
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(credential);
        let _ = fs::remove_dir(dir);
    }
}
