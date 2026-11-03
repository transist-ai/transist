// SPDX-License-Identifier: MPL-2.0
use anyhow::{Context, Result, bail};

pub(crate) fn parse_duration_to_ms(raw: &str) -> Result<u64> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        bail!("duration cannot be empty");
    }
    let split_idx = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .ok_or_else(|| anyhow::anyhow!("duration '{}' is missing unit", raw))?;
    if split_idx == 0 {
        bail!("duration '{}' must start with digits", raw);
    }
    let value = trimmed[..split_idx]
        .parse::<u64>()
        .with_context(|| format!("invalid duration value '{}'", raw))?;
    let unit = &trimmed[split_idx..];
    let multiplier = match unit {
        "ms" => 1u64,
        "s" => 1_000u64,
        "m" => 60_000u64,
        "h" => 3_600_000u64,
        "d" => 86_400_000u64,
        _ => bail!(
            "unsupported duration unit '{}' in '{}' (expected ms|s|m|h|d)",
            unit,
            raw
        ),
    };
    let duration_ms = value.checked_mul(multiplier).ok_or_else(|| {
        anyhow::anyhow!("duration '{}' overflows millisecond representation", raw)
    })?;
    if duration_ms == 0 {
        bail!("duration '{}' must be greater than 0", raw);
    }
    Ok(duration_ms)
}
