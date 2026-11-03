// SPDX-License-Identifier: MPL-2.0
//! Canonical capability requests shared by recipes, policy, and TEP.

use crate::AdapterKind;
use crate::filesystem_policy::{FilesystemPolicy, WorkspaceAccessMode};
use crate::network_policy::{NetworkAccessMode, NetworkPolicy};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use url::Url;

/// Capability namespaces understood by the governed runtime.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityName {
    /// Permission to use an adapter family.
    Adapter,
    /// Permission to load a plugin package.
    Plugin,
    /// Permission to invoke a plugin action.
    Action,
    /// Permission to use a local or external provider.
    Provider,
    /// Permission to read a logical secret reference.
    Secret,
    /// Permission to access a filesystem resource.
    Filesystem,
    /// Permission to connect to or redirect to a network origin.
    Network,
    /// Permission to request or decide a manual approval gate.
    Approval,
}

impl CapabilityName {
    /// Returns the stable wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adapter => "adapter",
            Self::Plugin => "plugin",
            Self::Action => "action",
            Self::Provider => "provider",
            Self::Secret => "secret",
            Self::Filesystem => "filesystem",
            Self::Network => "network",
            Self::Approval => "approval",
        }
    }

    fn allowed_operations(self) -> &'static [&'static str] {
        match self {
            Self::Adapter | Self::Action => &["invoke"],
            Self::Plugin => &["load"],
            Self::Provider => &["use"],
            Self::Secret => &["read"],
            Self::Filesystem => &["list", "read", "create", "write", "delete", "rename"],
            Self::Network => &["connect", "redirect"],
            Self::Approval => &["request", "decide"],
        }
    }
}

impl fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One normalized capability request or exact policy grant.
#[derive(Debug, Clone, Serialize, Eq, PartialEq, Ord, PartialOrd)]
pub struct CapabilityRequest {
    /// Capability namespace.
    pub capability: CapabilityName,
    /// Canonical resource identifier within the namespace.
    pub resource: String,
    /// Canonical operations requested for the resource.
    pub operations: Vec<String>,
}

/// One source dependency observed while projecting authority.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CapabilitySourceDependency {
    /// The authored value is a literal.
    Literal,
    /// The value depends on an immutable workflow input.
    WorkflowInput {
        /// Input name.
        name: String,
    },
    /// The value depends on a mutable workflow variable.
    WorkflowVariable {
        /// Variable name.
        name: String,
    },
    /// The value depends on a step output.
    StepOutput {
        /// Producing step and output path.
        reference: String,
    },
    /// The value depends on a runtime value unavailable at run birth.
    PostBirth {
        /// Stable description of the dependency.
        reference: String,
    },
}

/// Authority source dependencies for one projected field.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
pub struct CapabilitySource {
    /// Authority-bearing field path.
    pub field: String,
    /// Dependencies found in that field, in canonical order.
    #[serde(default)]
    pub dependencies: Vec<CapabilitySourceDependency>,
}

/// Typed authority projection used to validate and persist capability requests.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct CapabilityProjection {
    /// Normalized capability requests emitted by the projection.
    pub requests: Vec<CapabilityRequest>,
    /// Source dependencies that produced the requests.
    #[serde(default)]
    pub sources: Vec<CapabilitySource>,
}

impl<'de> Deserialize<'de> for CapabilityRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawCapabilityRequest {
            capability: CapabilityName,
            resource: String,
            operations: Vec<String>,
        }
        let raw = RawCapabilityRequest::deserialize(deserializer)?;
        Self::new(raw.capability, raw.resource, raw.operations).map_err(serde::de::Error::custom)
    }
}

