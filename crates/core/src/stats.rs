//! Consistency and momentum, the two numbers the product treats as primary.
//!
//! Both are computed **per enrollment**. Nothing here takes a list of
//! enrollments, and nothing here returns a combined figure, because a single
//! number spanning a bounded program and a background habit makes both
//! meaningless (PRD F5, section 8).
//!
//! Every function returns `Option`, and `None` means "there was nothing to
//! measure" rather than zero. A user with no available work yet should see a
//! dash, not 0%, which reads as failure they did not earn.

use crate::weekly::WeekBucket;

/// Trailing window for consistency, in local days. PRD F5.
pub const CONSISTENCY_WINDOW_DAYS: u32 = 30;
/// Number of prior ISO weeks momentum compares against. System design 4.6.
pub const MOMENTUM_BASELINE_WEEKS: usize = 4;

/// One finalised day's contribution, as far as the trailing figures care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DayTotals {
    pub earned: i32,
    pub available: i32,
    /// Declared rest days leave the denominator entirely, so resting cannot
    /// lower consistency (PRD F6).
    pub is_rest: bool,
}

impl DayTotals {
    pub fn new(earned: i32, available: i32) -> Self {
        Self {
            earned,
            available,
            is_rest: false,
        }
    }

    pub fn rest() -> Self {
        Self {
            earned: 0,
            available: 0,
            is_rest: true,
        }
    }

    /// A frozen day: a miss that a banked freeze absorbed.
    ///
    /// It stays in the denominator with nothing earned. A freeze protects the
    /// *streak*, not the consistency figure — treating it as a rest day would
    /// let someone bank three freezes and report 100% consistency for a week
    /// they did nothing in, which is exactly the number the product says it
    /// treats as primary.
    pub fn frozen(available: i32) -> Self {
        Self {
            earned: 0,
            available,
            is_rest: false,
        }
    }
}

/// Earned over available as a percentage, or `None` when nothing was available.
pub fn completion_rate(earned: i32, available: i32) -> Option<u8> {
    if available <= 0 {
        return None;
    }

    let rate = (f64::from(earned.max(0)) / f64::from(available) * 100.0).round();
    Some(rate.clamp(0.0, 100.0) as u8)
}

/// Sum a set of days into an `(earned, available)` pair, dropping rest days.
pub fn totals(days: &[DayTotals]) -> (i32, i32) {
    days.iter()
        .filter(|day| !day.is_rest)
        .fold((0, 0), |(earned, available), day| {
            (
                earned.saturating_add(day.earned),
                available.saturating_add(day.available),
            )
        })
}

/// Trailing-30-day consistency for one enrollment.
///
/// `days` is the caller's already-windowed slice — the window is chosen in the
/// enrollment's own timezone, never with `current_date` in SQL. `buckets` are
/// the `n_per_week` buckets overlapping the same window; their points are added
/// to both sides of the ratio so a floating task counts exactly once, on a
/// weekly basis, rather than once per day it appeared on.
pub fn consistency(days: &[DayTotals], buckets: &[WeekBucket]) -> Option<u8> {
    let (mut earned, mut available) = totals(days);

    for bucket in buckets {
        earned = earned.saturating_add(bucket.earned_points());
        available = available.saturating_add(bucket.available_points());
    }

    completion_rate(earned, available)
}

/// One ISO week's totals, for momentum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WeekTotals {
    pub earned: i32,
    pub available: i32,
}

impl WeekTotals {
    pub fn rate(&self) -> Option<u8> {
        completion_rate(self.earned, self.available)
    }
}

