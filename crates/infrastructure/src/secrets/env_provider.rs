//! Secrets read from the process's environment variables.
//!
//! The simplest adapter that honours the "secrets are not versioned" rule: the
//! `.http` says `Authorization: Bearer {{api_token}}` and the value arrives
//! from `HTS_SECRET_API_TOKEN`, which can come from a git-ignored `.env`, from
//! `direnv`, or from a CI's secret manager.
//!
//! A future adapter against the operating system's keychain will implement the
//! same port and can replace it without touching any use case.

use std::collections::BTreeMap;

use async_trait::async_trait;
use http_studio_application::{ApplicationError, SecretProvider};

/// The prefix that marks an environment variable as a workspace secret.
const PREFIX: &str = "HTS_SECRET_";

/// A secret provider backed by environment variables.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvSecretProvider;

impl EnvSecretProvider {
    /// Creates the provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// How many secrets the process environment holds right now.
    ///
    /// It returns the count and not the values on purpose: `hts info` needs to
    /// be able to say "there are two" in output that gets pasted into an issue,
    /// and a token printed there is already burned.
    #[must_use]
    pub fn visible_count() -> usize {
        std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|key| Self::variable_name(key).is_some())
            .count()
    }

    /// Normalises `HTS_SECRET_API_TOKEN` to the variable name `api_token`.
    ///
    /// It is lowercased because workspace variables are written in
    /// `snake_case`, and forcing the `.http` to shout would be gratuitous.
    fn variable_name(key: &str) -> Option<String> {
        key.strip_prefix(PREFIX)
            .filter(|name| !name.is_empty())
            .map(str::to_lowercase)
    }
}

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn secrets(&self) -> Result<BTreeMap<String, String>, ApplicationError> {
        Ok(std::env::vars()
            .filter_map(|(key, value)| Some((Self::variable_name(&key)?, value)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normaliza_el_nombre_de_la_variable() {
        assert_eq!(
            EnvSecretProvider::variable_name("HTS_SECRET_API_TOKEN"),
            Some("api_token".to_owned())
        );
    }

    #[test]
    fn ignora_variables_sin_el_prefijo() {
        assert_eq!(EnvSecretProvider::variable_name("PATH"), None);
    }

    #[test]
    fn ignora_el_prefijo_a_secas() {
        assert_eq!(EnvSecretProvider::variable_name("HTS_SECRET_"), None);
    }
}