impl CapabilityRequest {
    /// Creates a request after validating and canonicalizing its resource and operations.
    pub fn new(
        capability: CapabilityName,
        resource: impl AsRef<str>,
        operations: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, CapabilityError> {
        let resource = canonical_resource(capability, resource.as_ref())?;
        let mut normalized = BTreeSet::new();
        for operation in operations {
            let operation = operation.as_ref().trim().to_ascii_lowercase();
            if !capability
                .allowed_operations()
                .contains(&operation.as_str())
            {
                return Err(CapabilityError::InvalidOperation {
                    capability,
                    operation,
                });
            }
            normalized.insert(operation);
        }
        if normalized.is_empty() {
            return Err(CapabilityError::EmptyOperations);
        }
        Ok(Self {
            capability,
            resource,
            operations: normalized.into_iter().collect(),
        })
    }

    /// Returns true when this grant covers every operation in the request.
    pub fn grants(&self, requested: &Self) -> bool {
        self.capability == requested.capability
            && self.resource == requested.resource
            && requested
                .operations
                .iter()
                .all(|operation| self.operations.binary_search(operation).is_ok())
    }
}

/// Canonicalization failures for capability configuration or generated requests.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CapabilityError {
    /// The resource is empty or malformed.
    InvalidResource(String),
    /// An operation is not valid for the capability namespace.
    InvalidOperation {
        /// Capability namespace that rejected the operation.
        capability: CapabilityName,
        /// Invalid operation name.
        operation: String,
    },
    /// A request must contain at least one operation.
    EmptyOperations,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResource(resource) => {
                write!(f, "invalid capability resource '{resource}'")
            }
            Self::InvalidOperation {
                capability,
                operation,
            } => write!(
                f,
                "operation '{operation}' is invalid for {capability} capability"
            ),
            Self::EmptyOperations => f.write_str("capability operations cannot be empty"),
        }
    }
}

impl std::error::Error for CapabilityError {}

fn canonical_resource(capability: CapabilityName, raw: &str) -> Result<String, CapabilityError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('*') || raw.contains('\0') {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    match capability {
        CapabilityName::Adapter => raw
            .parse::<AdapterKind>()
            .map(|kind| kind.to_string())
            .map_err(|_| CapabilityError::InvalidResource(raw.to_string())),
        CapabilityName::Plugin | CapabilityName::Action => {
            canonical_plugin_resource(capability, raw)
        }
        CapabilityName::Provider => match raw {
            "local" | "external" => Ok(raw.to_string()),
            _ => Err(CapabilityError::InvalidResource(raw.to_string())),
        },
        CapabilityName::Secret => {
            if raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
            {
                Ok(raw.to_string())
            } else {
                Err(CapabilityError::InvalidResource(raw.to_string()))
            }
        }
        CapabilityName::Filesystem => canonical_filesystem_resource(raw),
        CapabilityName::Network => canonical_network_resource(raw),
        CapabilityName::Approval => {
            let mut parts = raw.split('/');
            let workflow = parts.next().unwrap_or_default();
            let gate = parts.next().unwrap_or_default();
            if parts.next().is_some() || workflow.is_empty() || gate.is_empty() {
                return Err(CapabilityError::InvalidResource(raw.to_string()));
            }
            Ok(format!("{workflow}/{gate}"))
        }
    }
}

