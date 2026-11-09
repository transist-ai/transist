// SPDX-License-Identifier: MPL-2.0
use super::ast::{EvaluationContext, Expression, Value};
use crate::types::{KernelStepState, KernelStepStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumericOp {
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
}

impl Expression {
    pub(crate) fn evaluate(&self, ctx: &EvaluationContext<'_>) -> Result<Value, String> {
        let Expression::Source(source) = self;
        match evaluate_expression(source, ctx) {
            Some(value) => Ok(Value::Bool(value)),
            None => Err("expression could not be evaluated".to_string()),
        }
    }
}

fn evaluate_expression(expr: &str, ctx: &EvaluationContext<'_>) -> Option<bool> {
    let trimmed = trim_wrapping_parens(expr.trim());
    if let Some(inner) = strip_not(trimmed) {
        return evaluate_expression(inner, ctx).map(|value| !value);
    }
    if let Some(parts) = split_logical(trimmed, "||") {
        return evaluate_any(&parts, ctx);
    }
    if let Some(parts) = split_logical(trimmed, "&&") {
        return evaluate_all(&parts, ctx);
    }
    if let Some((lhs, op, rhs)) = split_comparison(trimmed) {
        let lhs_value = parse_operand(lhs, ctx, true);
        let rhs_value = parse_operand(rhs, ctx, true);
        return compare_json_values(lhs_value?, op, rhs_value?);
    }

    let value = parse_operand(trimmed, ctx, false)?;
    Some(truthy(&value))
}

fn evaluate_any(parts: &[&str], ctx: &EvaluationContext<'_>) -> Option<bool> {
    let mut saw_unknown = false;
    for part in parts {
        match evaluate_expression(part, ctx) {
            Some(true) => return Some(true),
            Some(false) => {}
            None => saw_unknown = true,
        }
    }
    if saw_unknown { None } else { Some(false) }
}

fn evaluate_all(parts: &[&str], ctx: &EvaluationContext<'_>) -> Option<bool> {
    let mut saw_unknown = false;
    for part in parts {
        match evaluate_expression(part, ctx) {
            Some(false) => return Some(false),
            Some(true) => {}
            None => saw_unknown = true,
        }
    }
    if saw_unknown { None } else { Some(true) }
}

fn trim_wrapping_parens(expr: &str) -> &str {
    let mut trimmed = expr.trim();
    while let Some(inner) = trim_one_wrapping_paren_pair(trimmed) {
        trimmed = inner.trim();
    }
    trimmed
}

fn trim_one_wrapping_paren_pair(expr: &str) -> Option<&str> {
    let raw = expr.trim();
    if !(raw.starts_with('(') && raw.ends_with(')')) {
        return None;
    }
    let mut depth = 0_i32;
    let mut in_quote = false;
    let mut quote_char = '\0';
    for (idx, ch) in raw.char_indices() {
        if in_quote {
            if ch == quote_char {
                in_quote = false;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                in_quote = true;
                quote_char = ch;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && idx != raw.len() - 1 {
                    return None;
                }
            }
            _ => {}
        }
    }
    if depth == 0 {
        Some(&raw[1..raw.len() - 1])
    } else {
        None
    }
}

fn split_logical<'a>(expr: &'a str, token: &str) -> Option<Vec<&'a str>> {
    let mut depth = 0_i32;
    let mut in_quote = false;
    let mut quote_char = '\0';
    let mut idx = 0_usize;
    let mut start = 0_usize;
    let mut parts = Vec::new();

    while idx < expr.len() {
        let ch = expr[idx..].chars().next()?;
        let ch_len = ch.len_utf8();
        if in_quote {
            if ch == quote_char {
                in_quote = false;
            }
            idx += ch_len;
            continue;
        }
        match ch {
            '"' | '\'' => {
                in_quote = true;
                quote_char = ch;
            }
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if depth == 0 && expr[idx..].starts_with(token) => {
                let part = expr[start..idx].trim();
                if part.is_empty() {
                    return None;
                }
                parts.push(part);
                idx += token.len();
                start = idx;
                continue;
            }
            _ => {}
        }
        idx += ch_len;
    }

    if parts.is_empty() {
        return None;
    }
    let tail = expr[start..].trim();
    if tail.is_empty() {
        return None;
    }
    parts.push(tail);
    Some(parts)
}

