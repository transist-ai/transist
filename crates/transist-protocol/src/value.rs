// SPDX-License-Identifier: MPL-2.0
//! Generic JSON-compatible kernel values used across runtime and storage layers.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Kernel-safe dynamic value type used by `transist-core` interfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum KernelValue {
    /// JSON `null`.
    #[default]
    Null,
    /// Boolean value.
    Bool(bool),
    /// Arbitrary JSON number.
    Number(serde_json::Number),
    /// UTF-8 string value.
    String(String),
    /// Ordered list of nested kernel values.
    Array(Vec<KernelValue>),
    /// String-keyed object of nested kernel values.
    Object(BTreeMap<String, KernelValue>),
}

impl KernelValue {
    /// Converts a `serde_json::Value` into the kernel-safe value representation.
    pub fn from_json(value: serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(value) => Self::Bool(value),
            serde_json::Value::Number(value) => Self::Number(value),
            serde_json::Value::String(value) => Self::String(value),
            serde_json::Value::Array(values) => {
                Self::Array(values.into_iter().map(Self::from_json).collect())
            }
            serde_json::Value::Object(values) => Self::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from_json(value)))
                    .collect(),
            ),
        }
    }

    /// Converts a kernel-safe value back into `serde_json::Value`.
    pub fn to_json(self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(value) => serde_json::Value::Bool(value),
            Self::Number(value) => serde_json::Value::Number(value),
            Self::String(value) => serde_json::Value::String(value),
            Self::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(Self::to_json).collect())
            }
            Self::Object(values) => serde_json::Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, value.to_json()))
                    .collect(),
            ),
        }
    }
}

impl From<serde_json::Value> for KernelValue {
    fn from(value: serde_json::Value) -> Self {
        Self::from_json(value)
    }
}

impl From<KernelValue> for serde_json::Value {
    fn from(value: KernelValue) -> Self {
        value.to_json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json_value() {
        let src = serde_json::json!({"ok": true, "n": 1, "arr": [1, 2]});
        let kernel = KernelValue::from_json(src.clone());
        let back: serde_json::Value = kernel.into();
        assert_eq!(src, back);
    }
}
