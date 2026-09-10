//! Peak/off-peak pricing window evaluation.
//!
//! A window is a 5-field cron expression (`min hour day month weekday`,
//! minute precision) marking the window start plus a duration in minutes.
//! The window is active at `now` when any minute in `[now - duration, now]`
//! matches the schedule, so windows spanning midnight need no special case.

use chrono::{DateTime, Datelike, Duration, Timelike, Weekday};

/// Upper bound on the minutes scanned for one window check.
const MAX_WINDOW_SCAN_MINUTES: u64 = 7 * 24 * 60;

/// A parsed 5-field cron schedule.
///
/// Fields accept numbers, `*`, ranges (`a-b`), steps (`*/n`, `a-b/n`) and
/// lists (`a,b-c/2`). Weekday accepts 0-6 with 7 as a Sunday alias. Month or
/// weekday names are not supported.
#[derive(Debug, Clone)]
pub struct CronSchedule {
    minutes: [bool; 60],
    hours: [bool; 24],
    days: [bool; 32],
    months: [bool; 13],
    weekdays: [bool; 7],
    day_restricted: bool,
    weekday_restricted: bool,
}

impl CronSchedule {
    /// Parses a 5-field cron expression.
    pub fn parse(expr: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!("expected 5 cron fields, found {}", fields.len()));
        }
        let minutes = parse_field(fields[0], 0, 59)?;
        let hours = parse_field(fields[1], 0, 23)?;
        let days = parse_field(fields[2], 1, 31)?;
        let months = parse_field(fields[3], 1, 12)?;
        let raw_weekdays: [bool; 8] = parse_field(fields[4], 0, 7)?;
        let mut weekdays = [false; 7];
        for (day, allowed) in raw_weekdays.iter().enumerate().take(8) {
            if *allowed {
                weekdays[day % 7] = true;
            }
        }
        Ok(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            day_restricted: fields[2] != "*",
            weekday_restricted: fields[4] != "*",
        })
    }

    /// Returns whether `dt` is a scheduled minute.
    ///
    /// Follows the standard cron day rule: when both day-of-month and
    /// day-of-week are restricted, either match suffices.
    pub fn matches<Tz: chrono::TimeZone>(&self, dt: &DateTime<Tz>) -> bool {
        if !self.minutes[dt.minute() as usize]
            || !self.hours[dt.hour() as usize]
            || !self.months[dt.month() as usize]
        {
            return false;
        }
        let weekday = match dt.weekday() {
            Weekday::Sun => 0,
            Weekday::Mon => 1,
            Weekday::Tue => 2,
            Weekday::Wed => 3,
            Weekday::Thu => 4,
            Weekday::Fri => 5,
            Weekday::Sat => 6,
        };
        match (self.day_restricted, self.weekday_restricted) {
            (false, false) => true,
            (true, false) => self.days[dt.day() as usize],
            (false, true) => self.weekdays[weekday],
            (true, true) => self.days[dt.day() as usize] || self.weekdays[weekday],
        }
    }
}

/// Parses one cron field into a lookup table over `[lo, hi]`.
fn parse_field<const N: usize>(field: &str, lo: u32, hi: u32) -> Result<[bool; N], String> {
    let mut table = [false; N];
    if field.is_empty() {
        return Err("empty cron field".to_string());
    }
    for item in field.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step.parse().map_err(|_| format!("invalid cron step '{step}'"))?;
                if step == 0 {
                    return Err("cron step must be positive".to_string());
                }
                (range, step)
            }
            None => (item, 1),
        };
        let (start, end) = if range == "*" {
            (lo, hi)
        } else if let Some((from, to)) = range.split_once('-') {
            let from: u32 = from.parse().map_err(|_| format!("invalid cron value '{from}'"))?;
            let to: u32 = to.parse().map_err(|_| format!("invalid cron value '{to}'"))?;
            (from, to)
        } else {
            let value: u32 = range.parse().map_err(|_| format!("invalid cron value '{range}'"))?;
            (value, value)
        };
        if start < lo || end > hi || start > end {
            return Err(format!("cron value '{item}' out of range {lo}-{hi}"));
        }
        let mut value = start;
        while value <= end {
            table[value as usize] = true;
            value += step;
        }
    }
    Ok(table)
}

/// Returns whether a window starting on `schedule` minutes and lasting
/// `duration_min` minutes covers `now`.
pub fn window_active<Tz: chrono::TimeZone>(
    schedule: &CronSchedule,
    duration_min: u64,
    now: &DateTime<Tz>,
) -> bool {
    let base = now.with_second(0).and_then(|dt| dt.with_nanosecond(0));
    let base = match base {
        Some(base) => base,
        None => return false,
    };
    let span = duration_min.min(MAX_WINDOW_SCAN_MINUTES);
    // Half-open window: active strictly before `start + duration`, so a
    // 360-minute window starting at 22:00 ends at 04:00.
    for back in 0..span {
        if schedule.matches(&(base.clone() - Duration::minutes(back as i64))) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::Tz;

    fn shanghai(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Tz> {
        chrono_tz::Asia::Shanghai
            .with_ymd_and_hms(year, month, day, hour, min, 0)
            .single()
            .expect("valid test time")
    }

    #[test]
    fn nightly_window_covers_evening_and_early_morning() {
        let schedule = CronSchedule::parse("0 22 * * *").unwrap();
        assert!(window_active(&schedule, 360, &shanghai(2026, 9, 9, 23, 30)));
        assert!(window_active(&schedule, 360, &shanghai(2026, 9, 10, 3, 0)));
        assert!(!window_active(&schedule, 360, &shanghai(2026, 9, 10, 5, 0)));
        assert!(!window_active(&schedule, 360, &shanghai(2026, 9, 9, 21, 0)));
    }

    #[test]
    fn weekday_restriction_is_honoured() {
        let schedule = CronSchedule::parse("0 9 * * 1").unwrap();
        // 2026-09-07 is a Monday.
        assert!(window_active(&schedule, 60, &shanghai(2026, 9, 7, 9, 30)));
        assert!(!window_active(&schedule, 60, &shanghai(2026, 9, 8, 9, 30)));
    }

    #[test]
    fn steps_lists_and_ranges_match() {
        let schedule = CronSchedule::parse("*/15 9-17 * * *").unwrap();
        assert!(schedule.matches(&shanghai(2026, 9, 9, 10, 30)));
        assert!(!schedule.matches(&shanghai(2026, 9, 9, 10, 31)));
        assert!(!schedule.matches(&shanghai(2026, 9, 9, 18, 0)));
    }

    #[test]
    fn malformed_expressions_are_rejected() {
        assert!(CronSchedule::parse("0 22 * *").is_err());
        assert!(CronSchedule::parse("0 25 * * *").is_err());
        assert!(CronSchedule::parse("*/0 * * * *").is_err());
        assert!(CronSchedule::parse("abc * * * *").is_err());
    }
}