fn strip_not(expr: &str) -> Option<&str> {
    let rest = expr.strip_prefix("NOT")?.trim_start();
    if rest.is_empty() {
        None
    } else {
        Some(trim_wrapping_parens(rest))
    }
}

fn split_comparison(expr: &str) -> Option<(&str, NumericOp, &str)> {
    let operators = [
        ("<=", NumericOp::Le),
        (">=", NumericOp::Ge),
        ("==", NumericOp::Eq),
        ("!=", NumericOp::Ne),
        ("<", NumericOp::Lt),
        (">", NumericOp::Gt),
    ];
    let mut depth = 0_i32;
    let mut in_quote = false;
    let mut quote_char = '\0';
    let bytes = expr.as_bytes();
    let mut idx = 0_usize;
    while idx < bytes.len() {
        let ch = expr[idx..].chars().next()?;
        let ch_len = ch.len_utf8();
        if in_quote {
            if ch == quote_char {
                in_quote = false;
            }
            idx += ch_len;
            continue;
        }
        match ch {
            '"' | '\'' => {
                in_quote = true;
                quote_char = ch;
            }
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if depth == 0 => {
                for (token, op) in operators {
                    if expr[idx..].starts_with(token) {
                        let lhs = expr[..idx].trim();
                        let rhs = expr[idx + token.len()..].trim();
                        if !lhs.is_empty() && !rhs.is_empty() {
                            return Some((lhs, op, rhs));
                        }
                    }
                }
            }
            _ => {}
        }
        idx += ch_len;
    }
    None
}

fn parse_operand(
    raw: &str,
    ctx: &EvaluationContext<'_>,
    comparison_operand: bool,
) -> Option<Value> {
    let trimmed = trim_wrapping_parens(raw.trim());
    if trimmed.is_empty() {
        return None;
    }
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        return Some(Value::String(trimmed[1..trimmed.len() - 1].to_string()));
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return Some(Value::Bool(true));
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Some(Value::Bool(false));
    }
    if trimmed.eq_ignore_ascii_case("null") {
        return Some(Value::Null);
    }
    if let Ok(number) = trimmed.parse::<i64>() {
        return Some(Value::Number(number.into()));
    }
    if let Ok(number) = trimmed.parse::<f64>() {
        if let Some(number) = serde_json::Number::from_f64(number) {
            return Some(Value::Number(number));
        }
    }
    if trimmed
        .strip_prefix("inputs.")
        .and_then(split_first_segment)
        .is_some_and(|(name, _)| {
            ctx.deferred_inputs
                .is_some_and(|names| names.contains(name))
        })
    {
        return None;
    }
    if ctx.strict_operands
        && (trimmed.starts_with("inputs.")
            || trimmed.starts_with("vars.")
            || trimmed.starts_with("steps.")
            || trimmed.starts_with("out.")
            || matches!(trimmed, "out" | "executions" | "status" | "result.status"))
    {
        return resolve_reference(trimmed, ctx);
    }
    if let Some(value) = resolve_reference(trimmed, ctx) {
        return Some(value);
    }
    // The legacy fallback also accepted incomplete syntax as a truthy string.
    // Marked loop guards keep unquoted literal operands, but syntax fragments
    // cannot become values and authorize either branch.
    if ctx.strict_operands
        && (!comparison_operand
            || trimmed.contains(['(', ')', '&', '|', '=', '<', '>', '!', '\'', '"']))
    {
        return None;
    }
    Some(Value::String(trimmed.to_string()))
}

