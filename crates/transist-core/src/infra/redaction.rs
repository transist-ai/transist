// SPDX-License-Identifier: MPL-2.0
//! Structured redaction helpers for runtime and adapter outputs.

use base64::Engine as _;
use transist_protocol::ExecutionResult;

const REDACTED: &str = "[REDACTED]";

/// Redacts known secret values from raw text and structured JSON payloads.
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    patterns: Vec<String>,
}

impl Redactor {
    /// Constructs a redactor from secret values.
    ///
    /// Secrets are normalized by dropping empty values, de-duplicating, and
    /// sorting longest-first for deterministic overlap handling.
    pub fn from_secrets<'a, I>(secrets: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut patterns = Vec::new();
        for secret in secrets {
            if secret.is_empty() {
                continue;
            }
            patterns.push(secret.to_string());
            for engine in [
                &base64::engine::general_purpose::STANDARD,
                &base64::engine::general_purpose::URL_SAFE,
            ] {
                let encoded = engine.encode(secret);
                patterns.push(encoded.clone());
                patterns.push(encoded.trim_end_matches('=').to_string());
            }
        }

        patterns.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        patterns.dedup();

        Self { patterns }
    }

    /// Redacts every exact secret substring from arbitrary text.
    pub fn redact_str(&self, input: &str) -> String {
        if self.patterns.is_empty() || input.is_empty() {
            return input.to_string();
        }

        let mut redacted = input.to_string();
        for pattern in &self.patterns {
            redacted = redacted.replace(pattern, REDACTED);
        }
        redacted
    }

    /// Returns whether a string contains a known raw or encoded secret.
    pub fn contains_secret(&self, input: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| !pattern.is_empty() && input.contains(pattern))
    }

    /// Returns whether bytes contain a known raw or encoded secret sequence.
    pub fn contains_secret_bytes(&self, input: &[u8]) -> bool {
        self.patterns.iter().any(|pattern| {
            !pattern.is_empty()
                && input
                    .windows(pattern.len())
                    .any(|window| window == pattern.as_bytes())
        })
    }

    /// Redacts fields of an execution result before the kernel persists it.
    pub fn redact_execution_result(&self, result: &mut ExecutionResult) {
        result.status = self.redact_str(&result.status);
        result.stdout = self.redact_str(&result.stdout);
        result.stderr = self.redact_str(&result.stderr);
        result.metrics = result
            .metrics
            .iter()
            .map(|(name, value)| (self.redact_str(name), *value))
            .collect();
        result.output = self.redact_json(result.output.clone());
        if let Some(error) = result.error.as_mut() {
            error.message = self.redact_str(&error.message);
        }
        if let Some(usage) = result.usage.as_mut() {
            usage.provider = usage
                .provider
                .as_deref()
                .map(|value| self.redact_str(value));
            usage.model = usage.model.as_deref().map(|value| self.redact_str(value));
        }
    }

    /// Redacts secret values in JSON keys and string fields while preserving
    /// arrays, booleans, and numbers.
    pub fn redact_json(&self, value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(text) => serde_json::Value::String(self.redact_str(&text)),
            serde_json::Value::Array(values) => serde_json::Value::Array(
                values
                    .into_iter()
                    .map(|value| self.redact_json(value))
                    .collect(),
            ),
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .map(|(key, value)| (self.redact_str(&key), self.redact_json(value)))
                    .collect(),
            ),
            other => other,
        }
    }
}
