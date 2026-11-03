// SPDX-License-Identifier: MPL-2.0
use super::ast::Expression;
use super::lexer::tokenize;

impl Expression {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        let tokens = tokenize(source)?;
        Ok(Expression::Source(tokens.source.trim().to_string()))
    }
}
