//! The streak state machine.
//!
//! `fold` is the source of truth. The incremental `step` is an optimisation over
//! it, and every correction — a repair, a timezone change, a backfill — is
//! applied by rewriting the affected day outcomes and replaying `fold` from
//! there. For that to work, the day sequence alone has to determine the streak,
//! which is why a missed day with no freeze **breaks** here rather than parking
//! in a `Repairable` state that `fold` could never reach on its own.
//!
//! `Repairable` is a live transient owned by the finaliser, not a fold output.
//! See [`Finalisation`].

/// The outcome stored on a finalised day. Derived from the day's score, except
/// for `Rest`, which the user declares in advance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayOutcome {
    Complete,
    Partial,
    Missed,
    Rest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreakState {
    Active,
    /// Intraday warning. Never produced by `fold`; see [`at_risk`].
    AtRisk,
    /// A missed day whose 24-hour repair window is still open. Never produced by
    /// `fold`; see [`Finalisation::HeldForRepair`].
    Repairable,
    Broken,
}

/// Consecutive non-missed days that earn one freeze.
pub const DAYS_PER_FREEZE: u8 = 7;
/// Most freezes an enrollment can bank at once, per PRD F6.
pub const FREEZE_CAP: u8 = 3;
/// Streak length below which an at-risk notification is not worth sending.
pub const AT_RISK_MIN_STREAK: u32 = 3;
/// Day score below which the day counts as missed, per PRD F5.
pub const MISSED_SCORE: u8 = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Streak {
    pub current: u32,
    pub longest: u32,
    pub freezes: u8,
    pub clean_run: u8,
    pub state: StreakState,
}

impl Default for Streak {
    fn default() -> Self {
        Self {
            current: 0,
            longest: 0,
            freezes: 0,
            clean_run: 0,
            state: StreakState::Active,
        }
    }
}

/// The result of advancing a streak by one day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub streak: Streak,
    /// Set when the stored day must be written as something other than what was
    /// scored. A missed day absorbed by a freeze is stored as `frozen`, so the
    /// heatmap stays honest about what happened and the user is told after the
    /// fact rather than asked in the moment.
    pub rewritten: Option<DayOutcome>,
}

/// Advance the streak by one finalised day.
pub fn step(s: &Streak, outcome: DayOutcome) -> Step {
    let mut next = s.clone();
    let mut rewritten = None;

    match outcome {
        // Neutral. A declared rest day neither advances nor resets anything, and
        // is excluded from consistency denominators elsewhere.
        DayOutcome::Rest => {}
        DayOutcome::Complete => {
            next.current += 1;
            next.longest = next.longest.max(next.current);
            next.clean_run = next.clean_run.saturating_add(1);
            next.state = StreakState::Active;
        }
        // Sustains without incrementing, per PRD F6.
        DayOutcome::Partial => {
            next.clean_run = next.clean_run.saturating_add(1);
            next.state = StreakState::Active;
        }
        DayOutcome::Missed => {
            if next.freezes > 0 {
                next.freezes -= 1;
                next.clean_run = 0;
                next.state = StreakState::Active;
                rewritten = Some(DayOutcome::Rest);
            } else {
                // Broken with `current == 0` and `longest == 0` is
                // indistinguishable from a fresh start, which is correct.
                next.current = 0;
                next.clean_run = 0;
                next.state = StreakState::Broken;
            }
        }
    }

    if next.clean_run >= DAYS_PER_FREEZE && next.freezes < FREEZE_CAP {
        next.freezes += 1;
        next.clean_run = 0;
    }

    Step {
        streak: next,
        rewritten,
    }
}

pub fn fold(days: &[DayOutcome]) -> Streak {
    fold_from(Streak::default(), days)
}

pub fn fold_from(seed: Streak, days: &[DayOutcome]) -> Streak {
    days.iter().fold(seed, |acc, day| step(&acc, *day).streak)
}

/// What the finaliser should write after scoring a day.
///
/// The split exists because the fold's truth and the product's copy disagree for
/// exactly 24 hours. The fold says a missed day breaks the streak. The product
/// says never show a broken streak while recovery is still possible (PRD F6,
/// F11). Both are right; they just apply at different times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finalisation {
    /// Write this straight through.
    Settled(Step),
    /// The day was missed, no freeze was available, and repair is still on the
    /// table. Store `visible` now so the user keeps seeing their streak, and
    /// schedule the repair window. If it closes without a repair, store
    /// `on_expiry`. If the user repairs, rewrite the day's outcome and replay
    /// `fold` from that date instead of using either of these.
    HeldForRepair { visible: Streak, on_expiry: Step },
}

