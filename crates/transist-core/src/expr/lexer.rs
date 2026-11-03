// SPDX-License-Identifier: MPL-2.0
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenStream<'a> {
    pub(crate) source: &'a str,
}

pub(crate) fn tokenize(source: &str) -> Result<TokenStream<'_>, String> {
    if source.trim().is_empty() {
        return Err("expression is empty".to_string());
    }
    Ok(TokenStream { source })
}
