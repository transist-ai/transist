// SPDX-License-Identifier: MPL-2.0
//! JSON transport sanitization for persisted inspection values.
//!
//! Persisted workflow inputs, event payloads, gate summaries, and diagnostics
//! are arbitrary runtime data. Before they cross the HTTP boundary, strings
//! that are absolute, home-relative, drive-letter, UNC, or traversal paths are
//! replaced with `[path redacted]`. Other strings, numbers, booleans, nulls,
//! JSON structure is preserved. Object keys are sanitized with deterministic
//! aliases when multiple keys redact to the same value. Leading `./` or `.\\`
//! components are ignored while classifying a candidate, so they cannot hide
//! a host path. Valid HTTP(S) URLs are opaque remote identifiers and are
//! preserved byte-for-byte. This is a transport-only transformation; persisted
//! records are never rewritten.

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::routes::{COMPANION_API_ROUTES, INSPECTION_GET_ROUTES};

const REDACTED_PATH: &str = "[path redacted]";
const MAX_PERCENT_DECODE_LAYERS: usize = 8;
const MAX_PATH_CANDIDATES: usize = 64;

/// Serializes an API DTO after recursively redacting host-path strings.
pub(crate) fn sanitized_json<T: Serialize>(payload: T) -> Response {
    let value = serde_json::to_value(payload).expect("API DTO serialization");
    Json(sanitize_json(value)).into_response()
}

/// Redacts host paths from a UTF-8 text response.
///
/// Invalid text is rejected so its source bytes cannot cross the API boundary.
pub(crate) fn sanitize_text(bytes: Vec<u8>) -> Result<Vec<u8>, ()> {
    String::from_utf8(bytes)
        .map(|text| sanitize_string(&text).into_bytes())
        .map_err(|_| ())
}

/// Recursively sanitizes arbitrary persisted JSON while preserving its shape.
pub(crate) fn sanitize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => serde_json::Value::String(sanitize_string(&value)),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(sanitize_json).collect())
        }
        serde_json::Value::Object(values) => serde_json::Value::Object(sanitize_object(values)),
        other => other,
    }
}

fn sanitize_object(
    values: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut sanitized = serde_json::Map::new();
    for (key, value) in values {
        let base = sanitize_string(&key);
        let mut candidate = base.clone();
        let mut collision = 2;
        while sanitized.contains_key(&candidate) {
            candidate = format!("{base} #{collision}");
            collision += 1;
        }
        sanitized.insert(candidate, sanitize_json(value));
    }
    sanitized
}

fn sanitize_string(value: &str) -> String {
    let (url, _) = split_trailing_url_punctuation(value);
    if is_host_path(value) && !is_valid_http_url(url) {
        return REDACTED_PATH.to_string();
    }

    value
        .split_inclusive(char::is_whitespace)
        .map(sanitize_token)
        .collect()
}

fn sanitize_token(token: &str) -> String {
    if token.trim().is_empty() {
        return token.to_string();
    }
    let trimmed = token.trim_end_matches(char::is_whitespace);
    let (url, punctuation) = split_trailing_url_punctuation(trimmed);
    if is_valid_http_url(url) {
        return format!("{}{}{}", url, punctuation, &token[trimmed.len()..]);
    }
    let has_trailing_punctuation = trimmed
        .chars()
        .next_back()
        .is_some_and(|character| matches!(character, '"' | '\'' | ',' | ';' | ':' | '.'));
    if is_host_path(trimmed) && !has_trailing_punctuation {
        return format!("{REDACTED_PATH}{}", &token[trimmed.len()..]);
    }

    let mut candidate_count = 0;
    for (index, character) in token.char_indices() {
        let prefix = &token[..index];
        let slash_boundary = index == 0
            || prefix
                .chars()
                .next_back()
                .is_some_and(|character| !character.is_ascii_alphanumeric());
        let candidate_start = (character == '/' && slash_boundary)
            || (character == '\\'
                && token.as_bytes().get(index + 1) == Some(&b'\\')
                && slash_boundary)
            || character == '~'
            || character == '.'
            || character == '%'
            || (character.is_ascii_alphabetic()
                && token.as_bytes().get(index + 1) == Some(&b':')
                && slash_boundary);
        if !candidate_start || index == 0 && token.starts_with("https://") {
            continue;
        }
        candidate_count += 1;
        if candidate_count > MAX_PATH_CANDIDATES {
            return REDACTED_PATH.to_string();
        }
        let candidate = token[index..]
            .trim_end_matches(char::is_whitespace)
            .trim_end_matches(|c: char| {
                matches!(
                    c,
                    '"' | '\'' | ')' | ']' | '}' | '>' | ',' | ';' | '.' | ':'
                )
            });
        if candidate.is_empty() {
            continue;
        }
        if is_host_path(candidate) {
            let suffix = &token[index + candidate.len()..];
            return format!("{prefix}{REDACTED_PATH}{suffix}");
        }
    }
    token.to_string()
}

