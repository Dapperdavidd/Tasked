//! Which notifications actually get sent.
//!
//! This is a filter, not a scheduler. The jobs decide *when* to consider
//! sending something; this decides whether it survives contact with the three
//! rules in PRD F8 that exist to stop the product becoming a nag:
//!
//! 1. **Never more than three in twenty-four hours.**
//! 2. **Never during declared quiet hours.**
//! 3. **Composed across enrollments, never one per enrollment.** If both a
//!    bounded program streak and a standing streak are at risk, that is one
//!    notification, not two.
//!
//! The third rule is the one that will get broken by accident. Each enrollment
//! finalises independently, in its own transaction, so it is natural for each
//! to enqueue its own reminder — and the user gets two pushes about the same
//! evening. Composition is enforced here, once, rather than in each job.
//!
//! Notification payloads carry **counts and kinds, never task content**. A
//! lock-screen preview is the least private surface in the product, and this is
//! an app people put therapy homework into.

use chrono::{DateTime, Duration, NaiveTime, Utc};
use chrono_tz::Tz;

/// PRD F8: three in twenty-four hours is the ceiling, across every kind.
pub const MAX_PER_24_HOURS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotificationKind {
    /// The one-off recovery offer. Highest value: it is the difference between
    /// a user returning and a user churning, and it expires.
    RepairAvailable,
    /// Composed across enrollments. Fires at the day boundary minus three
    /// hours, only when a streak is genuinely worth protecting.
    StreakAtRisk,
    /// Neutral. States the day and the total task count across both sections.
    MorningCard,
    /// Loss framed only when a streak is genuinely at risk.
    EveningCheckIn,
    /// At most one per month across the whole standing list.
    StandingDrift,
    /// Aggregate only, at most once daily. Lowest value, first to be dropped.
    CohortPulse,
}

impl NotificationKind {
    /// Lower sorts first and survives the daily cap.
    ///
    /// Ordering is by what the user loses if it never arrives. A missed repair
    /// window cannot be recovered; a cohort pulse is noise by comparison.
    pub const fn priority(self) -> u8 {
        match self {
            Self::RepairAvailable => 0,
            Self::StreakAtRisk => 1,
            Self::MorningCard => 2,
            Self::EveningCheckIn => 3,
            Self::StandingDrift => 4,
            Self::CohortPulse => 5,
        }
    }
}

/// A window in the user's local time during which nothing may be delivered.
///
/// Wraps midnight, which is the common case: 23:00 to 06:00.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietHours {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl QuietHours {
    pub fn contains(&self, local: NaiveTime) -> bool {
        if self.start == self.end {
            // A zero-width window silences nothing. Treating it as silencing
            // everything would mute a user who set both fields to the same
            // value by accident.
            return false;
        }
        if self.start < self.end {
            local >= self.start && local < self.end
        } else {
            // Wraps midnight.
            local >= self.start || local < self.end
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub kind: NotificationKind,
    pub at: DateTime<Utc>,
}

/// Why a candidate did not survive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// Inside the user's declared quiet hours.
    QuietHours,
    /// Three already land in the same rolling twenty-four hours.
    DailyCapReached,
    /// Another candidate of the same kind already covers this delivery. This is
    /// the composition rule: two enrollments, one notification.
    ComposedIntoAnother,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub send: Vec<Candidate>,
    pub dropped: Vec<(Candidate, Dropped)>,
}

/// Decide what to send.
///
/// `already_sent` is the delivery history the caller has for this user, used so
/// the cap spans job runs rather than resetting every tick. Entries outside the
/// relevant window are ignored, so callers may pass a generous slice.
pub fn plan(
    candidates: &[Candidate],
    quiet: Option<QuietHours>,
    timezone: Tz,
    already_sent: &[DateTime<Utc>],
    suppressed: &[NotificationKind],
) -> Plan {
    let mut ordered: Vec<Candidate> = candidates
        .iter()
        .copied()
        .filter(|candidate| !suppressed.contains(&candidate.kind))
        .collect();

    // Priority first so the cap drops the least valuable, then time so the
    // result is stable and reads chronologically within a priority band.
    ordered.sort_by_key(|candidate| (candidate.kind.priority(), candidate.at));

    let mut plan = Plan::default();

    for candidate in ordered {
        // Composition: one notification per kind, however many enrollments
        // asked for it.
        if plan.send.iter().any(|kept| kept.kind == candidate.kind) {
            plan.dropped.push((candidate, Dropped::ComposedIntoAnother));
            continue;
        }

        let local = candidate.at.with_timezone(&timezone).time();
        if quiet.is_some_and(|window| window.contains(local)) {
            plan.dropped.push((candidate, Dropped::QuietHours));
            continue;
        }

        if would_exceed_cap(&candidate, &plan.send, already_sent) {
            plan.dropped.push((candidate, Dropped::DailyCapReached));
            continue;
        }

        plan.send.push(candidate);
    }

    plan.send.sort_by_key(|candidate| candidate.at);
    plan
}

