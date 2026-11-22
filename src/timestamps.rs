//! Timestamp decoding shared by the session readers.
//!
//! Every agent writes RFC 3339 timestamps in its own dialect: Codex and Claude
//! use millisecond fractions, Grok writes microseconds, and Gemini sometimes
//! omits the fraction entirely. The `time` crate needs extra features for that
//! range of shapes, so the fixed form these logs use is decoded here instead.

use std::time::{SystemTime, UNIX_EPOCH};
use time::{Date, Month, Time};

/// Milliseconds since the Unix epoch, for a file's modification time.
pub fn epoch_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse an RFC 3339 timestamp such as `2026-08-17T14:20:51.810Z`.
///
/// A fraction is a decimal fraction, not a digit count: `.8` is 800 ms while
/// `.810` is 810 ms, and digits past the third are truncated. An explicit
/// numeric offset is honoured; a missing one is treated as UTC.
pub fn parse_rfc3339_millis(value: &str) -> Option<i64> {
    if value.len() < 19 {
        return None;
    }
    let year: i32 = value.get(0..4)?.parse().ok()?;
    let month: u8 = value.get(5..7)?.parse().ok()?;
    let day: u8 = value.get(8..10)?.parse().ok()?;
    let hour: u8 = value.get(11..13)?.parse().ok()?;
    let minute: u8 = value.get(14..16)?.parse().ok()?;
    let second: u8 = value.get(17..19)?.parse().ok()?;
    let separators = [4, 7, 10, 13, 16].map(|index| value.as_bytes().get(index).copied());
    if separators[0] != Some(b'-')
        || separators[1] != Some(b'-')
        || !matches!(separators[2], Some(b'T' | b't' | b' '))
        || separators[3] != Some(b':')
        || separators[4] != Some(b':')
    {
        return None;
    }
    let rest = &value[19..];
    let (millis, rest) = match rest.strip_prefix('.') {
        Some(fraction) => {
            let digits = fraction
                .char_indices()
                .take_while(|(_, character)| character.is_ascii_digit())
                .last()
                .map(|(index, _)| index + 1)
                .unwrap_or(0);
            let millis = fraction_millis(&fraction[..digits])?;
            (millis, &fraction[digits..])
        }
        None => (0, rest),
    };
    let offset = match rest {
        "Z" | "z" | "" => 0,
        zone => {
            let sign = match zone.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hours: i64 = zone.get(1..3)?.parse().ok()?;
            let minutes: i64 = zone.get(4..6)?.parse().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    let seconds = date.with_time(time).assume_utc().unix_timestamp() - offset;
    Some(seconds * 1_000 + millis)
}

/// Milliseconds from a fractional-second digit run: `.8` is 800 ms, `.81` is
/// 810 ms, `.810` is 810 ms and anything past three digits is truncated.
fn fraction_millis(digits: &str) -> Option<i64> {
    if digits.is_empty() {
        return Some(0);
    }
    let mut millis = 0;
    for (index, character) in digits.chars().take(3).enumerate() {
        let digit = character.to_digit(10)? as i64;
        millis += digit * 10i64.pow(2 - index as u32);
    }
    Some(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_timestamps_the_logs_write() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_rfc3339_millis("2026-08-17T14:20:51.810Z"),
            Some(1_786_976_451_810)
        );
        // A fraction shorter than three digits is a decimal fraction, not
        // milliseconds: `.8` is 800 ms, not 8 ms.
        assert_eq!(
            parse_rfc3339_millis("2026-08-17T14:20:51.8Z"),
            Some(1_786_976_451_800)
        );
        // An explicit offset is honoured.
        assert_eq!(
            parse_rfc3339_millis("2026-08-17T22:20:51.810+08:00"),
            Some(1_786_976_451_810)
        );
        // Grok writes six fractional digits.
        assert_eq!(
            parse_rfc3339_millis("2026-06-19T11:31:12.282793Z"),
            Some(1_781_868_672_282)
        );
        assert_eq!(parse_rfc3339_millis("not a timestamp"), None);
    }
}