fn is_valid_http_url(value: &str) -> bool {
    let scheme_end = if value.starts_with("http://") {
        7
    } else if value.starts_with("https://") {
        8
    } else {
        return false;
    };
    if value[scheme_end..].is_empty()
        || value
            .chars()
            .any(|character| character.is_ascii_control() || character.is_whitespace())
        || value.contains('\\')
        || !has_valid_percent_encoding(value)
    {
        return false;
    }

    let authority_end = value[scheme_end..]
        .find(['/', '?', '#'])
        .map(|index| scheme_end + index)
        .unwrap_or(value.len());
    let authority = &value[scheme_end..authority_end];
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host_port.is_empty() {
        return false;
    }
    if host_port.starts_with('[') {
        let Some(bracket_end) = host_port.find(']') else {
            return false;
        };
        if bracket_end == 1 {
            return false;
        }
        let suffix = &host_port[bracket_end + 1..];
        return suffix.is_empty()
            || suffix.strip_prefix(':').is_some_and(|port| {
                !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
            });
    }

    match host_port.split_once(':') {
        Some((host, port)) => {
            !host.is_empty() && !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
        }
        None => true,
    }
}

fn has_valid_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().all(|(index, byte)| {
        *byte != b'%'
            || (index + 2 < bytes.len()
                && hex_value(bytes[index + 1]).is_some()
                && hex_value(bytes[index + 2]).is_some())
    })
}

fn split_trailing_url_punctuation(value: &str) -> (&str, &str) {
    let sanitized = value.trim_end_matches(|character| {
        matches!(
            character,
            '"' | '\'' | ')' | ']' | '}' | '>' | ',' | ';' | '.'
        )
    });
    (sanitized, &value[sanitized.len()..])
}

fn is_host_path(value: &str) -> bool {
    let mut candidate = value.to_string();
    for _ in 0..=MAX_PERCENT_DECODE_LAYERS {
        if is_literal_host_path(&candidate) {
            return true;
        }
        let decoded = percent_decode_once(&candidate);
        if decoded == candidate {
            break;
        }
        candidate = decoded;
    }
    // A value that remains encoded after the bounded pass is hostile or
    // pathological. Fail closed rather than allowing deeper encoding to hide
    // a path or consume unbounded decoding work.
    is_literal_host_path(&candidate) || percent_decode_once(&candidate) != candidate
}

fn is_literal_host_path(value: &str) -> bool {
    if is_public_route(value) {
        return false;
    }
    let replaced = value.replace('\\', "/");
    let normalized = normalize_leading_current_components(&replaced);
    normalized.starts_with('/')
        || normalized.starts_with("~/")
        || (normalized.len() >= 3
            && normalized.as_bytes()[0].is_ascii_alphabetic()
            && normalized.as_bytes()[1] == b':'
            && normalized.as_bytes()[2] == b'/')
        || normalized.split('/').any(|component| component == "..")
}

