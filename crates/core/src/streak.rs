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
    AtRisk,
    Repairable,
    Broken,
}

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

pub fn step(s: &Streak, outcome: DayOutcome) -> (Streak, Option<DayOutcome>) {
    let mut next = s.clone();
    let mut rewritten = None;

    match outcome {
        DayOutcome::Rest => {}
        DayOutcome::Complete => {
            next.current += 1;
            next.longest = next.longest.max(next.current);
            next.clean_run += 1;
            next.state = StreakState::Active;
        }
        DayOutcome::Partial => {
            next.clean_run += 1;
            next.state = StreakState::AtRisk;
        }
        DayOutcome::Missed => {
            if next.freezes > 0 {
                next.freezes -= 1;
                next.clean_run = 0;
                next.state = StreakState::Active;
                rewritten = Some(DayOutcome::Rest);
            } else {
                next.state = StreakState::Repairable;
            }
        }
    }

    if next.clean_run >= 7 && next.freezes < 3 {
        next.freezes += 1;
        next.clean_run = 0;
    }

    (next, rewritten)
}

pub fn fold(days: &[DayOutcome]) -> Streak {
    days.iter()
        .fold(Streak::default(), |acc, day| step(&acc, *day).0)
}

pub fn fold_from(seed: Streak, days: &[DayOutcome]) -> Streak {
    days.iter().fold(seed, |acc, day| step(&acc, *day).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
    fn miss_consumes_freeze_and_rewrites_to_rest() {
        let seeded = Streak {
            current: 7,
            longest: 7,
            freezes: 1,
            clean_run: 0,
            state: StreakState::Active,
        };

        let (next, rewritten) = step(&seeded, DayOutcome::Missed);
        assert_eq!(next.current, 7);
        assert_eq!(next.freezes, 0);
        assert_eq!(rewritten, Some(DayOutcome::Rest));
    }

    #[test]
    fn miss_without_freeze_becomes_repairable_without_resetting_longest() {
        let streak = fold(&[DayOutcome::Complete, DayOutcome::Missed]);
        assert_eq!(streak.current, 1);
        assert_eq!(streak.longest, 1);
        assert_eq!(streak.state, StreakState::Repairable);
    }

    proptest! {
        #[test]
        fn fold_can_resume_from_any_split(days in prop::collection::vec(any_outcome(), 0..200), split in 0usize..200) {
            let split = split.min(days.len());
            let prefix = &days[..split];
            let suffix = &days[split..];

            let one_pass = fold(&days);
            let resumed = fold_from(fold(prefix), suffix);

            prop_assert_eq!(one_pass, resumed);
        }

        #[test]
        fn longest_is_never_less_than_current(days in prop::collection::vec(any_outcome(), 0..200)) {
            let streak = fold(&days);
            prop_assert!(streak.longest >= streak.current);
        }
    }

    fn any_outcome() -> impl Strategy<Value = DayOutcome> {
        prop_oneof![
            Just(DayOutcome::Complete),
            Just(DayOutcome::Partial),
            Just(DayOutcome::Missed),
            Just(DayOutcome::Rest),
        ]
    }
}
