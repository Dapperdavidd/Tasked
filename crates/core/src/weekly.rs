//! Floating tasks and the weekly buckets that hold them.
//!
//! The `n_per_week` cadence is the case most trackers get wrong. "Gym 3x a week"
//! is not three specific days, and a tracker that picks the days for you is
//! wrong on Wednesday and guilt-inducing on Thursday.
//!
//! The rules, from system design 4.5:
//!
//! - A floating task appears on **every** day of the ISO week.
//! - It is **excluded from the day's `available_points`**, so an untouched
//!   floating task can never drag a day below the missed threshold.
//! - Its points live in a week bucket instead, and consistency sums daily
//!   denominators plus weekly buckets.
//! - Once the week's quota is met it stops appearing for the rest of the week.
//!
//! The payoff is the invariant in [`WeekBucket`]: someone who runs Monday,
//! Wednesday and Friday scores exactly the same as someone who runs Friday,
//! Saturday and Sunday.

use chrono::{Datelike, NaiveDate};

/// ISO-8601 week key. The ISO year is not always the calendar year: 2027-01-01
/// falls in ISO week 53 of 2026, which is why this carries its own year field
/// rather than pairing a week number with `date.year()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IsoWeek {
    pub year: i32,
    pub week: u32,
}

pub fn iso_week_of(local_date: NaiveDate) -> IsoWeek {
    let week = local_date.iso_week();
    IsoWeek {
        year: week.year(),
        week: week.week(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeeklyError {
    /// The quota was already met; the task should not have been on screen.
    QuotaAlreadyMet,
}

/// One `n_per_week` template's progress through one ISO week.
///
/// Mirrors a `week_buckets` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeekBucket {
    pub required: u32,
    pub completed: u32,
    pub points_each: i32,
}

impl WeekBucket {
    pub fn new(required: u32, points_each: i32) -> Self {
        Self {
            required,
            completed: 0,
            points_each,
        }
    }

    pub fn quota_met(&self) -> bool {
        self.completed >= self.required
    }

    pub fn remaining(&self) -> u32 {
        self.required.saturating_sub(self.completed)
    }

    /// Whether the task should still be rendered on the remaining days of the
    /// week. A met quota removes the row rather than showing it ticked, because
    /// the point of the cap is that the week is done, not that today is.
    pub fn shows_today(&self) -> bool {
        !self.quota_met()
    }

    /// The bucket's contribution to the consistency denominator.
    pub fn available_points(&self) -> i32 {
        self.points_each
            .saturating_mul(i32::try_from(self.required).unwrap_or(i32::MAX))
    }

    /// The bucket's contribution to the consistency numerator.
    ///
    /// Clamped at `required`: over-delivering on a floating task cannot push a
    /// consistency figure above 100%.
    pub fn earned_points(&self) -> i32 {
        self.points_each
            .saturating_mul(i32::try_from(self.completed.min(self.required)).unwrap_or(i32::MAX))
    }

    /// Record one completion. Idempotency and day-window checks belong to the
    /// caller; this only enforces the quota.
    pub fn complete_one(&mut self) -> Result<(), WeeklyError> {
        if self.quota_met() {
            return Err(WeeklyError::QuotaAlreadyMet);
        }
        self.completed += 1;
        Ok(())
    }

    /// Reverse a completion, for an unticked checkbox.
    pub fn uncomplete_one(&mut self) {
        self.completed = self.completed.saturating_sub(1);
    }
}

/// One materialised task instance, as far as scoring is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstancePoints {
    pub points: i32,
    pub is_floating: bool,
}

/// A day's `available_points`.
///
/// Floating tasks are excluded. This is the single rule that keeps "gym 3x a
/// week" from marking Tuesday as missed, and it lives here rather than in a SQL
/// `sum()` so that it cannot be quietly dropped by a future query rewrite.
pub fn day_available_points(instances: &[InstancePoints]) -> i32 {
    instances
        .iter()
        .filter(|instance| !instance.is_floating)
        .map(|instance| instance.points)
        .sum()
}

/// A day's `earned_points`, on the same basis.
pub fn day_earned_points(completed: &[InstancePoints]) -> i32 {
    day_available_points(completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("test date is valid")
    }

    #[test]
    fn iso_week_year_is_not_the_calendar_year_at_the_boundary() {
        // 2027-01-01 is a Friday, so ISO puts it in the last week of 2026.
        assert_eq!(
            iso_week_of(date(2027, 1, 1)),
            IsoWeek {
                year: 2026,
                week: 53
            }
        );
        assert_eq!(
            iso_week_of(date(2026, 7, 31)),
            IsoWeek {
                year: 2026,
                week: 31
            }
        );
    }

    #[test]
    fn a_week_is_one_bucket_regardless_of_which_days_are_used() {
        let monday = date(2026, 7, 27);
        for offset in 0..7 {
            let day = monday
                .checked_add_signed(chrono::Duration::days(offset))
                .expect("test date is in range");
            assert_eq!(iso_week_of(day), iso_week_of(monday));
        }
    }

    #[test]
    fn floating_tasks_stay_out_of_the_day_denominator() {
        let instances = [
            InstancePoints {
                points: 25,
                is_floating: false,
            },
            InstancePoints {
                points: 40,
                is_floating: true,
            },
        ];

        // The untouched floating task must not be able to halve the day's score.
        assert_eq!(day_available_points(&instances), 25);
    }

    #[test]
    fn the_row_disappears_once_the_quota_is_met() {
        let mut bucket = WeekBucket::new(3, 30);

        assert!(bucket.shows_today());
        assert_eq!(bucket.remaining(), 3);

        bucket.complete_one().expect("first of three");
        bucket.complete_one().expect("second of three");
        assert!(bucket.shows_today());

        bucket.complete_one().expect("third of three");
        assert!(!bucket.shows_today());
        assert_eq!(bucket.remaining(), 0);
        assert_eq!(bucket.complete_one(), Err(WeeklyError::QuotaAlreadyMet));
    }

    #[test]
    fn unticking_frees_the_slot_again() {
        let mut bucket = WeekBucket::new(2, 30);
        bucket.complete_one().expect("first of two");
        bucket.complete_one().expect("second of two");
        assert!(!bucket.shows_today());

        bucket.uncomplete_one();
        assert!(bucket.shows_today());
        assert_eq!(bucket.completed, 1);

        // Unticking past zero is a no-op rather than an underflow.
        bucket.uncomplete_one();
        bucket.uncomplete_one();
        assert_eq!(bucket.completed, 0);
    }

    /// The invariant the whole module exists for.
    ///
    /// Walks a real week day by day rather than just completing a bucket three
    /// times, so it also proves the floating task never touched a daily
    /// denominator and that the row vanished once the quota was met.
    fn run_week(session_days: [bool; 7]) -> (WeekBucket, i32, i32, u32) {
        let mut bucket = WeekBucket::new(3, 30);
        let mut daily_available = 0;
        let mut daily_earned = 0;
        let mut days_the_row_was_shown = 0;

        for did_session in session_days {
            // One ordinary 25-point task every day, plus the floating one.
            let instances = [
                InstancePoints {
                    points: 25,
                    is_floating: false,
                },
                InstancePoints {
                    points: 30,
                    is_floating: true,
                },
            ];

            if bucket.shows_today() {
                days_the_row_was_shown += 1;
            }

            daily_available += day_available_points(&instances);
            daily_earned += day_earned_points(&instances[..1]);

            if did_session && bucket.shows_today() {
                bucket.complete_one().expect("quota was checked first");
            }
        }

        (
            bucket,
            daily_earned,
            daily_available,
            days_the_row_was_shown,
        )
    }

    #[test]
    fn monday_wednesday_friday_scores_the_same_as_friday_saturday_sunday() {
        let mwf = run_week([true, false, true, false, true, false, false]);
        let fss = run_week([false, false, false, false, true, true, true]);

        let (mwf_bucket, mwf_earned, mwf_available, _) = mwf;
        let (fss_bucket, fss_earned, fss_available, _) = fss;

        // Identical consistency inputs, which is the whole claim.
        assert_eq!(
            (
                mwf_earned + mwf_bucket.earned_points(),
                mwf_available + mwf_bucket.available_points()
            ),
            (
                fss_earned + fss_bucket.earned_points(),
                fss_available + fss_bucket.available_points()
            )
        );

        // The floating task contributed 90 points of quota to each, and nothing
        // at all to any day's denominator.
        assert_eq!(mwf_bucket.earned_points(), 90);
        assert_eq!(mwf_available, 7 * 25);
    }

    #[test]
    fn finishing_early_retires_the_row_for_the_rest_of_the_week() {
        let (bucket, _, _, shown_on) = run_week([true, true, true, false, false, false, false]);

        assert!(bucket.quota_met());
        // Shown Monday, Tuesday, Wednesday, then gone.
        assert_eq!(shown_on, 3);
    }

    #[test]
    fn a_partly_done_week_is_scored_proportionally() {
        let mut bucket = WeekBucket::new(3, 30);
        bucket.complete_one().expect("one of three");

        assert_eq!(bucket.earned_points(), 30);
        assert_eq!(bucket.available_points(), 90);
    }
}