fn percent_decode_once(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_public_route(value: &str) -> bool {
    if let Some((path, member)) = value.split_once("?member_id=") {
        let parts: Vec<_> = path.split('/').collect();
        return parts.len() == 8
            && parts[1..4] == ["api", "v1", "runs"]
            && parts[5] == "documents"
            && parts[7] == "content"
            && parts[6].len() == 64
            && parts[6]
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            && member.len() == 64
            && member
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            && is_public_route(path);
    }
    if !value.starts_with('/')
        || value.contains(['?', '#', '\\'])
        || value.contains("//")
        || value.ends_with('/')
    {
        return false;
    }
    let segments: Vec<String> = value
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(decode_public_route_segment)
        .collect::<Option<_>>()
        .unwrap_or_default();
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| matches!(segment.as_str(), "." | ".."))
    {
        return false;
    }
    COMPANION_API_ROUTES
        .iter()
        .chain(INSPECTION_GET_ROUTES.iter())
        .any(|route| {
            let route_segments = route.path.split('/').filter(|segment| !segment.is_empty());
            let mut actual_segments = segments.iter().map(String::as_str);
            let matched =
                route_segments
                    .zip(&mut actual_segments)
                    .all(|(route_segment, actual_segment)| {
                        if route_segment.starts_with('{') && route_segment.ends_with('}') {
                            is_safe_public_route_segment(actual_segment)
                        } else {
                            route_segment == actual_segment
                        }
                    });
            matched
                && route.path.matches('/').count() == value.matches('/').count()
                && actual_segments.next().is_none()
        })
}

fn decode_public_route_segment(segment: &str) -> Option<String> {
    let decoded = percent_decode_once(segment);
    if decoded.contains('%') || decoded.contains(['/', '\\']) {
        return None;
    }
    Some(decoded)
}

fn is_safe_public_route_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 512
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
}

