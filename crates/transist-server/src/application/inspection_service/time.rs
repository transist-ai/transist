// SPDX-License-Identifier: MPL-2.0
use super::service::ApiProblem;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rfc3339ParseError {
    Invalid,
    BeforeUnixEpoch,
}

pub(crate) fn parse_rfc3339_to_ms(raw: &str) -> Result<u64, ApiProblem> {
    match parse_rfc3339_timestamp_ms(raw) {
        Ok(value) => Ok(value),
        Err(Rfc3339ParseError::Invalid) => Err(ApiProblem::invalid_request(format!(
            "invalid RFC3339 timestamp '{raw}'"
        ))),
        Err(Rfc3339ParseError::BeforeUnixEpoch) => Err(ApiProblem::invalid_request(format!(
            "timestamp '{raw}' is before the unix epoch"
        ))),
    }
}

pub(crate) fn format_timestamp(timestamp_ms: Option<u64>) -> Option<String> {
    let timestamp_ms = timestamp_ms?;
    format_rfc3339_timestamp_ms(timestamp_ms)
}

pub(crate) fn parse_rfc3339_timestamp_ms(raw: &str) -> Result<u64, Rfc3339ParseError> {
    let (date, time_and_offset) = raw.split_once('T').ok_or(Rfc3339ParseError::Invalid)?;
    let (year, month, day) = parse_rfc3339_date(date)?;
    let (time, offset_seconds) = parse_time_and_offset(time_and_offset)?;
    let (hour, minute, second, millis) = parse_rfc3339_time(time)?;
    let days = days_from_civil(year, month, day);
    let local_seconds =
        (days as i128) * 86_400 + (hour as i128) * 3_600 + (minute as i128) * 60 + second as i128;
    let utc_seconds = local_seconds - offset_seconds as i128;
    if utc_seconds < 0 {
        return Err(Rfc3339ParseError::BeforeUnixEpoch);
    }
    let timestamp_ms = utc_seconds
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(millis as i128))
        .ok_or(Rfc3339ParseError::Invalid)?;
    u64::try_from(timestamp_ms).map_err(|_| Rfc3339ParseError::Invalid)
}

pub(crate) fn format_rfc3339_timestamp_ms(timestamp_ms: u64) -> Option<String> {
    let seconds = timestamp_ms / 1_000;
    if seconds > i64::MAX as u64 {
        return None;
    }
    let millis = (timestamp_ms % 1_000) as u32;
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = (seconds % 86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    let mut out = String::with_capacity(24);
    write!(
        &mut out,
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
    )
    .ok()?;
    if millis != 0 {
        write!(&mut out, ".{millis:03}").ok()?;
    }
    out.push('Z');
    Some(out)
}

fn parse_rfc3339_date(raw: &str) -> Result<(i32, u32, u32), Rfc3339ParseError> {
    if raw.len() != 10 || raw.as_bytes()[4] != b'-' || raw.as_bytes()[7] != b'-' {
        return Err(Rfc3339ParseError::Invalid);
    }
    let year = parse_digits::<i32>(&raw[0..4])?;
    let month = parse_digits::<u32>(&raw[5..7])?;
    let day = parse_digits::<u32>(&raw[8..10])?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return Err(Rfc3339ParseError::Invalid);
    }
    Ok((year, month, day))
}

fn parse_time_and_offset(raw: &str) -> Result<(&str, i32), Rfc3339ParseError> {
    if let Some(time) = raw.strip_suffix('Z') {
        return Ok((time, 0));
    }
    let split_at = raw
        .char_indices()
        .find_map(|(idx, ch)| (idx >= 8 && matches!(ch, '+' | '-')).then_some(idx))
        .ok_or(Rfc3339ParseError::Invalid)?;
    let time = &raw[..split_at];
    let offset = &raw[split_at..];
    Ok((time, parse_rfc3339_offset(offset)?))
}

fn parse_rfc3339_time(raw: &str) -> Result<(u32, u32, u32, u32), Rfc3339ParseError> {
    let (whole, fractional) = match raw.split_once('.') {
        Some((whole, fractional)) => (whole, Some(fractional)),
        None => (raw, None),
    };
    if whole.len() != 8 || whole.as_bytes()[2] != b':' || whole.as_bytes()[5] != b':' {
        return Err(Rfc3339ParseError::Invalid);
    }
    let hour = parse_digits::<u32>(&whole[0..2])?;
    let minute = parse_digits::<u32>(&whole[3..5])?;
    let second = parse_digits::<u32>(&whole[6..8])?;
    if hour > 23 || minute > 59 || second > 59 {
        return Err(Rfc3339ParseError::Invalid);
    }

    let millis = match fractional {
        Some(digits) => parse_fractional_millis(digits)?,
        None => 0,
    };
    Ok((hour, minute, second, millis))
}

fn parse_fractional_millis(raw: &str) -> Result<u32, Rfc3339ParseError> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Rfc3339ParseError::Invalid);
    }
    let mut millis = 0u32;
    for (idx, byte) in raw.bytes().take(3).enumerate() {
        millis += ((byte - b'0') as u32) * 10_u32.pow(2 - idx as u32);
    }
    Ok(millis)
}

fn parse_rfc3339_offset(raw: &str) -> Result<i32, Rfc3339ParseError> {
    if raw.len() != 6 || raw.as_bytes()[3] != b':' {
        return Err(Rfc3339ParseError::Invalid);
    }
    let sign = match raw.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return Err(Rfc3339ParseError::Invalid),
    };
    let hours = parse_digits::<i32>(&raw[1..3])?;
    let minutes = parse_digits::<i32>(&raw[4..6])?;
    if hours > 23 || minutes > 59 {
        return Err(Rfc3339ParseError::Invalid);
    }
    Ok(sign * (hours * 3_600 + minutes * 60))
}

fn parse_digits<T>(raw: &str) -> Result<T, Rfc3339ParseError>
where
    T: std::str::FromStr,
{
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Rfc3339ParseError::Invalid);
    }
    raw.parse().map_err(|_| Rfc3339ParseError::Invalid)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day = day as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) as i64
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}
