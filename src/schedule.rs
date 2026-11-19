//! Cron scheduling.
//!
//! Sorta does not run a daemon. A schedule is a cron expression that
//! `sorta scan --if-due` evaluates when the OS scheduler (systemd timer, cron,
//! Task Scheduler) calls it. That keeps the binary stateless, keeps the "one
//! process, no service" promise, and means a missed run is just a missed tick
//! rather than a broken daemon.
//!
//! Supported syntax is the five-field subset people actually write:
//! `*`, `a`, `a-b`, `*/n`, `a-b/n`, and comma lists of those.

use chrono::{DateTime, Datelike, Duration, Local, Timelike};

/// A parsed five-field cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    /// The expression as written, for display.
    pub expression: String,
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Vec<u32>,
    months: Vec<u32>,
    weekdays: Vec<u32>,
}

/// How far ahead [`Cron::next_after`] will search before giving up.
const SEARCH_LIMIT_MINUTES: u32 = 366 * 24 * 60;

impl Cron {
    /// Parses `minute hour day-of-month month day-of-week`.
    pub fn parse(expression: &str) -> anyhow::Result<Self> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        if fields.len() != 5 {
            anyhow::bail!(
                "expected 5 cron fields (minute hour day month weekday), got {} in '{expression}'",
                fields.len()
            );
        }

        Ok(Self {
            expression: expression.to_string(),
            minutes: parse_field(fields[0], 0, 59)?,
            hours: parse_field(fields[1], 0, 23)?,
            days: parse_field(fields[2], 1, 31)?,
            months: parse_field(fields[3], 1, 12)?,
            weekdays: parse_field(fields[4], 0, 6)?,
        })
    }

    /// True when this expression fires at exactly `at`.
    pub fn matches(&self, at: DateTime<Local>) -> bool {
        self.minutes.contains(&at.minute())
            && self.hours.contains(&at.hour())
            && self.days.contains(&at.day())
            && self.months.contains(&at.month())
            && self.weekdays.contains(&at.weekday().num_days_from_sunday())
    }

    /// The first firing time strictly after `from`.
    ///
    /// Searches minute by minute for up to a year. A expression that cannot fire
    /// (e.g. `0 0 31 2 *`) returns `None` rather than looping.
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        let mut cursor = from
            .with_second(0)
            .and_then(|d| d.with_nanosecond(0))
            .unwrap_or(from)
            + Duration::minutes(1);

        for _ in 0..SEARCH_LIMIT_MINUTES {
            if self.matches(cursor) {
                return Some(cursor);
            }
            cursor += Duration::minutes(1);
        }
        None
    }

    /// The next `count` firing times, for `sorta schedule`.
    pub fn upcoming(&self, from: DateTime<Local>, count: usize) -> Vec<DateTime<Local>> {
        let mut out = Vec::new();
        let mut cursor = from;
        for _ in 0..count {
            match self.next_after(cursor) {
                Some(next) => {
                    out.push(next);
                    cursor = next;
                }
                None => break,
            }
        }
        out
    }
}

fn parse_field(field: &str, min: u32, max: u32) -> anyhow::Result<Vec<u32>> {
    let mut values: Vec<u32> = Vec::new();

    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| anyhow::anyhow!("'{part}' has a non-numeric step"))?;
                if step == 0 {
                    anyhow::bail!("'{part}' has a zero step");
                }
                (range, step)
            }
            None => (part, 1),
        };

        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                a.parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("'{part}' has a non-numeric start"))?,
                b.parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("'{part}' has a non-numeric end"))?,
            )
        } else {
            let value: u32 = range
                .parse()
                .map_err(|_| anyhow::anyhow!("'{part}' is not a number"))?;
            if part.contains('/') {
                (value, max)
            } else {
                (value, value)
            }
        };

        if start < min || end > max || start > end {
            anyhow::bail!("'{part}' is outside {min}-{max}");
        }

        let mut value = start;
        while value <= end {
            values.push(value);
            value += step;
        }
    }

    values.sort_unstable();
    values.dedup();
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid local time")
    }

    #[test]
    fn parses_the_common_forms() {
        let cron = Cron::parse("0 18 * * *").unwrap();
        assert_eq!(cron.hours, vec![18]);
        assert_eq!(cron.minutes, vec![0]);
        assert_eq!(cron.days.len(), 31);

        let every_ten = Cron::parse("*/10 * * * *").unwrap();
        assert_eq!(every_ten.minutes, vec![0, 10, 20, 30, 40, 50]);

        let range = Cron::parse("0 9-17 * * 1-5").unwrap();
        assert_eq!(range.hours, vec![9, 10, 11, 12, 13, 14, 15, 16, 17]);
        assert_eq!(range.weekdays, vec![1, 2, 3, 4, 5]);

        let list = Cron::parse("0 0,12 * * *").unwrap();
        assert_eq!(list.hours, vec![0, 12]);
    }

    #[test]
    fn rejects_bad_expressions() {
        assert!(Cron::parse("0 18 * *").is_err());
        assert!(Cron::parse("99 * * * *").is_err());
        assert!(Cron::parse("*/0 * * * *").is_err());
        assert!(Cron::parse("5-1 * * * *").is_err());
        assert!(Cron::parse("abc * * * *").is_err());
    }

    #[test]
    fn matches_exactly() {
        let cron = Cron::parse("30 18 * * *").unwrap();
        assert!(cron.matches(at(2026, 11, 19, 18, 30)));
        assert!(!cron.matches(at(2026, 11, 19, 18, 31)));
        assert!(!cron.matches(at(2026, 11, 19, 17, 30)));
    }

    #[test]
    fn finds_the_next_run() {
        let cron = Cron::parse("0 18 * * *").unwrap();
        let next = cron
            .next_after(at(2026, 11, 19, 9, 15))
            .expect("18:00 today");
        assert_eq!(
            next.format("%Y-%m-%d %H:%M").to_string(),
            "2026-11-19 18:00"
        );

        let tomorrow = cron
            .next_after(at(2026, 11, 19, 19, 0))
            .expect("18:00 tomorrow");
        assert_eq!(
            tomorrow.format("%Y-%m-%d %H:%M").to_string(),
            "2026-11-20 18:00"
        );
    }

    #[test]
    fn impossible_expressions_return_none() {
        let cron = Cron::parse("0 0 31 2 *").unwrap();
        assert!(cron.next_after(at(2026, 1, 1, 0, 0)).is_none());
    }

    #[test]
    fn upcoming_lists_several_runs() {
        let cron = Cron::parse("0 9 * * 1-5").unwrap();
        let runs = cron.upcoming(at(2026, 11, 19, 10, 0), 3);
        assert_eq!(runs.len(), 3);
        // 19 Nov 2026 is a Thursday, so the next runs are Thu, Fri, Mon.
        assert_eq!(runs[0].format("%a %d").to_string(), "Thu 19");
        assert_eq!(runs[1].format("%a %d").to_string(), "Fri 20");
        assert_eq!(runs[2].format("%a %d").to_string(), "Mon 23");
    }
}
