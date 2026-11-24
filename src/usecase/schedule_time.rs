//! Schedule options and recurrence maths (GOWA `validations/schedule_validation.go`:
//! `ParseScheduleOptions`, `NextScheduleOccurrence`).

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};

/// Optional fields of every `/send/*` body that schedule it instead of sending now.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ScheduleOptions {
    pub scheduled_at: String,
    pub timezone: String,
    pub recurrence: String,
    pub weekdays: Vec<i64>,
    pub day_of_month: i64,
    pub end_at: String,
    pub occurrence_limit: i64,
}

impl ScheduleOptions {
    pub fn is_scheduled(&self) -> bool {
        !self.scheduled_at.trim().is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct ScheduleSpec {
    pub scheduled_at: DateTime<Utc>,
    pub timezone: String,
    pub tz: Tz,
    pub recurrence: String,
    /// 0 = Sunday … 6 = Saturday.
    pub weekdays: Vec<u8>,
    pub day_of_month: u32,
    pub end_at: Option<DateTime<Utc>>,
    pub occurrence_limit: i64,
}

fn invalid(message: &str) -> ApiError {
    ApiError::validation(message)
}

/// `None` when the request is not scheduled.
pub fn parse_options(o: &ScheduleOptions, now: DateTime<Utc>) -> ApiResult<Option<ScheduleSpec>> {
    if !o.is_scheduled() {
        return Ok(None);
    }
    let zone = o.timezone.trim();
    if zone.is_empty() {
        return Err(invalid("timezone is required for scheduled sends"));
    }
    let tz: Tz = zone
        .parse()
        .map_err(|_| invalid("timezone must be a valid IANA timezone"))?;
    let scheduled_at = DateTime::parse_from_rfc3339(o.scheduled_at.trim())
        .map_err(|_| invalid("scheduled_at must be an RFC3339 timestamp"))?
        .with_timezone(&Utc);
    if scheduled_at <= now {
        return Err(invalid("scheduled_at must be in the future"));
    }
    let recurrence = match o.recurrence.trim().to_lowercase().as_str() {
        "" => "once".to_owned(),
        r @ ("once" | "daily" | "weekly" | "monthly") => r.to_owned(),
        _ => {
            return Err(invalid(
                "recurrence must be once, daily, weekly, or monthly",
            ));
        }
    };
    if recurrence == "once" && (!o.weekdays.is_empty() || o.day_of_month != 0) {
        return Err(invalid(
            "weekly and monthly recurrence fields require a recurring schedule",
        ));
    }
    let mut weekdays = Vec::new();
    if recurrence == "weekly" {
        if o.weekdays.is_empty() {
            return Err(invalid("weekdays are required for weekly recurrence"));
        }
        for day in &o.weekdays {
            if !(0..=6).contains(day) || weekdays.contains(&(*day as u8)) {
                return Err(invalid("weekdays must contain unique values from 0 to 6"));
            }
            weekdays.push(*day as u8);
        }
    } else {
        weekdays = o.weekdays.iter().map(|d| *d as u8).collect();
    }
    if recurrence == "monthly" && !(1..=31).contains(&o.day_of_month) {
        return Err(invalid(
            "day_of_month must be between 1 and 31 for monthly recurrence",
        ));
    }
    if recurrence != "monthly" && o.day_of_month != 0 {
        return Err(invalid("day_of_month requires monthly recurrence"));
    }
    let end_at = if o.end_at.trim().is_empty() {
        None
    } else {
        let parsed = DateTime::parse_from_rfc3339(o.end_at.trim())
            .map_err(|_| invalid("end_at must be an RFC3339 timestamp"))?
            .with_timezone(&Utc);
        if parsed <= scheduled_at {
            return Err(invalid("end_at must be after scheduled_at"));
        }
        Some(parsed)
    };
    if o.occurrence_limit < 0 {
        return Err(invalid("occurrence_limit cannot be negative"));
    }
    if recurrence == "once" && o.occurrence_limit > 1 {
        return Err(invalid(
            "occurrence_limit must be 1 or omitted for one-time sends",
        ));
    }
    Ok(Some(ScheduleSpec {
        scheduled_at,
        timezone: zone.to_owned(),
        tz,
        recurrence,
        weekdays,
        day_of_month: o.day_of_month as u32,
        end_at,
        occurrence_limit: o.occurrence_limit,
    }))
}

/// The wall clock `time` on `date` in `tz`. A wall time inside a DST gap does
/// not exist; it is pushed forward past the gap so each local day has at most
/// one send and never before its wall time. In an overlap the first (earlier)
/// instant wins.
fn occurrence_on(date: NaiveDate, time: NaiveTime, tz: Tz) -> DateTime<Utc> {
    let mut wall = date.and_time(time);
    loop {
        if let Some(dt) = tz.from_local_datetime(&wall).earliest() {
            return dt.with_timezone(&Utc);
        }
        wall += Duration::hours(1);
    }
}

/// First slot strictly after `after`, within `end_at`; `None` for one-time
/// schedules or when the series is over.
pub fn next_occurrence(spec: &ScheduleSpec, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if spec.recurrence == "once" {
        return None;
    }
    let local = after.with_timezone(&spec.tz);
    let time = spec.scheduled_at.with_timezone(&spec.tz).time();
    let today = local.date_naive();
    let candidate = match spec.recurrence.as_str() {
        "daily" => (0..=2)
            .map(|off| occurrence_on(today + Duration::days(off), time, spec.tz))
            .find(|c| *c > after),
        "weekly" => (0..=7).find_map(|off| {
            let day = today + Duration::days(off);
            let weekday = day.weekday().num_days_from_sunday() as u8;
            if !spec.weekdays.contains(&weekday) {
                return None;
            }
            Some(occurrence_on(day, time, spec.tz)).filter(|c| *c > after)
        }),
        "monthly" => (0..=24).find_map(|off| {
            let months = today.year() * 12 + today.month0() as i32 + off;
            let (year, month) = (months.div_euclid(12), months.rem_euclid(12) as u32 + 1);
            let last = last_day(year, month);
            let day = spec.day_of_month.min(last);
            let date = NaiveDate::from_ymd_opt(year, month, day)?;
            Some(occurrence_on(date, time, spec.tz)).filter(|c| *c > after)
        }),
        _ => None,
    }?;
    match spec.end_at {
        Some(end) if candidate > end => None,
        _ => Some(candidate),
    }
}

fn last_day(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    (NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid month") - Duration::days(1)).day()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn opts(at: &str, tz: &str, recurrence: &str) -> ScheduleOptions {
        ScheduleOptions {
            scheduled_at: at.into(),
            timezone: tz.into(),
            recurrence: recurrence.into(),
            ..Default::default()
        }
    }

    fn spec(o: ScheduleOptions) -> ScheduleSpec {
        parse_options(&o, utc("2026-01-01T00:00:00Z"))
            .unwrap()
            .unwrap()
    }

    fn err(o: ScheduleOptions) -> String {
        parse_options(&o, utc("2026-01-01T00:00:00Z"))
            .unwrap_err()
            .message
    }

    #[test]
    fn unscheduled_is_none() {
        assert!(
            parse_options(&ScheduleOptions::default(), Utc::now())
                .unwrap()
                .is_none()
        );
        assert!(!opts("  ", "", "").is_scheduled());
    }

    #[test]
    fn option_validation_messages() {
        let at = "2026-10-01T09:00:00+07:00";
        assert_eq!(
            err(opts(at, "", "")),
            "timezone is required for scheduled sends"
        );
        assert_eq!(
            err(opts(at, "Mars/Base", "")),
            "timezone must be a valid IANA timezone"
        );
        assert_eq!(
            err(opts("tomorrow", "Asia/Jakarta", "")),
            "scheduled_at must be an RFC3339 timestamp"
        );
        assert_eq!(
            err(opts("2025-01-01T00:00:00Z", "Asia/Jakarta", "")),
            "scheduled_at must be in the future"
        );
        assert_eq!(
            err(opts(at, "Asia/Jakarta", "hourly")),
            "recurrence must be once, daily, weekly, or monthly"
        );
        let mut o = opts(at, "Asia/Jakarta", "");
        o.weekdays = vec![1];
        assert_eq!(
            err(o),
            "weekly and monthly recurrence fields require a recurring schedule"
        );
        assert_eq!(
            err(opts(at, "Asia/Jakarta", "weekly")),
            "weekdays are required for weekly recurrence"
        );
        let mut o = opts(at, "Asia/Jakarta", "weekly");
        o.weekdays = vec![1, 1];
        assert_eq!(err(o), "weekdays must contain unique values from 0 to 6");
        let mut o = opts(at, "Asia/Jakarta", "weekly");
        o.weekdays = vec![7];
        assert_eq!(err(o), "weekdays must contain unique values from 0 to 6");
        assert_eq!(
            err(opts(at, "Asia/Jakarta", "monthly")),
            "day_of_month must be between 1 and 31 for monthly recurrence"
        );
        let mut o = opts(at, "Asia/Jakarta", "daily");
        o.day_of_month = 5;
        assert_eq!(err(o), "day_of_month requires monthly recurrence");
        let mut o = opts(at, "Asia/Jakarta", "daily");
        o.end_at = "2026-10-01T09:00:00+07:00".into();
        assert_eq!(err(o), "end_at must be after scheduled_at");
        let mut o = opts(at, "Asia/Jakarta", "daily");
        o.end_at = "x".into();
        assert_eq!(err(o), "end_at must be an RFC3339 timestamp");
        let mut o = opts(at, "Asia/Jakarta", "daily");
        o.occurrence_limit = -1;
        assert_eq!(err(o), "occurrence_limit cannot be negative");
        let mut o = opts(at, "Asia/Jakarta", "once");
        o.occurrence_limit = 2;
        assert_eq!(
            err(o),
            "occurrence_limit must be 1 or omitted for one-time sends"
        );
        // Recurrence is case-insensitive; empty means once.
        assert_eq!(spec(opts(at, "Asia/Jakarta", "DAILY")).recurrence, "daily");
        assert_eq!(spec(opts(at, "Asia/Jakarta", "")).recurrence, "once");
    }

    #[test]
    fn once_has_no_next() {
        let s = spec(opts("2026-10-01T09:00:00+07:00", "Asia/Jakarta", "once"));
        assert_eq!(s.scheduled_at, utc("2026-10-01T02:00:00Z"));
        assert!(next_occurrence(&s, utc("2026-10-01T02:00:00Z")).is_none());
    }

    #[test]
    fn daily_keeps_the_wall_clock() {
        let s = spec(opts("2026-10-01T09:00:00+07:00", "Asia/Jakarta", "daily"));
        assert_eq!(
            next_occurrence(&s, utc("2026-10-01T02:00:00Z")),
            Some(utc("2026-10-02T02:00:00Z"))
        );
        // Before today's slot: today.
        assert_eq!(
            next_occurrence(&s, utc("2026-10-05T01:00:00Z")),
            Some(utc("2026-10-05T02:00:00Z"))
        );
        // Strictly after.
        assert_eq!(
            next_occurrence(&s, utc("2026-10-05T02:00:00Z")),
            Some(utc("2026-10-06T02:00:00Z"))
        );
    }

    #[test]
    fn weekly_picks_the_next_listed_weekday() {
        let mut o = opts("2026-10-01T09:00:00+07:00", "Asia/Jakarta", "weekly");
        o.weekdays = vec![1, 3, 5]; // Mon, Wed, Fri
        let s = spec(o);
        // 2026-10-01 is a Thursday → next is Friday 2026-10-02.
        assert_eq!(
            next_occurrence(&s, utc("2026-10-01T02:00:00Z")),
            Some(utc("2026-10-02T02:00:00Z"))
        );
        // After Friday → Monday 2026-10-05.
        assert_eq!(
            next_occurrence(&s, utc("2026-10-02T02:00:00Z")),
            Some(utc("2026-10-05T02:00:00Z"))
        );
    }

    #[test]
    fn monthly_clamps_to_the_last_day() {
        let mut o = opts("2026-01-31T09:00:00+07:00", "Asia/Jakarta", "monthly");
        o.day_of_month = 31;
        let mut s = spec(o);
        s.scheduled_at = utc("2026-01-31T02:00:00Z");
        assert_eq!(
            next_occurrence(&s, utc("2026-01-31T02:00:00Z")),
            Some(utc("2026-02-28T02:00:00Z"))
        );
        assert_eq!(
            next_occurrence(&s, utc("2026-02-28T02:00:00Z")),
            Some(utc("2026-03-31T02:00:00Z"))
        );
        // December → January rollover and a leap year.
        assert_eq!(
            next_occurrence(&s, utc("2026-12-31T02:00:00Z")),
            Some(utc("2027-01-31T02:00:00Z"))
        );
        assert_eq!(
            next_occurrence(&s, utc("2027-12-31T02:00:00Z")),
            Some(utc("2028-01-31T02:00:00Z"))
        );
        assert_eq!(
            next_occurrence(&s, utc("2028-01-31T02:00:00Z")),
            Some(utc("2028-02-29T02:00:00Z"))
        );
    }

    #[test]
    fn end_at_stops_the_series() {
        let mut o = opts("2026-10-01T09:00:00+07:00", "Asia/Jakarta", "daily");
        o.end_at = "2026-10-03T09:00:00+07:00".into();
        let s = spec(o);
        assert_eq!(
            next_occurrence(&s, utc("2026-10-02T02:00:00Z")),
            Some(utc("2026-10-03T02:00:00Z"))
        );
        assert_eq!(next_occurrence(&s, utc("2026-10-03T02:00:00Z")), None);
    }

    #[test]
    fn dst_gap_moves_past_the_gap_and_the_wall_clock_follows_the_zone() {
        // New York springs forward on 2026-03-08: 02:30 does not exist.
        let s = spec(opts(
            "2026-03-06T02:30:00-05:00",
            "America/New_York",
            "daily",
        ));
        assert_eq!(
            next_occurrence(&s, utc("2026-03-06T07:30:00Z")),
            Some(utc("2026-03-07T07:30:00Z"))
        );
        assert_eq!(
            next_occurrence(&s, utc("2026-03-07T07:30:00Z")),
            Some(utc("2026-03-08T07:30:00Z")),
            "02:30 (gap) becomes 03:30 EDT = 07:30Z"
        );
        // After the change the 02:30 wall clock is EDT (UTC-4).
        assert_eq!(
            next_occurrence(&s, utc("2026-03-08T07:30:00Z")),
            Some(utc("2026-03-09T06:30:00Z"))
        );
    }
}
