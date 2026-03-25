//! Natural language time parsing for heartbeat scheduler probe responses.
//!
//! Parses character-generated time expressions like "8:30 PM", "in 3 hours",
//! "tomorrow morning" into concrete `NaiveDateTime` values.

use chrono::{Duration, NaiveDateTime};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Result of parsing a time expression from a character's probe response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeParseResult {
    /// Character chose a specific time.
    Time(NaiveDateTime),
    /// Character declined to schedule (no parseable time found).
    Declined,
}

/// Parse a natural language time expression from a character's probe response.
///
/// Tries parsers in order of specificity:
/// 1. Relative: "in 3 hours", "in 30 minutes"
/// 2. Absolute 12h: "8:30 PM", "8 PM"
/// 3. Absolute 24h: "20:30"
/// 4. Named period: "tomorrow morning", "tonight"
///
/// Returns `TimeParseResult::Declined` if no valid time expression is found.
pub fn parse_time_expression(text: &str, now: NaiveDateTime) -> TimeParseResult {
    let lower = text.to_lowercase();

    if let Some(dt) = try_parse_relative(&lower, now) {
        return TimeParseResult::Time(dt);
    }
    if let Some(dt) = try_parse_absolute_ampm(&lower, now) {
        return TimeParseResult::Time(dt);
    }
    if let Some(dt) = try_parse_24h(&lower, now) {
        return TimeParseResult::Time(dt);
    }
    if let Some(dt) = try_parse_named_period(&lower, now) {
        return TimeParseResult::Time(dt);
    }

    TimeParseResult::Declined
}

// ---------------------------------------------------------------------------
// Relative time: "in N hours", "in N minutes"
// ---------------------------------------------------------------------------

