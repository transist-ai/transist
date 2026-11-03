// SPDX-License-Identifier: MPL-2.0
/// Default `config.yaml` content written into a new Transist home.
pub const DEFAULT_HOME_CONFIG_YAML: &str = "version: v1\nruntime:\n  mode: governed\npolicy:\n  profile: allowlist\n  sources: [workflows, workflow-authorizations]\n  remote_secret_delivery:\n    process: false\n    http: false\n    mcp: false\n";

/// Default `toolchains.yaml` content written into a new Transist home.
pub const DEFAULT_REPO_CONFIG_YAML: &str = "schema: transist.toolchains.v1\ntoolchains: {}\n";
