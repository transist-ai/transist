// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Canonical hashing primitives shared by workflow and plugin discovery.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Computes a lowercase hexadecimal SHA-256 digest for a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_preserves_fixed_width_lowercase_representation() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"39"),
            "0b918943df0962bc7a1824c0555a389347b4febdc7cf9d1254406d80ce44e3f9"
        );
    }

    #[test]
    fn file_digest_preserves_canonical_representation() {
        assert_eq!(
            digest_file_entries(&[("a.txt".to_string(), 0o644, b"a".to_vec())]),
            "8a1719e27875f02340d27792bbcbfa08e4f17c14a4fd75959b1246805e5e44f7"
        );
    }

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
        assert_eq!(
            digest_named_entries(&first),
            "2f83ee2a77e26e26bfb27eaeec1263a34500f29113cefd55bcf44acdd62b884a"
        );
    }
}