fn canonical_plugin_resource(
    capability: CapabilityName,
    raw: &str,
) -> Result<String, CapabilityError> {
    let (package, action) = raw
        .split_once('/')
        .map_or((raw, None), |(package, action)| (package, Some(action)));
    let (plugin, version) = package
        .rsplit_once('@')
        .ok_or_else(|| CapabilityError::InvalidResource(raw.to_string()))?;
    if plugin.is_empty() || version.is_empty() || plugin.contains('/') {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    let canonical_version = semver::Version::parse(version)
        .map(|version| version.to_string())
        .or_else(|_| {
            (version.starts_with('v')
                && version.len() > 1
                && version[1..]
                    .chars()
                    .all(|character| character.is_ascii_digit()))
            .then(|| version.to_string())
            .ok_or(())
        })
        .map_err(|_| CapabilityError::InvalidResource(raw.to_string()))?;
    let package = format!("{}@{canonical_version}", plugin.to_ascii_lowercase());
    match (capability, action) {
        (CapabilityName::Plugin, None) => Ok(package),
        (CapabilityName::Action, Some(action)) if !action.is_empty() => {
            Ok(format!("{package}/{}", action.trim()))
        }
        _ => Err(CapabilityError::InvalidResource(raw.to_string())),
    }
}

fn canonical_filesystem_resource(raw: &str) -> Result<String, CapabilityError> {
    let (scheme, path) = raw
        .split_once("://")
        .ok_or_else(|| CapabilityError::InvalidResource(raw.to_string()))?;
    if !matches!(scheme, "workspace" | "run" | "file") || path.is_empty() {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    let path_without_root = path.trim_start_matches('/');
    if path_without_root.is_empty()
        || path_without_root
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    if scheme == "run"
        && !(path_without_root == "artifacts"
            || path_without_root == "evidence"
            || path_without_root.starts_with("artifacts/")
            || path_without_root.starts_with("evidence/"))
    {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    if scheme == "file" && !Path::new(path).is_absolute() {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    Ok(format!("{scheme}://{path}"))
}

fn canonical_network_resource(raw: &str) -> Result<String, CapabilityError> {
    let url = Url::parse(raw).map_err(|_| CapabilityError::InvalidResource(raw.to_string()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.query().is_some()
    {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    let host = url
        .host_str()
        .filter(|host| host.is_ascii())
        .ok_or_else(|| CapabilityError::InvalidResource(raw.to_string()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| CapabilityError::InvalidResource(raw.to_string()))?;
    Ok(format!(
        "{}://{}:{}",
        url.scheme(),
        host.to_ascii_lowercase(),
        port
    ))
}

/// Builds the canonical capabilities required by one invoke request.
#[allow(clippy::too_many_arguments)]
pub fn invoke_capabilities(
    adapter: AdapterKind,
    plugin: &str,
    plugin_version: Option<&str>,
    action: &str,
    provider: &str,
    secret_refs: &[String],
    filesystem_policy: &FilesystemPolicy,
    workspace_root: &Path,
    home_dir: &Path,
    network_origins: &[String],
) -> Result<Vec<CapabilityRequest>, CapabilityError> {
    let mut requests = vec![
        CapabilityRequest::new(CapabilityName::Adapter, adapter.to_string(), ["invoke"]),
        CapabilityRequest::new(CapabilityName::Provider, provider, ["use"]),
    ]
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    if let Some(plugin_version) = plugin_version.filter(|version| !version.trim().is_empty()) {
        let package = format!("{plugin}@{plugin_version}");
        requests.push(CapabilityRequest::new(
            CapabilityName::Plugin,
            &package,
            ["load"],
        )?);
        requests.push(CapabilityRequest::new(
            CapabilityName::Action,
            format!("{package}/{action}"),
            ["invoke"],
        )?);
    }
    requests.extend(filesystem_capabilities(
        filesystem_policy,
        workspace_root,
        home_dir,
    )?);
    for secret in secret_refs {
        requests.push(CapabilityRequest::new(
            CapabilityName::Secret,
            secret,
            ["read"],
        )?);
    }
    for origin in network_origins {
        requests.push(CapabilityRequest::new(
            CapabilityName::Network,
            origin,
            ["connect", "redirect"],
        )?);
    }
    requests.sort();
    requests.dedup();
    Ok(requests)
}

/// Builds filesystem capabilities from the effective policy and runtime roots.
pub fn filesystem_capabilities(
    policy: &FilesystemPolicy,
    workspace_root: &Path,
    _home_dir: &Path,
) -> Result<Vec<CapabilityRequest>, CapabilityError> {
    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.into());
    let workspace_mode = policy
        .workspace_access
        .as_ref()
        .map(|access| access.mode)
        .unwrap_or(WorkspaceAccessMode::Full);
    let mut requests = Vec::new();
    let operations = match workspace_mode {
        WorkspaceAccessMode::Full => vec!["list", "read", "create", "write", "delete", "rename"],
        WorkspaceAccessMode::ReadOnly => vec!["list", "read"],
        WorkspaceAccessMode::Custom => Vec::new(),
    };
    if !operations.is_empty() {
        requests.push(CapabilityRequest::new(
            CapabilityName::Filesystem,
            format!("workspace://{}", workspace_root.display()),
            operations,
        )?);
    }
    for raw_path in &policy.read_allow_paths {
        // Read grants must resolve to a real path at birth; an unresolved
        // read target denies authority projection rather than silently
        // persisting a capability request for a resource that never
        // existed. Write grants remain `allow_missing=true` because a
        // write-allow path is a canonical create target that may not exist
        // yet.
        let path = canonical_external_policy_path(raw_path, false)?;
        requests.push(CapabilityRequest::new(
            CapabilityName::Filesystem,
            format!("file://{}", path.display()),
            ["list", "read"],
        )?);
    }
    for raw_path in &policy.write_allow_paths {
        let path = canonical_external_policy_path(raw_path, true)?;
        requests.push(CapabilityRequest::new(
            CapabilityName::Filesystem,
            format!("file://{}", path.display()),
            ["list", "read", "create", "write", "delete", "rename"],
        )?);
    }
    requests.push(CapabilityRequest::new(
        CapabilityName::Filesystem,
        "run://artifacts",
        ["create", "write"],
    )?);
    requests.push(CapabilityRequest::new(
        CapabilityName::Filesystem,
        "run://evidence",
        ["create", "write"],
    )?);
    Ok(requests)
}

/// Builds normalized network origins from effective policy and plugin metadata.
pub fn network_origins(
    policy: &NetworkPolicy,
    base_url: Option<&str>,
) -> Result<Vec<String>, CapabilityError> {
    if policy.mode == NetworkAccessMode::None {
        return Ok(Vec::new());
    }
    let mut origins = BTreeSet::new();
    if let Some(base_url) = base_url {
        let url = Url::parse(base_url)
            .map_err(|_| CapabilityError::InvalidResource(base_url.to_string()))?;
        let origin = network_origin_from_url(&url)?;
        if policy.mode != NetworkAccessMode::Loopback || origin_is_loopback(&origin) {
            origins.insert(origin);
        }
    }
    let ports = if policy.allow_ports.is_empty() {
        vec![if policy.tls_required { 443 } else { 80 }]
    } else {
        policy.allow_ports.clone()
    };
    for host in &policy.allow_hosts {
        if policy.mode == NetworkAccessMode::Loopback && !host_is_loopback(host) {
            continue;
        }
        for port in &ports {
            let scheme = if policy.tls_required { "https" } else { "http" };
            let authority = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host.clone()
            };
            let url = Url::parse(&format!("{scheme}://{authority}:{port}/"))
                .map_err(|_| CapabilityError::InvalidResource(host.clone()))?;
            origins.insert(network_origin_from_url(&url)?);
        }
    }
    Ok(origins.into_iter().collect())
}

fn network_origin_from_url(url: &Url) -> Result<String, CapabilityError> {
    let host = url
        .host_str()
        .filter(|host| host.is_ascii())
        .ok_or_else(|| CapabilityError::InvalidResource(url.to_string()))?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_ascii_lowercase()
    };
    let port = url
        .port_or_known_default()
        .ok_or_else(|| CapabilityError::InvalidResource(url.to_string()))?;
    let origin = format!("{}://{host}:{port}/", url.scheme().to_ascii_lowercase());
    CapabilityRequest::new(CapabilityName::Network, &origin, ["connect"])
        .map(|request| request.resource)
}

fn host_is_loopback(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false)
}

fn origin_is_loopback(origin: &str) -> bool {
    Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(host_is_loopback))
        .unwrap_or(false)
}

/// Expands and canonicalizes one external filesystem policy path.
///
/// Read grants require an existing target. Write grants may set
/// `allow_missing` because they identify a canonical create target.
pub fn canonical_external_policy_path(
    raw: &str,
    allow_missing: bool,
) -> Result<PathBuf, CapabilityError> {
    if raw != raw.trim() {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    if raw.is_empty() || raw.contains(['\0', '*', '?', '[', ']']) {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    // `~` and `~/...` expand to the operator's OS home directory (matching
    // the documented contract and the adapter sandbox's own tilde
    // expansion), never to the Transist home directory passed in via
    // `home_dir` — those are different directories.
    let path = if raw.starts_with("~/") {
        let os_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| CapabilityError::InvalidResource(raw.to_string()))?;
        os_home.join(raw.strip_prefix("~/").expect("checked prefix"))
    } else {
        PathBuf::from(raw)
    };
    if !path.is_absolute() {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    if let Ok(path) = path.canonicalize() {
        return Ok(path);
    }
    if !allow_missing {
        return Err(CapabilityError::InvalidResource(raw.to_string()));
    }
    let mut missing = Vec::new();
    let mut cursor = path.as_path();
    while !cursor.exists() {
        let Some(name) = cursor.file_name() else {
            return Err(CapabilityError::InvalidResource(raw.to_string()));
        };
        missing.push(name.to_os_string());
        cursor = cursor
            .parent()
            .ok_or_else(|| CapabilityError::InvalidResource(raw.to_string()))?;
    }
    let mut resolved = cursor
        .canonicalize()
        .map_err(|_| CapabilityError::InvalidResource(raw.to_string()))?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_and_matches_exact_grants() {
        let request = CapabilityRequest::new(
            CapabilityName::Network,
            "HTTPS://Example.COM:443/",
            ["redirect", "connect"],
        )
        .expect("request");
        let grant = CapabilityRequest::new(
            CapabilityName::Network,
            "https://example.com:443/",
            ["connect", "redirect"],
        )
        .expect("grant");
        assert_eq!(request, grant);
        assert!(grant.grants(&request));
    }

    #[test]
    fn filesystem_capabilities_reject_unresolved_read_grants() {
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec!["/definitely/does/not/exist/anywhere".to_string()],
            write_allow_paths: Vec::new(),
        };
        let error = filesystem_capabilities(
            &policy,
            Path::new("/tmp/workspace"),
            Path::new("/tmp/transist-home"),
        )
        .expect_err("unresolved read grant must be rejected before run birth");
        assert!(matches!(error, CapabilityError::InvalidResource(_)));
    }

    #[test]
    fn filesystem_capabilities_accept_missing_write_grants() {
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: Vec::new(),
            write_allow_paths: vec!["/definitely/does/not/exist/anywhere".to_string()],
        };
        filesystem_capabilities(
            &policy,
            Path::new("/tmp/workspace"),
            Path::new("/tmp/transist-home"),
        )
        .expect("write grants are canonical create targets and may not exist yet");
    }

    #[test]
    fn filesystem_capabilities_accept_existing_read_grants() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let existing = tmp.path().join("gitconfig");
        std::fs::write(&existing, "").expect("write fixture file");
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: vec![existing.display().to_string()],
            write_allow_paths: Vec::new(),
        };
        filesystem_capabilities(
            &policy,
            Path::new("/tmp/workspace"),
            Path::new("/tmp/transist-home"),
        )
        .expect("an existing read grant must resolve to a valid capability request");
    }

    #[test]
    fn filesystem_write_grants_include_read_operations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let policy = FilesystemPolicy {
            workspace_access: None,
            read_allow_paths: Vec::new(),
            write_allow_paths: vec![tmp.path().display().to_string()],
        };
        let requests = filesystem_capabilities(
            &policy,
            Path::new("/tmp/workspace"),
            Path::new("/tmp/transist-home"),
        )
        .expect("filesystem capabilities");
        let grant = requests
            .iter()
            .find(|request| {
                request.resource
                    == format!(
                        "file://{}",
                        tmp.path()
                            .canonicalize()
                            .expect("canonical tempdir")
                            .display()
                    )
            })
            .expect("write grant");
        assert_eq!(
            grant.operations,
            vec!["create", "delete", "list", "read", "rename", "write"]
        );
    }

    #[test]
    fn external_policy_paths_reject_surrounding_whitespace_and_bare_tilde() {
        assert!(canonical_external_policy_path(" /tmp/path", true).is_err());
        assert!(canonical_external_policy_path("/tmp/path ", true).is_err());
        assert!(canonical_external_policy_path("~", true).is_err());
    }

    #[test]
    fn rejects_wildcards_and_ambiguous_paths() {
        assert!(CapabilityRequest::new(CapabilityName::Secret, "TOKEN*", ["read"]).is_err());
        assert!(
            CapabilityRequest::new(
                CapabilityName::Filesystem,
                "workspace://root/../secret",
                ["read"]
            )
            .is_err()
        );
    }

    #[test]
    fn derives_read_only_filesystem_operations() {
        let policy = FilesystemPolicy {
            workspace_access: Some(crate::WorkspaceAccessPolicy {
                mode: WorkspaceAccessMode::ReadOnly,
            }),
            ..FilesystemPolicy::default()
        };
        let requests = filesystem_capabilities(
            &policy,
            Path::new(env!("CARGO_MANIFEST_DIR")),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .expect("filesystem capabilities");
        let workspace = requests
            .iter()
            .find(|request| request.resource.starts_with("workspace://"))
            .expect("workspace capability");
        assert_eq!(workspace.operations, vec!["list", "read"]);
    }

    #[test]
    fn derives_network_origins_from_policy_and_base_url() {
        let policy = NetworkPolicy {
            mode: NetworkAccessMode::Allowlist,
            allow_hosts: vec!["api.example.com".to_string()],
            allow_ports: vec![443, 8443],
            tls_required: true,
            max_redirects: 2,
        };
        assert_eq!(
            network_origins(&policy, Some("https://plugin.example.com/api"))
                .expect("network origins"),
            vec![
                "https://api.example.com:443",
                "https://api.example.com:8443",
                "https://plugin.example.com:443",
            ]
        );
    }
}
