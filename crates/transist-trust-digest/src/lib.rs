// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Canonical hashing primitives shared by workflow and plugin discovery.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Computes a lowercase hexadecimal SHA-256 digest for a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Normalizes CRLF and bare CR line endings to LF without changing other bytes.
pub fn normalize_lf(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                normalized.push(b'\n');
                if index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                    index += 1;
                }
            }
            byte => normalized.push(byte),
        }
        index += 1;
    }
    normalized
}

/// Computes the canonical digest for sorted logical paths and their content digests.
///
/// Each entry contributes `relative_path`, a NUL byte, its lowercase hexadecimal
/// content digest, and a trailing NUL byte. `BTreeMap` ordering is therefore part
/// of the digest contract and independent of filesystem enumeration order.
pub fn digest_named_entries(entries: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (relative_path, content_digest) in entries {
        hasher.update(relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(content_digest.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

/// Computes the canonical digest for sorted regular files and their metadata.
///
/// Each tuple contains a normalized relative path, permission mode bits, and
/// file bytes. The caller must provide entries in relative-path order.
pub fn digest_file_entries(entries: &[(String, u32, Vec<u8>)]) -> String {
    let mut hasher = Sha256::new();
    for (relative_path, mode, bytes) in entries {
        hasher.update(relative_path.as_bytes());
        hasher.update(b"\0");
        hasher.update(mode.to_le_bytes());
        hasher.update(bytes);
        hasher.update(b"\0");
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lf_handles_mixed_line_endings() {
        assert_eq!(normalize_lf(b"a\r\nb\rc\n"), b"a\nb\nc\n");
    }

    #[test]
    fn named_digest_is_independent_of_insertion_order() {
        let mut first = BTreeMap::new();
        first.insert("b.txt".to_string(), sha256_hex(b"b"));
        first.insert("a.txt".to_string(), sha256_hex(b"a"));

        let mut second = BTreeMap::new();
        second.insert("a.txt".to_string(), sha256_hex(b"a"));
        second.insert("b.txt".to_string(), sha256_hex(b"b"));

        assert_eq!(digest_named_entries(&first), digest_named_entries(&second));
    }
}