fn resolve_reference(raw: &str, ctx: &EvaluationContext<'_>) -> Option<Value> {
    if let Some(path) = raw.strip_prefix("inputs.") {
        let (first, rest) = split_first_segment(path)?;
        let root = ctx
            .workflow_inputs
            .and_then(|inputs| inputs.get(first))
            .map(|value| Value::from(value.clone()))?;
        return resolve_nested_json(&root, rest);
    }

    if let Some(path) = raw.strip_prefix("vars.") {
        let (first, rest) = split_first_segment(path)?;
        let root = ctx
            .workflow_vars
            .and_then(|vars| vars.get(first))
            .map(|value| Value::from(value.clone()))?;
        return resolve_nested_json(&root, rest);
    }

    if let Some(path) = raw.strip_prefix("steps.") {
        let (step_id, rest) = split_first_segment(path)?;
        let step = ctx.snapshot.and_then(|value| value.steps.get(step_id))?;
        return resolve_step_reference(step, rest);
    }

    let step = ctx.dependency_state?;
    match raw {
        "executions" => Some(Value::Number(step.activation_executions().into())),
        "status" => Some(Value::String(step_status_name(step.status).to_string())),
        "result.status" => Some(Value::String(
            step.last_result
                .as_ref()
                .map(|result| result.status.clone())
                .unwrap_or_else(|| "unknown".to_string()),
        )),
        "out" => step
            .last_result
            .as_ref()
            .map(|result| result.output.clone()),
        _ if raw.starts_with("out.") => {
            let root = step
                .last_result
                .as_ref()
                .map(|result| result.output.clone())?;
            resolve_nested_json(&root, Some(&raw["out.".len()..]))
        }
        _ => None,
    }
}

fn resolve_step_reference(step: &KernelStepState, rest: Option<&str>) -> Option<Value> {
    match rest {
        None => Some(step_json(step)),
        Some("executions") => Some(Value::Number(step.activation_executions().into())),
        Some("status") => Some(Value::String(step_status_name(step.status).to_string())),
        Some("result.status") => Some(Value::String(
            step.last_result
                .as_ref()
                .map(|result| result.status.clone())
                .unwrap_or_else(|| "unknown".to_string()),
        )),
        Some("out") => step
            .last_result
            .as_ref()
            .map(|result| result.output.clone()),
        Some(path) if path.starts_with("out.") => {
            let root = step
                .last_result
                .as_ref()
                .map(|result| result.output.clone())?;
            resolve_nested_json(&root, Some(&path["out.".len()..]))
        }
        Some(path) => resolve_nested_json(&step_json(step), Some(path)),
    }
}

fn step_json(step: &KernelStepState) -> Value {
    serde_json::json!({
        "status": step_status_name(step.status),
        "executions": step.activation_executions(),
        "result": {
            "status": step
                .last_result
                .as_ref()
                .map(|result| result.status.clone())
                .unwrap_or_else(|| "unknown".to_string()),
        },
        "out": step
            .last_result
            .as_ref()
            .map(|result| result.output.clone())
            .unwrap_or(Value::Null),
    })
}

fn step_status_name(status: KernelStepStatus) -> &'static str {
    match status {
        KernelStepStatus::Pending => "pending",
        KernelStepStatus::Runnable => "runnable",
        KernelStepStatus::Dispatching => "dispatching",
        KernelStepStatus::Running => "running",
        KernelStepStatus::Blocked => "blocked",
        KernelStepStatus::Succeeded => "succeeded",
        KernelStepStatus::Failed => "failed",
        KernelStepStatus::RetryWait => "retry_wait",
        KernelStepStatus::Skipped => "skipped",
        KernelStepStatus::Canceled => "canceled",
    }
}

fn compare_json_values(lhs: Value, op: NumericOp, rhs: Value) -> Option<bool> {
    match op {
        NumericOp::Eq => Some(lhs == rhs),
        NumericOp::Ne => Some(lhs != rhs),
        NumericOp::Lt | NumericOp::Le | NumericOp::Ge | NumericOp::Gt => {
            let lhs = lhs.as_f64()?;
            let rhs = rhs.as_f64()?;
            Some(match op {
                NumericOp::Lt => lhs < rhs,
                NumericOp::Le => lhs <= rhs,
                NumericOp::Ge => lhs >= rhs,
                NumericOp::Gt => lhs > rhs,
                NumericOp::Eq | NumericOp::Ne => return None,
            })
        }
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().map(|value| value != 0.0).unwrap_or(false),
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn split_first_segment(path: &str) -> Option<(&str, Option<&str>)> {
    if path.is_empty() {
        return None;
    }
    match path.split_once('.') {
        Some((head, tail)) if !head.is_empty() => Some((head, Some(tail))),
        Some(_) => None,
        None => Some((path, None)),
    }
}

fn resolve_nested_json(root: &Value, path: Option<&str>) -> Option<Value> {
    let Some(path) = path else {
        return Some(root.clone());
    };
    let mut current = root;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.get(segment)?;
    }
    Some(current.clone())
}