fn normalize_leading_current_components(value: &str) -> &str {
    let mut normalized = value;
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped;
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recursively_redacts_portable_host_path_forms_and_preserves_other_values() {
        let value = json!({
            "safe": "keep this",
            "nested": [
                "/".to_string() + "Users/alice/private/token",
                "./C:\\Users\\alice\\secret.txt",
                "./~/.ssh/id_rsa",
                ".\\\\server\\share\\secret.txt",
                r"error=\\server\share\secret.txt",
                "./../secrets/token",
                "run://workflow/reports/result.md"
            ]
        });

        assert_eq!(
            sanitize_json(value),
            json!({
                "safe": "keep this",
                    "nested": [
                        "[path redacted]",
                        "[path redacted]",
                        "[path redacted]",
                        "[path redacted]",
                        "error=[path redacted]",
                        "[path redacted]",
                    "run:[path redacted]"
                ]
            })
        );
    }

    #[test]
    fn redacts_paths_embedded_in_diagnostics_without_losing_other_text() {
        assert_eq!(
            sanitize_string(&("failed reading /".to_string() + "Users/alice/state.json: invalid")),
            "failed reading [path redacted]: invalid"
        );
        assert_eq!(sanitize_string("ordinary text"), "ordinary text");
        assert_eq!(
            sanitize_string("/api/v1/runs/example/events"),
            "/api/v1/runs/example/events"
        );
        assert_eq!(
            sanitize_string("/inspect/runs/example"),
            "/inspect/runs/example"
        );
        assert_eq!(
            sanitize_string(&("/api/".to_string() + "Users/alice/secret.txt")),
            "[path redacted]"
        );
        assert_eq!(
            sanitize_string(&("/inspect/".to_string() + "Users/alice/secret.txt")),
            "[path redacted]"
        );
        assert_eq!(
            sanitize_string(&("/api/v1/runs/../".to_string() + "Users/alice/secret.txt")),
            "[path redacted]"
        );
        assert_eq!(
            sanitize_string(
                &("/api/v1/runs/example/unknown/".to_string() + "Users/alice/secret.txt")
            ),
            "[path redacted]"
        );
        assert_eq!(
            sanitize_string(&("/api/v1/runs//".to_string() + "Users/alice/secret.txt")),
            "[path redacted]"
        );
        assert_eq!(
            sanitize_string(
                &("/api/v1/runs/example?file=/".to_string() + "Users/alice/secret.txt")
            ),
            "[path redacted]"
        );
        for encoded in [
            "/api/v1/runs/%2FUsers%2Falice/steps",
            "/api/v1/runs/%5C%5Cserver%5Cshare/steps",
            "/api/v1/runs/example/%2e%2e/steps",
            "/api/v1/runs/example%2f..%2fUsers/steps",
        ] {
            assert_eq!(sanitize_string(encoded), "[path redacted]", "{encoded}");
        }
    }

    #[test]
    fn redacts_encoded_host_paths_without_decoding_safe_output() {
        let mut boundary_encoded = ("/".to_string() + "Users/alice/secret.txt").to_string();
        for _ in 0..=MAX_PERCENT_DECODE_LAYERS {
            boundary_encoded = boundary_encoded.replace('%', "%25").replace('/', "%2F");
        }
        for encoded in [
            "%2FUsers%2Falice%2Fsecret.txt",
            "%252FUsers%252Falice%252Fsecret.txt",
            "%25252FUsers%25252Falice%25252Fsecret.txt",
            "%7E%2F.ssh%2Fid_rsa",
            "%2E%2E%2Fsecret.txt",
            "%43%3A%5CUsers%5Calice%5Csecret.txt",
            "%5C%5Cserver%5Cshare%5Csecret.txt",
            "file=%2FUsers%2Falice%2Fsecret.txt",
            "file=%252E%252E%252Fsecret.txt",
            "file=%25252FUsers%25252Falice%25252Fsecret.txt",
            &boundary_encoded,
        ] {
            let expected = if encoded.starts_with("file=") {
                "file=[path redacted]"
            } else {
                "[path redacted]"
            };
            assert_eq!(sanitize_string(encoded), expected, "{encoded}");
        }
        assert_eq!(
            sanitize_string("value=hello%20world"),
            "value=hello%20world"
        );
    }

    #[test]
    fn sanitizes_path_shaped_object_keys_without_losing_colliding_values() {
        let value = serde_json::Map::from_iter([
            (
                "/".to_string() + "Users/alice/secret",
                serde_json::Value::from(1),
            ),
            (
                "C:\\Users\\alice\\secret".to_string(),
                serde_json::Value::from(2),
            ),
            ("[path redacted]".to_string(), serde_json::Value::from(3)),
            (
                "safe".to_string(),
                serde_json::json!({ "../private": true }),
            ),
        ]);
        let value = serde_json::Value::from(value);

        assert_eq!(
            sanitize_json(value),
            json!({
                "[path redacted]": 1,
                "[path redacted] #2": 2,
                "[path redacted] #3": 3,
                "safe": { "[path redacted]": true }
            })
        );
    }

    #[test]
    fn sanitizes_raw_workflow_text_without_rewriting_safe_content() {
        assert_eq!(
            String::from_utf8(
                sanitize_text(
                    ("workspace_root: /".to_string() + "Users/alice/project\nname: demo\n")
                        .into_bytes(),
                )
                .expect("valid text")
            )
            .expect("utf8"),
            "workspace_root: [path redacted]\nname: demo\n"
        );
    }

    #[test]
    fn rejects_invalid_text_without_returning_source_bytes() {
        assert_eq!(sanitize_text(vec![b'a', 0xff]), Err(()));
    }

    fn alice_path(suffix: &str) -> String {
        "/".to_string() + "Users/alice" + suffix
    }

    #[test]
    fn redacts_standalone_prefixed_paths_and_preserves_http_urls() {
        assert_eq!(
            sanitize_string(&format!("file:{}", alice_path("/secret.txt"))),
            "file:[path redacted]"
        );
        assert_eq!(
            sanitize_string("path:/etc/transist/config"),
            "path:[path redacted]"
        );
        assert_eq!(
            sanitize_string(&format!(
                "https://example.test{}",
                alice_path("/secret.txt")
            )),
            format!("https://example.test{}", alice_path("/secret.txt"))
        );
        assert_eq!(
            sanitize_string("http://example.test/etc/transist/config"),
            "http://example.test/etc/transist/config"
        );
    }

    #[test]
    fn preserves_http_url_authority_path_query_and_fragment_as_opaque_remote_identifier() {
        let input = format!(
            "https://example.test{}?file={}",
            alice_path("/private.txt"),
            alice_path("/private.txt")
        );
        assert_eq!(sanitize_string(&input), input);
        assert_eq!(
            sanitize_string("http://localhost:8080/etc/transist/config"),
            "http://localhost:8080/etc/transist/config"
        );
        assert_eq!(
            sanitize_string("https://example.test/path"),
            "https://example.test/path"
        );
        assert_eq!(
            sanitize_string("https://example.test"),
            "https://example.test"
        );
        assert_eq!(
            sanitize_string("https://example.test/"),
            "https://example.test/"
        );
        assert_eq!(
            sanitize_string("https://example.test/api/v1/runs/example"),
            "https://example.test/api/v1/runs/example"
        );
        assert_eq!(
            sanitize_string(&format!(
                "https://example.test/?file={} ",
                alice_path("/private.txt")
            )),
            format!("https://example.test/?file={} ", alice_path("/private.txt"))
        );
    }

    #[test]
    fn preserves_url_query_and_fragment_values() {
        let query_input = format!(
            "https://example.test?file={}&mode=keep",
            alice_path("/secret")
        );
        assert_eq!(sanitize_string(&query_input), query_input);
        let fragment_input = format!(
            "https://user:secret@example.test#{}&tab=keep",
            alice_path("/secret")
        );
        assert_eq!(sanitize_string(&fragment_input), fragment_input);
        assert_eq!(
            sanitize_string(
                "https://user@example.test?file=%2FUsers%2Falice&mode=keep#share=%5C%5Cserver%5Cprivate&tab=keep"
            ),
            "https://user@example.test?file=%2FUsers%2Falice&mode=keep#share=%5C%5Cserver%5Cprivate&tab=keep"
        );
        assert_eq!(
            sanitize_string("https://example.test\\\\server\\share\\secret.txt"),
            "https:[path redacted]"
        );
        assert_eq!(
            sanitize_string("https://example.test/etc"),
            "https://example.test/etc"
        );
    }

    #[test]
    fn preserves_safe_encoded_public_routes_and_url_punctuation() {
        assert_eq!(
            sanitize_string("https://example.test/api/v1/r%75ns/example%2D1/events"),
            "https://example.test/api/v1/r%75ns/example%2D1/events"
        );
        assert_eq!(
            sanitize_string("https://example.test/api/v1/runs/%2FUsers/events"),
            "https://example.test/api/v1/runs/%2FUsers/events"
        );
        assert_eq!(
            sanitize_string("https://example.test?file=%43%3A%5CUsers%5Calice&mode=keep)."),
            "https://example.test?file=%43%3A%5CUsers%5Calice&mode=keep)."
        );
    }

    #[test]
    fn preserves_safe_http_url_queries_fragments_duplicates_separators_and_punctuation() {
        let url = format!(
            "https://user:secret@example.test{}?keep=one&keep=ordinary;;flag#tab=one&tab=two,).",
            alice_path("")
        );
        assert_eq!(sanitize_string(&url), url);
    }

    #[test]
    fn malformed_bracketed_authorities_do_not_receive_url_preservation() {
        for value in [
            "https://[::1]invalid/../private",
            "https://[::1]extra:8080/../private",
            "https://[::1]x/../private",
        ] {
            assert!(!is_valid_http_url(value), "{value}");
            assert_eq!(sanitize_string(value), REDACTED_PATH, "{value}");
        }

        assert!(is_valid_http_url("https://[::1]:8080/private"));
        assert!(is_valid_http_url("https://[::1]/private"));
    }

    #[test]
    fn preserves_every_registered_route_when_sanitizing_standalone_paths() {
        for route in COMPANION_API_ROUTES
            .iter()
            .chain(INSPECTION_GET_ROUTES.iter())
        {
            let concrete = route
                .path
                .replace("{run_id}", "example")
                .replace("{step_id}", "step")
                .replace("{failure_id}", "failure")
                .replace("{artifact_id}", "artifact")
                .replace("{evidence_id}", "evidence")
                .replace("{workflow_file_id}", "wf")
                .replace("{workflow_ref}", "workflow@v1")
                .replace("{revision_id}", &"a".repeat(64))
                .replace("{file}", "inspect.css");
            assert_eq!(sanitize_string(&concrete), concrete, "{}", route.path);
        }
    }
}
