use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalendarError {
    InvalidBoundaryHour,
    UnresolvableLocalTime,
}

pub fn day_window(
    local_date: NaiveDate,
    boundary_hour: u32,
    tz: Tz,
) -> Result<(DateTime<Utc>, DateTime<Utc>), CalendarError> {
    if boundary_hour > 4 {
        return Err(CalendarError::InvalidBoundaryHour);
    }

    let Some(boundary) = NaiveTime::from_hms_opt(boundary_hour, 0, 0) else {
        return Err(CalendarError::InvalidBoundaryHour);
    };
    let naive = local_date.and_time(boundary);

    let opens = match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(earlier, _later) => earlier,
        LocalResult::None => {
            let shifted = naive + Duration::hours(1);
            tz.from_local_datetime(&shifted)
                .single()
                .ok_or(CalendarError::UnresolvableLocalTime)?
        }
    };

    Ok((
        opens.with_timezone(&Utc),
        (opens + Duration::days(1)).with_timezone(&Utc),
    ))
}

pub fn enrollment_today(
    now: DateTime<Utc>,
    boundary_hour: u32,
    tz: Tz,
) -> Result<NaiveDate, CalendarError> {
    if boundary_hour > 4 {
        return Err(CalendarError::InvalidBoundaryHour);
    }

    let local = now.with_timezone(&tz) - Duration::hours(boundary_hour as i64);
    Ok(local.date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("test date is valid")
    }

    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("test UTC datetime is valid")
    }

    #[test]
    fn computes_lagos_midnight_window() {
        let (opens, closes) =
            day_window(date(2026, 7, 31), 0, chrono_tz::Africa::Lagos).expect("window resolves");

        assert_eq!(opens, utc(2026, 7, 30, 23, 0));
        assert_eq!(closes, utc(2026, 7, 31, 23, 0));
    }

    #[test]
    fn chooses_earlier_in_fall_back_ambiguity() {
        let (opens, _) = day_window(date(2026, 11, 1), 1, chrono_tz::America::New_York)
            .expect("ambiguous window resolves");

        assert_eq!(opens, utc(2026, 11, 1, 5, 0));
    }

    #[test]
    fn shifts_forward_in_spring_gap() {
        let (opens, _) = day_window(date(2026, 3, 8), 2, chrono_tz::America::New_York)
            .expect("gap window resolves");

        assert_eq!(opens, utc(2026, 3, 8, 7, 0));
    }

    #[test]
    fn computes_today_with_boundary_shift() {
        let now = utc(2026, 7, 31, 1, 30);
        assert_eq!(
            enrollment_today(now, 3, chrono_tz::Africa::Lagos).expect("today resolves"),
            date(2026, 7, 30)
        );
    }

    #[test]
    fn rejects_invalid_boundary_hour() {
        assert_eq!(
            day_window(date(2026, 7, 31), 5, chrono_tz::UTC),
            Err(CalendarError::InvalidBoundaryHour)
        );
    }
}
