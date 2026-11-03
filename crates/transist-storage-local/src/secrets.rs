// SPDX-License-Identifier: MPL-2.0
use anyhow::Result;
use transist_core::ports::SecretProviderPort;

/// Secret provider that reads directly from the local process environment.
#[derive(Debug, Default)]
pub struct EnvSecretProvider;

impl SecretProviderPort for EnvSecretProvider {
    fn get_secret(&self, name: &str) -> Result<Option<String>> {
        Ok(std::env::var(name).ok())
    }

    fn has_secret(&self, name: &str) -> Result<bool> {
        Ok(std::env::var_os(name).is_some())
    }
}