fn try_parse_relative(text: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let words: Vec<&str> = text.split_whitespace().collect();

    for i in 0..words.len().saturating_sub(2) {
        if words[i] != "in" {
            continue;
        }

        let value: f64 = match words[i + 1].parse() {
            Ok(v) if v > 0.0 => v,
            _ => continue,
        };

        let unit = words[i + 2].trim_end_matches(|c: char| !c.is_alphabetic());

        if unit.starts_with("hour") {
            let mins = (value * 60.0) as i64;
            return Some(now + Duration::minutes(mins));
        }
        if unit.starts_with("min") {
            return Some(now + Duration::minutes(value as i64));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Absolute 12-hour: "8:30 PM", "8PM", "8 PM"
// ---------------------------------------------------------------------------

fn try_parse_absolute_ampm(text: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    for (marker, is_pm) in &[("pm", true), ("am", false)] {
        let mut search_from = 0;
        while let Some(rel_pos) = text[search_from..].find(marker) {
            let pos = search_from + rel_pos;
            search_from = pos + marker.len();

            // Boundary check: character after marker must be non-alphanumeric.
            if pos + marker.len() < text.len() {
                let next = text.as_bytes()[pos + marker.len()];
                if next.is_ascii_alphanumeric() {
                    continue;
                }
            }

            // Extract text before the marker, trimmed.
            let before = text[..pos].trim_end();
            if before.is_empty() {
                continue;
            }

            // Find the last contiguous chunk of digits/colons.
            let time_start = before
                .rfind(|c: char| !c.is_ascii_digit() && c != ':')
                .map_or(0, |p| p + 1);
            let time_str = &before[time_start..];

            if time_str.is_empty() || !time_str.chars().any(|c| c.is_ascii_digit()) {
                continue;
            }

            let (hour, minute) = match parse_hm(time_str) {
                Some(hm) => hm,
                None => continue,
            };
            let hour_24 = match to_24h(hour, *is_pm) {
                Some(h) => h,
                None => continue,
            };

            if hour_24 > 23 || minute > 59 {
                continue;
            }

            let target = now.date().and_hms_opt(hour_24, minute, 0)?;
            if target <= now {
                return Some(target + Duration::days(1));
            }
            return Some(target);
        }
    }
    None
}

/// Parse "H:MM" or "H" into (hour, minute).
fn parse_hm(s: &str) -> Option<(u32, u32)> {
    if let Some(colon) = s.find(':') {
        let h: u32 = s[..colon].parse().ok()?;
        let m: u32 = s[colon + 1..].parse().ok()?;
        Some((h, m))
    } else {
        let h: u32 = s.parse().ok()?;
        Some((h, 0))
    }
}

/// Convert 12-hour value to 24-hour.
fn to_24h(hour: u32, is_pm: bool) -> Option<u32> {
    if hour == 0 || hour > 12 {
        return None;
    }
    Some(match (hour, is_pm) {
        (12, false) => 0,  // 12 AM = midnight
        (12, true) => 12,  // 12 PM = noon
        (h, false) => h,   // AM
        (h, true) => h + 12, // PM
    })
}

// ---------------------------------------------------------------------------
// Absolute 24-hour: "20:30"
// ---------------------------------------------------------------------------

fn try_parse_24h(text: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    for (i, _) in text.match_indices(':') {
        // Extract hour digits before colon.
        let before = &text[..i];
        let hour_start = before
            .rfind(|c: char| !c.is_ascii_digit())
            .map_or(0, |p| p + 1);
        let hour_str = &before[hour_start..];
        if hour_str.is_empty() || hour_str.len() > 2 {
            continue;
        }

        // Extract minute digits after colon.
        let after = &text[i + 1..];
        let min_end = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        let min_str = &after[..min_end];
        if min_str.len() != 2 {
            continue;
        }

        // Skip if followed by am/pm (handled by the ampm parser).
        let rest = after[min_end..].trim_start();
        if rest.starts_with("am") || rest.starts_with("pm") {
            continue;
        }

        let hour: u32 = hour_str.parse().ok()?;
        let minute: u32 = min_str.parse().ok()?;

        if hour > 23 || minute > 59 {
            continue;
        }

        let target = now.date().and_hms_opt(hour, minute, 0)?;
        if target <= now {
            return Some(target + Duration::days(1));
        }
        return Some(target);
    }
    None
}

// ---------------------------------------------------------------------------
// Named periods: "tomorrow morning", "tonight", etc.
// ---------------------------------------------------------------------------

fn try_parse_named_period(text: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let tomorrow = now.date().succ_opt()?;

    if text.contains("tomorrow") {
        if text.contains("morning") {
            return tomorrow.and_hms_opt(9, 0, 0);
        }
        if text.contains("afternoon") {
            return tomorrow.and_hms_opt(14, 0, 0);
        }
        if text.contains("evening") || text.contains("night") {
            return tomorrow.and_hms_opt(19, 0, 0);
        }
        // Just "tomorrow" without period qualifier.
        return tomorrow.and_hms_opt(9, 0, 0);
    }

    if text.contains("tonight") {
        let target = now.date().and_hms_opt(21, 0, 0)?;
        if target <= now {
            return Some(target + Duration::days(1));
        }
        return Some(target);
    }

    if text.contains("this") && text.contains("evening") {
        let target = now.date().and_hms_opt(19, 0, 0)?;
        if target <= now {
            return Some(target + Duration::days(1));
        }
        return Some(target);
    }

    if text.contains("this") && text.contains("morning") {
        let target = now.date().and_hms_opt(9, 0, 0)?;
        if target <= now {
            return Some(target + Duration::days(1));
        }
        return Some(target);
    }

    if text.contains("this") && text.contains("afternoon") {
        let target = now.date().and_hms_opt(14, 0, 0)?;
        if target <= now {
            return Some(target + Duration::days(1));
        }
        return Some(target);
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, min, sec)
            .unwrap()
    }

    // -- relative time --------------------------------------------------------

    #[test]
    fn test_relative_hours() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("in 3 hours", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 17, 0, 0)));
    }

    #[test]
    fn test_relative_minutes() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("in 30 minutes", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 14, 30, 0)));
    }

    #[test]
    fn test_relative_fractional_hours() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("in 2.5 hours", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 16, 30, 0)));
    }

    #[test]
    fn test_relative_one_hour() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("in 1 hour", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 15, 0, 0)));
    }

    #[test]
    fn test_relative_embedded_in_sentence() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("I'll reach out in 2 hours, sounds good", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 16, 0, 0)));
    }

    // -- absolute 12h ---------------------------------------------------------

    #[test]
    fn test_ampm_830pm() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("8:30 PM", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 20, 30, 0)));
    }

    #[test]
    fn test_ampm_830pm_lowercase() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("8:30 pm", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 20, 30, 0)));
    }

    #[test]
    fn test_ampm_no_space() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("8:30PM", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 20, 30, 0)));
    }

    #[test]
    fn test_ampm_hour_only() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("8 PM", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 20, 0, 0)));
    }

    #[test]
    fn test_ampm_noon() {
        let now = dt(2026, 3, 25, 10, 0, 0);
        let result = parse_time_expression("12 PM", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 12, 0, 0)));
    }

    #[test]
    fn test_ampm_midnight() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("12 AM", now);
        // 12 AM is midnight — already past 22:00, so wraps to tomorrow.
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 0, 0, 0)));
    }

    #[test]
    fn test_ampm_past_time_wraps_to_tomorrow() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("8:30 PM", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 20, 30, 0)));
    }

    #[test]
    fn test_ampm_embedded_in_sentence() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("I'll reach out at 8:30 PM if that works", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 20, 30, 0)));
    }

    #[test]
    fn test_ampm_does_not_match_word_containing_am() {
        // "I am thinking" should not match "am" as a time marker.
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("I am thinking about it", now);
        assert_eq!(result, TimeParseResult::Declined);
    }

    // -- absolute 24h ---------------------------------------------------------

    #[test]
    fn test_24h_time() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("20:30", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 20, 30, 0)));
    }

    #[test]
    fn test_24h_past_wraps() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("14:00", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 14, 0, 0)));
    }

    // -- named periods --------------------------------------------------------

    #[test]
    fn test_tomorrow_morning() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("tomorrow morning", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 9, 0, 0)));
    }

    #[test]
    fn test_tomorrow_afternoon() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("tomorrow afternoon", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 14, 0, 0)));
    }

    #[test]
    fn test_tomorrow_evening() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("tomorrow evening", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 19, 0, 0)));
    }

    #[test]
    fn test_tomorrow_bare() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("maybe tomorrow", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 9, 0, 0)));
    }

    #[test]
    fn test_tonight() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("tonight", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 21, 0, 0)));
    }

    #[test]
    fn test_tonight_past_wraps() {
        let now = dt(2026, 3, 25, 22, 0, 0);
        let result = parse_time_expression("tonight", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 26, 21, 0, 0)));
    }

    #[test]
    fn test_this_evening() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("this evening", now);
        assert_eq!(result, TimeParseResult::Time(dt(2026, 3, 25, 19, 0, 0)));
    }

    // -- decline --------------------------------------------------------------

    #[test]
    fn test_decline_no_time() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("No, I don't think I'll reach out right now", now);
        assert_eq!(result, TimeParseResult::Declined);
    }

    #[test]
    fn test_decline_empty() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("", now);
        assert_eq!(result, TimeParseResult::Declined);
    }

    #[test]
    fn test_decline_vague() {
        let now = dt(2026, 3, 25, 14, 0, 0);
        let result = parse_time_expression("Maybe later, not sure", now);
        assert_eq!(result, TimeParseResult::Declined);
    }
}
