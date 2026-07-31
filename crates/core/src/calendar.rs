//! Local-date arithmetic for enrollments.
//!
//! Two rules govern everything here.
//!
//! 1. **Days tile.** `closes_at(d)` is defined as `opens_at(d + 1)`, never as
//!    `opens_at(d) + 24h`. A local day is 23 or 25 hours long across a DST
//!    transition, so the naive definition leaves an hour with no open day in it
//!    on fall back and an hour with two open days in it on spring forward.
//! 2. **One source of truth.** `enrollment_today` is derived from `day_window`
//!    rather than computed independently, so the two cannot drift.

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

/// Widest boundary the product allows, per PRD F2: midnight to 04:00 local.
pub const MAX_BOUNDARY_HOUR: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalendarError {
    InvalidBoundaryHour,
    UnresolvableLocalTime,
    DateOutOfRange,
}

/// The instant at which `local_date` begins for an enrollment on `boundary_hour`.
pub fn day_open(
    local_date: NaiveDate,
    boundary_hour: u32,
    tz: Tz,
) -> Result<DateTime<Utc>, CalendarError> {
    if boundary_hour > MAX_BOUNDARY_HOUR {
        return Err(CalendarError::InvalidBoundaryHour);
    }

    let Some(boundary) = NaiveTime::from_hms_opt(boundary_hour, 0, 0) else {
        return Err(CalendarError::InvalidBoundaryHour);
    };

    resolve_local(local_date.and_time(boundary), tz)
        .map(|opens| opens.with_timezone(&Utc))
        .ok_or(CalendarError::UnresolvableLocalTime)
}

/// The half-open interval `[opens_at, closes_at)` covering `local_date`.
///
/// Consecutive days tile exactly: `day_window(d).1 == day_window(d + 1).0`.
pub fn day_window(
    local_date: NaiveDate,
    boundary_hour: u32,
    tz: Tz,
) -> Result<(DateTime<Utc>, DateTime<Utc>), CalendarError> {
    let opens = day_open(local_date, boundary_hour, tz)?;
    let next = local_date.succ_opt().ok_or(CalendarError::DateOutOfRange)?;
    let closes = day_open(next, boundary_hour, tz)?;
    Ok((opens, closes))
}

/// The enrollment's current local date: the `d` whose window contains `now`.
///
/// Deliberately not `(local_wall_clock - boundary_hour).date()`. That expression
/// is right on all but two days a year, and the two days it is wrong on are the
/// ones that cost you a user.
pub fn enrollment_today(
    now: DateTime<Utc>,
    boundary_hour: u32,
    tz: Tz,
) -> Result<NaiveDate, CalendarError> {
    if boundary_hour > MAX_BOUNDARY_HOUR {
        return Err(CalendarError::InvalidBoundaryHour);
    }

    // Cheap guess from the local wall clock, then confirmed against the real
    // window. A DST transition can put the guess one day out in either direction.
    let local = now.with_timezone(&tz).naive_local();
    let candidate = (local - Duration::hours(i64::from(boundary_hour))).date();

    for delta in [0, -1, 1] {
        let Some(probe) = candidate.checked_add_signed(Duration::days(delta)) else {
            continue;
        };
        let (opens, closes) = day_window(probe, boundary_hour, tz)?;
        if now >= opens && now < closes {
            return Ok(probe);
        }
    }

    Err(CalendarError::UnresolvableLocalTime)
}

