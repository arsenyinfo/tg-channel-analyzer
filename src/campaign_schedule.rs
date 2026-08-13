use chrono::{DateTime, Days, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use std::error::Error;
use std::str::FromStr;

type ScheduleError = Box<dyn Error + Send + Sync>;

fn local_datetime(timezone: Tz, date: NaiveDate, time: NaiveTime) -> DateTime<Tz> {
    let mut naive = date.and_time(time);
    loop {
        match timezone.from_local_datetime(&naive) {
            LocalResult::Single(value) => return value,
            LocalResult::Ambiguous(first, second) => return first.min(second),
            LocalResult::None => naive += Duration::minutes(1),
        }
    }
}

pub fn next_allowed_time(
    candidate: DateTime<Utc>,
    timezone: Tz,
    start: NaiveTime,
    end: NaiveTime,
) -> Result<DateTime<Utc>, ScheduleError> {
    if start >= end {
        return Err("send window start must be before end".into());
    }
    let local = candidate.with_timezone(&timezone);
    let date = local.date_naive();
    let opening = local_datetime(timezone, date, start);
    let closing = local_datetime(timezone, date, end);
    if local < opening {
        return Ok(opening.with_timezone(&Utc));
    }
    if local < closing {
        return Ok(candidate);
    }
    let next_date = date
        .checked_add_days(Days::new(1))
        .ok_or("campaign schedule date overflow")?;
    Ok(local_datetime(timezone, next_date, start).with_timezone(&Utc))
}

pub fn normalize_retry_time(
    candidate: DateTime<Utc>,
    timezone: &str,
    window_start: NaiveTime,
    window_end: NaiveTime,
) -> Result<DateTime<Utc>, ScheduleError> {
    let timezone = Tz::from_str(timezone).map_err(|_| "invalid campaign timezone")?;
    next_allowed_time(candidate, timezone, window_start, window_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_night_to_morning_and_spills_after_close() {
        let timezone = Tz::Europe__Warsaw;
        let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let end = NaiveTime::from_hms_opt(20, 0, 0).unwrap();
        let before = Utc.with_ymd_and_hms(2026, 8, 13, 4, 0, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 8, 13, 19, 30, 0).unwrap();

        assert_eq!(
            next_allowed_time(before, timezone, start, end).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 13, 7, 0, 0).unwrap()
        );
        assert_eq!(
            next_allowed_time(after, timezone, start, end).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 14, 7, 0, 0).unwrap()
        );
    }

    #[test]
    fn handles_dst_boundaries() {
        let timezone = Tz::Europe__Warsaw;
        let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let end = NaiveTime::from_hms_opt(20, 0, 0).unwrap();
        let after_close_before_spring_change = Utc.with_ymd_and_hms(2026, 3, 28, 20, 0, 0).unwrap();

        assert_eq!(
            next_allowed_time(after_close_before_spring_change, timezone, start, end).unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 29, 7, 0, 0).unwrap()
        );
    }
}