/// This ISO week's completion rate minus the mean of the previous four, in
/// signed percentage points.
///
/// Compares the user only to their own past, which is where the motivation
/// actually lives. Returns `None` until there is both a current week and at
/// least one baseline week with work in it — showing "+0" to someone in their
/// first week is a lie about a comparison that has not happened yet.
///
/// Weeks with no available work (a fully rested week, or a week before the
/// program started) are dropped from the baseline rather than counted as zero.
pub fn momentum(this_week: WeekTotals, previous_weeks: &[WeekTotals]) -> Option<i16> {
    let current = this_week.rate()?;

    let baseline: Vec<u8> = previous_weeks
        .iter()
        .rev()
        .take(MOMENTUM_BASELINE_WEEKS)
        .filter_map(WeekTotals::rate)
        .collect();

    if baseline.is_empty() {
        return None;
    }

    let sum: u32 = baseline.iter().map(|rate| u32::from(*rate)).sum();
    let mean = f64::from(sum) / baseline.len() as f64;

    Some((f64::from(current) - mean).round() as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_rate_distinguishes_nothing_from_zero() {
        assert_eq!(completion_rate(0, 0), None);
        assert_eq!(completion_rate(0, 40), Some(0));
        assert_eq!(completion_rate(20, 40), Some(50));
        assert_eq!(completion_rate(40, 40), Some(100));
    }

    #[test]
    fn completion_rate_cannot_exceed_one_hundred() {
        assert_eq!(completion_rate(90, 40), Some(100));
    }

    #[test]
    fn rest_days_leave_the_denominator() {
        let days = [
            DayTotals::new(50, 100),
            DayTotals::rest(),
            DayTotals::new(50, 100),
        ];

        // 100/200, not 100/300. Resting is neutral, never a penalty.
        assert_eq!(consistency(&days, &[]), Some(50));
    }

    #[test]
    fn a_rest_day_cannot_lower_consistency() {
        let worked = [DayTotals::new(80, 100)];
        let worked_then_rested = [DayTotals::new(80, 100), DayTotals::rest()];

        assert_eq!(
            consistency(&worked, &[]),
            consistency(&worked_then_rested, &[])
        );
    }

    #[test]
    fn a_frozen_day_still_counts_against_consistency() {
        let rested = [DayTotals::new(100, 100), DayTotals::rest()];
        let frozen = [DayTotals::new(100, 100), DayTotals::frozen(100)];

        assert_eq!(consistency(&rested, &[]), Some(100));
        // The freeze saved the streak. It does not launder the missed day.
        assert_eq!(consistency(&frozen, &[]), Some(50));
    }

    #[test]
    fn consistency_is_none_before_there_is_anything_to_measure() {
        assert_eq!(consistency(&[], &[]), None);
        assert_eq!(consistency(&[DayTotals::rest()], &[]), None);
    }

    #[test]
    fn weekly_buckets_join_both_sides_of_the_ratio() {
        let days = [DayTotals::new(50, 100)];
        let mut bucket = WeekBucket::new(3, 30);
        bucket.complete_one().expect("one of three");

        // Days give 50/100. The bucket adds 30/90. Total 80/190 = 42%.
        assert_eq!(consistency(&days, &[bucket]), Some(42));
    }

    #[test]
    fn momentum_is_signed_percentage_points_against_the_last_four_weeks() {
        let this_week = WeekTotals {
            earned: 90,
            available: 100,
        };
        let previous = [
            WeekTotals {
                earned: 50,
                available: 100,
            },
            WeekTotals {
                earned: 60,
                available: 100,
            },
            WeekTotals {
                earned: 70,
                available: 100,
            },
            WeekTotals {
                earned: 80,
                available: 100,
            },
        ];

        // 90 against a mean of 65.
        assert_eq!(momentum(this_week, &previous), Some(25));
    }

    #[test]
    fn momentum_can_be_negative() {
        let this_week = WeekTotals {
            earned: 30,
            available: 100,
        };
        let previous = [WeekTotals {
            earned: 80,
            available: 100,
        }];

        assert_eq!(momentum(this_week, &previous), Some(-50));
    }

    #[test]
    fn momentum_uses_only_the_four_most_recent_baseline_weeks() {
        let this_week = WeekTotals {
            earned: 50,
            available: 100,
        };
        // Oldest first. The leading 0% weeks must not drag the baseline down.
        let previous = [
            WeekTotals {
                earned: 0,
                available: 100,
            },
            WeekTotals {
                earned: 0,
                available: 100,
            },
            WeekTotals {
                earned: 50,
                available: 100,
            },
            WeekTotals {
                earned: 50,
                available: 100,
            },
            WeekTotals {
                earned: 50,
                available: 100,
            },
            WeekTotals {
                earned: 50,
                available: 100,
            },
        ];

        assert_eq!(momentum(this_week, &previous), Some(0));
    }

    #[test]
    fn momentum_is_none_in_the_first_week() {
        let this_week = WeekTotals {
            earned: 90,
            available: 100,
        };

        assert_eq!(momentum(this_week, &[]), None);
        // A baseline of weeks with no work in them is still no baseline.
        assert_eq!(momentum(this_week, &[WeekTotals::default()]), None);
    }

    #[test]
    fn momentum_is_none_before_this_week_has_any_work() {
        let previous = [WeekTotals {
            earned: 80,
            available: 100,
        }];

        assert_eq!(momentum(WeekTotals::default(), &previous), None);
    }
}