/// Resolve a local wall-clock time to an instant, handling both DST edges.
///
/// Fall back: the time happens twice, take the earlier, so the day starts as
/// early as possible and the user is never told their day has not begun.
/// Spring forward: the time does not exist, take the first instant after the gap.
fn resolve_local(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt),
        LocalResult::Ambiguous(earlier, _later) => Some(earlier),
        LocalResult::None => {
            // Step in 15-minute increments rather than assuming a one-hour gap.
            // Lord Howe Island shifts by 30 minutes, and several historical
            // transitions are neither 30 nor 60.
            let mut probe = naive;
            for _ in 0..24 {
                probe += Duration::minutes(15);
                match tz.from_local_datetime(&probe) {
                    LocalResult::Single(dt) => return Some(dt),
                    LocalResult::Ambiguous(earlier, _later) => return Some(earlier),
                    LocalResult::None => continue,
                }
            }
            None
        }
    }
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
            .expect("test instant is valid")
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
        assert_eq!(
            enrollment_today(utc(2026, 7, 31, 1, 30), 3, chrono_tz::Africa::Lagos)
                .expect("today resolves"),
            date(2026, 7, 30)
        );
    }

    #[test]
    fn rejects_invalid_boundary_hour() {
        assert_eq!(
            day_window(date(2026, 7, 31), 5, chrono_tz::UTC),
            Err(CalendarError::InvalidBoundaryHour)
        );
        assert_eq!(
            enrollment_today(utc(2026, 7, 31, 0, 0), 5, chrono_tz::UTC),
            Err(CalendarError::InvalidBoundaryHour)
        );
    }

    // --- days tile, both hemispheres, both edges ---------------------------

    /// Regression: `closes_at` used to be `opens_at + 24h`, which left an
    /// unowned hour on fall back and a doubly-owned hour on spring forward.
    #[test]
    fn consecutive_days_tile_across_every_dst_transition() {
        let cases = [
            (chrono_tz::America::New_York, date(2026, 3, 8)), // spring forward
            (chrono_tz::America::New_York, date(2026, 11, 1)), // fall back
            (chrono_tz::Europe::London, date(2026, 3, 29)),
            (chrono_tz::Europe::London, date(2026, 10, 25)),
            // Southern hemisphere: the transitions run the other way round.
            (chrono_tz::Australia::Sydney, date(2026, 4, 5)),
            (chrono_tz::Australia::Sydney, date(2026, 10, 4)),
            (chrono_tz::Pacific::Auckland, date(2026, 4, 5)),
            (chrono_tz::Pacific::Auckland, date(2026, 9, 27)),
            // Half-hour shift, which a hard-coded one-hour gap step would miss.
            (chrono_tz::Australia::Lord_Howe, date(2026, 4, 5)),
            (chrono_tz::Australia::Lord_Howe, date(2026, 10, 4)),
            // Zone with no DST at all, as a control.
            (chrono_tz::Africa::Lagos, date(2026, 3, 8)),
        ];

        for (tz, transition) in cases {
            for boundary in 0..=MAX_BOUNDARY_HOUR {
                for offset in -2..=2 {
                    let day = transition
                        .checked_add_signed(Duration::days(offset))
                        .expect("test date is in range");
                    let next = day.succ_opt().expect("test date is in range");

                    let (_, closes) = day_window(day, boundary, tz).expect("window resolves");
                    let (opens_next, _) = day_window(next, boundary, tz).expect("window resolves");

                    assert_eq!(
                        closes, opens_next,
                        "gap or overlap in {tz} at boundary {boundary} on {day}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_dst_day_is_not_twenty_four_hours_long() {
        // Proves the tiling assertion above is load bearing rather than
        // trivially satisfied: these days genuinely are not 24 hours.
        let (opens, closes) =
            day_window(date(2026, 3, 8), 0, chrono_tz::America::New_York).expect("window resolves");
        assert_eq!((closes - opens).num_hours(), 23);

        let (opens, closes) = day_window(date(2026, 11, 1), 0, chrono_tz::America::New_York)
            .expect("window resolves");
        assert_eq!((closes - opens).num_hours(), 25);
    }

    /// Regression: `enrollment_today` used to subtract the boundary from the
    /// *instant* instead of the local wall clock, so on a fall-back day it
    /// returned tomorrow's date for an hour and writes landed on the wrong day.
    #[test]
    fn today_agrees_with_the_window_on_a_fall_back_day() {
        let now = utc(2026, 11, 1, 7, 30); // 02:30 EST, after the repeated hour
        let today = enrollment_today(now, 3, chrono_tz::America::New_York).expect("today resolves");

        assert_eq!(today, date(2026, 10, 31));

        let (opens, closes) =
            day_window(today, 3, chrono_tz::America::New_York).expect("window resolves");
        assert!(now >= opens && now < closes);
    }

    #[test]
    fn today_is_the_day_whose_window_contains_now_across_dst_weeks() {
        let cases = [
            (chrono_tz::America::New_York, utc(2026, 10, 30, 0, 0)),
            (chrono_tz::America::New_York, utc(2026, 3, 6, 0, 0)),
            (chrono_tz::Pacific::Auckland, utc(2026, 9, 25, 0, 0)),
            (chrono_tz::Australia::Lord_Howe, utc(2026, 4, 3, 0, 0)),
        ];

        for (tz, start) in cases {
            for boundary in 0..=MAX_BOUNDARY_HOUR {
                let mut cursor = start;
                let end = start + Duration::days(4);

                while cursor < end {
                    let today = enrollment_today(cursor, boundary, tz)
                        .unwrap_or_else(|_| panic!("today resolves at {cursor} in {tz}"));
                    let (opens, closes) = day_window(today, boundary, tz).expect("window resolves");

                    assert!(
                        cursor >= opens && cursor < closes,
                        "{cursor} fell outside its assigned window in {tz} at boundary {boundary}"
                    );
                    cursor += Duration::minutes(15);
                }
            }
        }
    }
}