/// Whether delivering `candidate` would put four notifications inside any
/// twenty-four hour window.
fn would_exceed_cap(
    candidate: &Candidate,
    accepted: &[Candidate],
    already_sent: &[DateTime<Utc>],
) -> bool {
    let window = Duration::hours(24);

    let nearby = already_sent
        .iter()
        .copied()
        .chain(accepted.iter().map(|kept| kept.at))
        .filter(|sent| (*sent - candidate.at).abs() < window)
        .count();

    nearby >= MAX_PER_24_HOURS
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, day, hour, minute, 0)
            .single()
            .expect("test instant is valid")
    }

    fn candidate(kind: NotificationKind, day: u32, hour: u32) -> Candidate {
        Candidate {
            kind,
            at: utc(day, hour, 0),
        }
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).expect("test time is valid")
    }

    const LAGOS: Tz = chrono_tz::Africa::Lagos;

    // --- composition --------------------------------------------------------

    /// The headline rule. Each enrollment finalises in its own transaction, so
    /// two at-risk candidates for the same evening is the *expected* input.
    #[test]
    fn two_enrollments_at_risk_produce_one_notification() {
        let candidates = [
            candidate(NotificationKind::StreakAtRisk, 1, 17),
            candidate(NotificationKind::StreakAtRisk, 1, 17),
        ];

        let result = plan(&candidates, None, LAGOS, &[], &[]);

        assert_eq!(result.send.len(), 1);
        assert_eq!(
            result.dropped,
            vec![(
                candidate(NotificationKind::StreakAtRisk, 1, 17),
                Dropped::ComposedIntoAnother
            )]
        );
    }

    #[test]
    fn different_kinds_are_not_composed_together() {
        let candidates = [
            candidate(NotificationKind::MorningCard, 1, 6),
            candidate(NotificationKind::EveningCheckIn, 1, 19),
        ];

        let result = plan(&candidates, None, LAGOS, &[], &[]);
        assert_eq!(result.send.len(), 2);
    }

    // --- quiet hours --------------------------------------------------------

    #[test]
    fn quiet_hours_wrapping_midnight_silence_the_night() {
        let quiet = QuietHours {
            start: time(23, 0),
            end: time(6, 0),
        };

        assert!(quiet.contains(time(23, 30)));
        assert!(quiet.contains(time(0, 15)));
        assert!(quiet.contains(time(5, 59)));
        assert!(!quiet.contains(time(6, 0)), "the window is half open");
        assert!(!quiet.contains(time(12, 0)));
    }

    #[test]
    fn quiet_hours_within_one_day_also_work() {
        let quiet = QuietHours {
            start: time(13, 0),
            end: time(14, 0),
        };

        assert!(quiet.contains(time(13, 30)));
        assert!(!quiet.contains(time(12, 59)));
        assert!(!quiet.contains(time(14, 0)));
    }

    #[test]
    fn an_empty_quiet_window_silences_nothing() {
        // Someone who set both fields to the same value meant "no quiet hours",
        // not "never contact me".
        let quiet = QuietHours {
            start: time(22, 0),
            end: time(22, 0),
        };
        assert!(!quiet.contains(time(22, 0)));
        assert!(!quiet.contains(time(3, 0)));
    }

    #[test]
    fn quiet_hours_are_evaluated_in_the_users_timezone_not_utc() {
        // 23:30 UTC is 00:30 in Lagos, which is inside a 23:00-06:00 window.
        let quiet = QuietHours {
            start: time(23, 0),
            end: time(6, 0),
        };
        let candidates = [candidate(NotificationKind::CohortPulse, 1, 23)];

        let result = plan(&candidates, Some(quiet), LAGOS, &[], &[]);

        assert!(result.send.is_empty(), "delivered inside quiet hours");
        assert_eq!(result.dropped[0].1, Dropped::QuietHours);
    }

    // --- the daily cap ------------------------------------------------------

    #[test]
    fn no_more_than_three_land_in_a_day_and_the_least_valuable_go_first() {
        let candidates = [
            candidate(NotificationKind::CohortPulse, 1, 12),
            candidate(NotificationKind::MorningCard, 1, 6),
            candidate(NotificationKind::StandingDrift, 1, 13),
            candidate(NotificationKind::EveningCheckIn, 1, 19),
            candidate(NotificationKind::RepairAvailable, 1, 9),
        ];

        let result = plan(&candidates, None, LAGOS, &[], &[]);

        assert_eq!(result.send.len(), MAX_PER_24_HOURS);
        let kinds: Vec<NotificationKind> = result.send.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            vec![
                NotificationKind::MorningCard,
                NotificationKind::RepairAvailable,
                NotificationKind::EveningCheckIn,
            ],
            "sent list is chronological"
        );
        assert!(result
            .dropped
            .iter()
            .all(|(_, reason)| *reason == Dropped::DailyCapReached));
    }

    #[test]
    fn notifications_already_sent_count_toward_the_cap() {
        let already = [utc(1, 6, 0), utc(1, 12, 0)];
        let candidates = [
            candidate(NotificationKind::EveningCheckIn, 1, 19),
            candidate(NotificationKind::CohortPulse, 1, 20),
        ];

        let result = plan(&candidates, None, LAGOS, &already, &[]);

        assert_eq!(result.send.len(), 1, "only one slot was left");
        assert_eq!(result.send[0].kind, NotificationKind::EveningCheckIn);
    }

    #[test]
    fn the_cap_is_a_rolling_window_not_a_calendar_day() {
        // Three sent late yesterday still suppress something early today.
        let already = [utc(1, 21, 0), utc(1, 22, 0), utc(1, 23, 0)];
        let candidates = [candidate(NotificationKind::MorningCard, 2, 6)];

        let result = plan(&candidates, None, LAGOS, &already, &[]);
        assert!(result.send.is_empty());

        // The same candidate a day later is fine.
        let candidates = [candidate(NotificationKind::MorningCard, 3, 6)];
        let result = plan(&candidates, None, LAGOS, &already, &[]);
        assert_eq!(result.send.len(), 1);
    }

    // --- suppression --------------------------------------------------------

    #[test]
    fn each_scheduled_notification_is_independently_suppressible() {
        let candidates = [
            candidate(NotificationKind::MorningCard, 1, 6),
            candidate(NotificationKind::EveningCheckIn, 1, 19),
        ];

        let result = plan(
            &candidates,
            None,
            LAGOS,
            &[],
            &[NotificationKind::MorningCard],
        );

        assert_eq!(result.send.len(), 1);
        assert_eq!(result.send[0].kind, NotificationKind::EveningCheckIn);
        // A suppressed notification was never a candidate; it is not a "drop"
        // the user needs explaining.
        assert!(result.dropped.is_empty());
    }

    #[test]
    fn nothing_in_produces_nothing_out() {
        assert_eq!(plan(&[], None, LAGOS, &[], &[]), Plan::default());
    }

    // --- invariants ---------------------------------------------------------

    #[test]
    fn a_plan_never_exceeds_the_cap_whatever_is_thrown_at_it() {
        let kinds = [
            NotificationKind::RepairAvailable,
            NotificationKind::StreakAtRisk,
            NotificationKind::MorningCard,
            NotificationKind::EveningCheckIn,
            NotificationKind::StandingDrift,
            NotificationKind::CohortPulse,
        ];

        // Every kind, twice over, all in one day.
        let candidates: Vec<Candidate> = kinds
            .iter()
            .flat_map(|kind| {
                [
                    Candidate {
                        kind: *kind,
                        at: utc(1, 8, 0),
                    },
                    Candidate {
                        kind: *kind,
                        at: utc(1, 20, 0),
                    },
                ]
            })
            .collect();

        let result = plan(&candidates, None, LAGOS, &[], &[]);

        assert!(result.send.len() <= MAX_PER_24_HOURS);
        assert_eq!(
            result.send.len() + result.dropped.len(),
            candidates.len(),
            "every candidate is accounted for, sent or explained"
        );

        let mut kinds_sent: Vec<NotificationKind> = result.send.iter().map(|c| c.kind).collect();
        let before = kinds_sent.len();
        kinds_sent.sort();
        kinds_sent.dedup();
        assert_eq!(before, kinds_sent.len(), "a kind was sent twice");
    }
}
