// SPDX-License-Identifier: MPL-2.0
use serde_json::json;
use transist_core::Redactor;

#[test]
fn embedded_secret_substrings_are_redacted() {
    let redactor = Redactor::from_secrets(["secret"]);
    assert_eq!(redactor.redact_str("xsecretx"), "x[REDACTED]x");
    assert_eq!(
        redactor.redact_str("prefix_secret_suffix"),
        "prefix_[REDACTED]_suffix"
    );
    assert_eq!(redactor.redact_str("token=secret"), "token=[REDACTED]");
}

#[test]
fn json_escaped_variants_are_caught() {
    let redactor = Redactor::from_secrets(["api-key"]);
    let value = json!({
        "nested": ["keep", "token=api-key"],
        "flat": "api-key"
    });

    let redacted = redactor.redact_json(value);
    assert_eq!(redacted["flat"], "[REDACTED]");
    assert_eq!(redacted["nested"][1], "token=[REDACTED]");
}

#[test]
fn overlapping_secrets_are_deterministic() {
    let redactor = Redactor::from_secrets(["abc", "abc123", "abc123"]);
    assert_eq!(redactor.redact_str("abc123 abc"), "[REDACTED] [REDACTED]");
}

#[test]
fn empty_secret_list_is_noop() {
    let redactor = Redactor::from_secrets(std::iter::empty::<&str>());
    assert_eq!(
        redactor.redact_str("nothing to redact"),
        "nothing to redact"
    );
}

#[test]
fn base64_variants_are_caught() {
    let redactor = Redactor::from_secrets(["topsecret"]);
    assert_eq!(redactor.redact_str("dG9wc2VjcmV0"), "[REDACTED]");
}

#[test]
fn padded_and_url_safe_base64_variants_are_caught() {
    let redactor = Redactor::from_secrets(["a"]);
    assert!(redactor.contains_secret("YQ=="));
    assert!(redactor.contains_secret("YQ"));
    assert_eq!(
        redactor.redact_str("prefix=YQ==/YQ.suffix"),
        "prefix=[REDACTED]/[REDACTED].suffix"
    );
}