/// Score a finalised day into the streak.
///
/// `repair_available` is the caller's answer to "has this enrollment already used
/// its one repair this calendar month", per PRD F6.
pub fn finalise(before: &Streak, outcome: DayOutcome, repair_available: bool) -> Finalisation {
    let advanced = step(before, outcome);

    let broke_without_a_freeze =
        outcome == DayOutcome::Missed && advanced.streak.state == StreakState::Broken;

    if broke_without_a_freeze && repair_available {
        let mut visible = before.clone();
        visible.state = StreakState::Repairable;
        return Finalisation::HeldForRepair {
            visible,
            on_expiry: advanced,
        };
    }

    Finalisation::Settled(advanced)
}

/// Whether an at-risk notification is warranted right now.
///
/// The scheduler decides *when* to ask (day boundary minus three hours); this
/// decides *whether*. Per PRD F8 the streak must be worth protecting, or the
/// notification is just noise on a day the user was never going to finish.
pub fn at_risk(streak: &Streak, day_score_so_far: u8) -> bool {
    streak.current >= AT_RISK_MIN_STREAK
        && day_score_so_far < MISSED_SCORE
        && matches!(streak.state, StreakState::Active | StreakState::AtRisk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn seeded(current: u32, longest: u32, freezes: u8, clean_run: u8) -> Streak {
        Streak {
            current,
            longest,
            freezes,
            clean_run,
            state: StreakState::Active,
        }
    }

    #[test]
    fn complete_days_increment_current_and_longest() {
        let streak = fold(&[DayOutcome::Complete, DayOutcome::Complete]);
        assert_eq!(streak.current, 2);
        assert_eq!(streak.longest, 2);
    }

    #[test]
    fn seven_clean_days_earn_one_freeze() {
        let streak = fold(&[DayOutcome::Complete; 7]);
        assert_eq!(streak.current, 7);
        assert_eq!(streak.freezes, 1);
        assert_eq!(streak.clean_run, 0);
    }

    #[test]
    fn partial_sustains_without_incrementing() {
        let streak = fold(&[DayOutcome::Complete, DayOutcome::Partial]);
        assert_eq!(streak.current, 1);
        assert_eq!(streak.state, StreakState::Active);
        // A partial day is still a clean day for freeze accrual.
        assert_eq!(streak.clean_run, 2);
    }

    #[test]
    fn miss_consumes_freeze_and_rewrites_to_rest() {
        let advanced = step(&seeded(7, 7, 1, 0), DayOutcome::Missed);

        assert_eq!(advanced.streak.current, 7);
        assert_eq!(advanced.streak.freezes, 0);
        assert_eq!(advanced.streak.state, StreakState::Active);
        assert_eq!(advanced.rewritten, Some(DayOutcome::Rest));
    }

    /// Regression: `step` used to leave `current` untouched and park in
    /// `Repairable`, so `fold` could never reach a broken streak and the nightly
    /// reconciliation job would diverge for every user who had ever missed a day.
    #[test]
    fn miss_without_freeze_breaks_and_preserves_longest() {
        let streak = fold(&[
            DayOutcome::Complete,
            DayOutcome::Complete,
            DayOutcome::Missed,
        ]);

        assert_eq!(streak.current, 0);
        assert_eq!(streak.longest, 2);
        assert_eq!(streak.state, StreakState::Broken);
    }

    #[test]
    fn rest_is_neutral() {
        let before = seeded(4, 9, 2, 3);
        let after = step(&before, DayOutcome::Rest);
        assert_eq!(after.streak, before);
        assert_eq!(after.rewritten, None);
    }

    #[test]
    fn a_new_streak_starts_after_a_break() {
        let streak = fold(&[
            DayOutcome::Complete,
            DayOutcome::Complete,
            DayOutcome::Missed,
            DayOutcome::Complete,
        ]);

        assert_eq!(streak.current, 1);
        assert_eq!(streak.longest, 2);
        assert_eq!(streak.state, StreakState::Active);
    }

    // --- finalisation and repair -------------------------------------------

    #[test]
    fn a_missed_day_is_held_at_the_visible_streak_while_repair_is_open() {
        let before = seeded(12, 27, 0, 3);
        let outcome = finalise(&before, DayOutcome::Missed, true);

        match outcome {
            Finalisation::HeldForRepair { visible, on_expiry } => {
                // The user still sees 12 days, not zero.
                assert_eq!(visible.current, 12);
                assert_eq!(visible.state, StreakState::Repairable);
                assert_eq!(on_expiry.streak.current, 0);
                assert_eq!(on_expiry.streak.state, StreakState::Broken);
                assert_eq!(on_expiry.streak.longest, 27);
            }
            Finalisation::Settled(_) => panic!("expected the repair window to be offered"),
        }
    }

    #[test]
    fn a_missed_day_settles_immediately_when_repair_is_spent() {
        let outcome = finalise(&seeded(12, 27, 0, 3), DayOutcome::Missed, false);

        match outcome {
            Finalisation::Settled(step) => {
                assert_eq!(step.streak.current, 0);
                assert_eq!(step.streak.state, StreakState::Broken);
            }
            Finalisation::HeldForRepair { .. } => panic!("repair was already used this month"),
        }
    }

    #[test]
    fn a_freeze_absorbs_the_miss_before_repair_is_ever_offered() {
        let outcome = finalise(&seeded(12, 27, 2, 0), DayOutcome::Missed, true);

        match outcome {
            Finalisation::Settled(step) => {
                assert_eq!(step.streak.current, 12);
                assert_eq!(step.streak.freezes, 1);
                assert_eq!(step.rewritten, Some(DayOutcome::Rest));
            }
            Finalisation::HeldForRepair { .. } => {
                panic!("a banked freeze is spent before the repair window opens")
            }
        }
    }

    /// Repair is modelled as rewriting the day and replaying, which is the whole
    /// reason `fold` has to be able to break on its own.
    #[test]
    fn repairing_a_day_restores_the_streak_by_replay() {
        let history = [DayOutcome::Complete; 5];
        let before_the_miss = fold(&history);

        let broken = fold_from(before_the_miss.clone(), &[DayOutcome::Missed]);
        assert_eq!(broken.current, 0);

        // The user repairs: the stored day becomes complete and we replay.
        let repaired = fold_from(before_the_miss, &[DayOutcome::Complete]);
        assert_eq!(repaired.current, 6);
        assert_eq!(repaired.state, StreakState::Active);
    }

    // --- at risk ------------------------------------------------------------

    #[test]
    fn at_risk_needs_a_streak_worth_protecting() {
        assert!(at_risk(&seeded(3, 3, 0, 3), 20));
        assert!(!at_risk(&seeded(2, 2, 0, 2), 20), "streak below the floor");
        assert!(!at_risk(&seeded(9, 9, 0, 2), 60), "day is already partial");

        let mut broken = seeded(0, 9, 0, 0);
        broken.state = StreakState::Broken;
        assert!(!at_risk(&broken, 0), "nothing left to lose");
    }

    // --- properties ---------------------------------------------------------

    fn any_outcome() -> impl Strategy<Value = DayOutcome> {
        prop_oneof![
            Just(DayOutcome::Complete),
            Just(DayOutcome::Partial),
            Just(DayOutcome::Missed),
            Just(DayOutcome::Rest),
        ]
    }

    proptest! {
        /// The property that makes repair, backfill, and timezone correction safe:
        /// replaying from any split point lands in the same place.
        #[test]
        fn fold_can_resume_from_any_split(
            days in prop::collection::vec(any_outcome(), 0..200),
            split in 0usize..200,
        ) {
            let split = split.min(days.len());
            let one_pass = fold(&days);
            let resumed = fold_from(fold(&days[..split]), &days[split..]);

            prop_assert_eq!(one_pass, resumed);
        }

        #[test]
        fn longest_is_never_less_than_current(days in prop::collection::vec(any_outcome(), 0..200)) {
            prop_assert!(fold(&days).longest >= fold(&days).current);
        }

        #[test]
        fn longest_never_decreases(days in prop::collection::vec(any_outcome(), 0..200)) {
            let mut streak = Streak::default();
            for day in days {
                let next = step(&streak, day).streak;
                prop_assert!(next.longest >= streak.longest);
                streak = next;
            }
        }

        #[test]
        fn freezes_stay_within_the_cap(days in prop::collection::vec(any_outcome(), 0..500)) {
            prop_assert!(fold(&days).freezes <= FREEZE_CAP);
        }

        /// Regression: `clean_run` is a `u8` that only resets when a freeze is
        /// earned or spent. At the freeze cap the earn branch stops firing, so it
        /// used to climb without bound and overflow on day 256 — reachable by any
        /// user chasing the 365-day milestone.
        #[test]
        fn long_clean_runs_do_not_overflow(len in 250usize..600) {
            let days = vec![DayOutcome::Complete; len];
            let streak = fold(&days);
            prop_assert_eq!(streak.current, len as u32);
            prop_assert!(streak.freezes <= FREEZE_CAP);
        }

        /// `clean_run` may only sit at or above the earn threshold when there is
        /// no room to bank another freeze.
        #[test]
        fn clean_run_only_exceeds_the_threshold_at_the_freeze_cap(
            days in prop::collection::vec(any_outcome(), 0..500),
        ) {
            let streak = fold(&days);
            prop_assert!(streak.clean_run < DAYS_PER_FREEZE || streak.freezes == FREEZE_CAP);
        }

        /// A day that is not missed can never reduce the current streak.
        #[test]
        fn only_a_miss_can_end_a_streak(
            days in prop::collection::vec(any_outcome(), 0..200),
        ) {
            let mut streak = Streak::default();
            for day in days {
                let next = step(&streak, day).streak;
                if day != DayOutcome::Missed {
                    prop_assert!(next.current >= streak.current);
                }
                streak = next;
            }
        }
    }
}
