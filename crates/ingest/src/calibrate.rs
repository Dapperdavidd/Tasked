//! Stage 4: calibration. Deterministic Rust, never a model call.
//!
//! The single most likely way this product dies is a generated plan that is too
//! ambitious. A model asked to turn a syllabus into a daily plan will produce a
//! nine-task Tuesday without hesitation, and the user quits on day three.
//!
//! So the intensity cap is enforced **here, in code, after generation**. The
//! prompt may ask the model to respect it; this module assumes it did not.
//!
//! Remedies are applied in the order the system design fixes:
//!
//! 1. Split tasks over 45 minutes into consecutive sub-tasks.
//! 2. Push overflow to the next day with room.
//! 3. Extend the duration, up to 1.5x the original.
//! 4. Only then flag the program as over capacity, with a warning.
//!
//! Content is never dropped to make the arithmetic work. If the load genuinely
//! does not fit, the user is told rather than quietly given a shorter program.

use crate::types::{GeneratedTask, Intensity, Warning};
use tracked_core::cadence::Cadence;

/// Tasks longer than this are split into consecutive sub-tasks.
pub const SPLIT_THRESHOLD_MINUTES: u16 = 45;
/// Hardest ceiling on duration, matching the `programs.duration_days` check.
pub const MAX_DURATION_DAYS: u16 = 730;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub duration_days: u16,
    pub tasks: Vec<GeneratedTask>,
    pub warnings: Vec<Warning>,
    /// Projected minutes per day index. The confirm screen's headline number is
    /// the sum of this, and it is the most important thing on that screen.
    pub projected_minutes: Vec<u32>,
}

impl Calibration {
    pub fn total_minutes(&self) -> u32 {
        self.projected_minutes.iter().sum()
    }
}

/// Project the daily minute load a task list implies over a duration.
///
/// Weekday-pinned cadences are projected as if the program starts on a Monday.
/// The real start date is chosen at confirm, after this runs, so there is no
/// better answer available here — and being deterministic matters more than
/// being clairvoyant, because the user is about to edit this list anyway.
///
/// Floating `n_per_week` tasks are placed on the lightest days of each ISO
/// week, which is exactly what the product lets the user do at run time.
pub fn project(tasks: &[GeneratedTask], duration_days: u16) -> Vec<u32> {
    let days = duration_days as usize;
    let mut load = vec![0u32; days];
    if days == 0 {
        return load;
    }

    for task in tasks {
        let minutes = u32::from(task.estimated_minutes);
        match &task.cadence {
            Cadence::Daily => {
                for slot in load.iter_mut() {
                    *slot = slot.saturating_add(minutes);
                }
            }
            Cadence::WeeklyDays { days: weekdays } => {
                for (index, slot) in load.iter_mut().enumerate() {
                    let weekday = (index % 7) as u8 + 1;
                    if weekdays.contains(&weekday) {
                        *slot = slot.saturating_add(minutes);
                    }
                }
            }
            Cadence::Once { day_offset } => {
                if let Some(slot) = load.get_mut(*day_offset as usize) {
                    *slot = slot.saturating_add(minutes);
                }
            }
            // Placed in the second pass, once fixed load is known.
            Cadence::NPerWeek { .. } => {}
        }
    }

    for task in tasks {
        let Cadence::NPerWeek { count } = task.cadence else {
            continue;
        };
        let minutes = u32::from(task.estimated_minutes);

        let mut week_start = 0usize;
        while week_start < days {
            let week_end = (week_start + 7).min(days);
            // Re-pick the lightest day for each session, so two sessions do not
            // both land on what was the lightest day before either was placed.
            for _ in 0..count {
                let lightest = (week_start..week_end).min_by_key(|index| load[*index]);
                if let Some(index) = lightest {
                    load[index] = load[index].saturating_add(minutes);
                }
            }
            week_start = week_end;
        }
    }

    load
}

/// Enforce the intensity cap against a generated task list.
pub fn calibrate(
    tasks: Vec<GeneratedTask>,
    duration_days: u16,
    intensity: Intensity,
) -> Calibration {
    let mut tasks = tasks;
    let mut warnings = Vec::new();

    let original_duration = duration_days.max(1);
    let mut duration = original_duration;
    let tolerated = u32::from(intensity.tolerated_minutes());
    let max_duration = ((u32::from(original_duration) * 3) / 2)
        .clamp(u32::from(original_duration), u32::from(MAX_DURATION_DAYS))
        as u16;

    // 1. Split.
    split_long_pinned_tasks(&mut tasks, &mut warnings);

    // 2 and 3. Push overflow forward, lengthening the program only when there
    // is genuinely nowhere left to push to.
    let iteration_budget = tasks
        .len()
        .saturating_mul(usize::from(max_duration))
        .saturating_add(64);

    for _ in 0..iteration_budget {
        let load = project(&tasks, duration);
        let over: Vec<usize> = load
            .iter()
            .enumerate()
            .filter(|(_, minutes)| **minutes > tolerated)
            .map(|(index, _)| index)
            .collect();

        if over.is_empty() {
            break;
        }

        // Only a pinned task can be moved. If nothing on any overloaded day is
        // movable, the overload is structural — recurring tasks that exceed the
        // cap on their own — and stretching the program cannot help.
        let movable = over
            .iter()
            .any(|day| largest_pinned_on(&tasks, *day).is_some());
        if !movable {
            break;
        }

        let relieved = over
            .iter()
            .any(|day| relieve_day(&mut tasks, &load, *day, duration, tolerated, &mut warnings));
        if relieved {
            continue;
        }

        if duration < max_duration {
            duration += 1;
            continue;
        }
        break;
    }

    if duration != original_duration {
        warnings.push(Warning::DurationExtended {
            from_days: original_duration,
            to_days: duration,
        });
    }

    // 4. Report whatever could not be fixed, rather than hiding it.
    let projected_minutes = project(&tasks, duration);
    let cap = intensity.daily_cap_minutes();

    for (index, minutes) in projected_minutes.iter().enumerate() {
        if *minutes > tolerated {
            warnings.push(Warning::DayOverCapacity {
                day_index: index as u16,
                projected_minutes: (*minutes).min(u32::from(u16::MAX)) as u16,
                cap_minutes: cap,
            });
        } else if *minutes == 0 {
            // PRD F1: every day of the stated duration carries at least one task.
            warnings.push(Warning::EmptyDay {
                day_index: index as u16,
            });
        }
    }

    Calibration {
        duration_days: duration,
        tasks,
        warnings,
        projected_minutes,
    }
}

/// Break pinned tasks longer than the threshold into consecutive sub-tasks.
///
/// Minutes are conserved exactly: a 100-minute task becomes 34 + 33 + 33, never
/// 3 x 33 with a minute quietly lost.
fn split_long_pinned_tasks(tasks: &mut Vec<GeneratedTask>, warnings: &mut Vec<Warning>) {
    let mut out: Vec<GeneratedTask> = Vec::with_capacity(tasks.len());

    for task in tasks.drain(..) {
        let Some(day) = task.pinned_day() else {
            out.push(task);
            continue;
        };
        if task.estimated_minutes <= SPLIT_THRESHOLD_MINUTES {
            out.push(task);
            continue;
        }

        let total = task.estimated_minutes;
        let parts = total.div_ceil(SPLIT_THRESHOLD_MINUTES);
        let base = total / parts;
        let remainder = total % parts;

        warnings.push(Warning::TaskSplit {
            task_index: out.len() as u16,
            parts,
        });

        for part in 0..parts {
            let minutes = if part < remainder { base + 1 } else { base };
            out.push(GeneratedTask {
                title: format!("{} ({}/{})", task.title, part + 1, parts),
                description: task.description.clone(),
                category: task.category.clone(),
                difficulty: task.difficulty,
                estimated_minutes: minutes,
                cadence: Cadence::Once {
                    day_offset: day + u32::from(part),
                },
            });
        }
    }

    *tasks = out;
}

fn largest_pinned_on(tasks: &[GeneratedTask], day: usize) -> Option<usize> {
    tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.pinned_day() == Some(day as u32))
        .max_by_key(|(_, task)| task.estimated_minutes)
        .map(|(index, _)| index)
}

/// Move the biggest movable task off an overloaded day to the earliest later
/// day that can absorb it. Returns whether anything moved.
fn relieve_day(
    tasks: &mut [GeneratedTask],
    load: &[u32],
    day: usize,
    duration: u16,
    tolerated: u32,
    warnings: &mut Vec<Warning>,
) -> bool {
    let Some(index) = largest_pinned_on(tasks, day) else {
        return false;
    };
    let minutes = u32::from(tasks[index].estimated_minutes);

    for target in (day + 1)..usize::from(duration) {
        let Some(target_load) = load.get(target) else {
            break;
        };
        if target_load.saturating_add(minutes) <= tolerated {
            tasks[index].cadence = Cadence::Once {
                day_offset: target as u32,
            };
            warnings.push(Warning::TaskMoved {
                task_index: index as u16,
                from_day: day as u16,
                to_day: target as u16,
            });
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn task(minutes: u16, cadence: Cadence) -> GeneratedTask {
        GeneratedTask {
            title: "do the thing".to_owned(),
            description: None,
            category: None,
            difficulty: 3,
            estimated_minutes: minutes,
            cadence,
        }
    }

    fn pinned(minutes: u16, day: u32) -> GeneratedTask {
        task(minutes, Cadence::Once { day_offset: day })
    }

    fn total_minutes(tasks: &[GeneratedTask]) -> u32 {
        tasks
            .iter()
            .map(|task| u32::from(task.estimated_minutes))
            .sum()
    }

    // --- projection ---------------------------------------------------------

    #[test]
    fn daily_tasks_land_on_every_day() {
        let load = project(&[task(15, Cadence::Daily)], 4);
        assert_eq!(load, vec![15, 15, 15, 15]);
    }

    #[test]
    fn weekday_tasks_project_from_a_monday_start() {
        // Monday and Wednesday, over a fortnight.
        let load = project(&[task(30, Cadence::WeeklyDays { days: vec![1, 3] })], 14);
        assert_eq!(load, vec![30, 0, 30, 0, 0, 0, 0, 30, 0, 30, 0, 0, 0, 0]);
    }

    #[test]
    fn floating_tasks_fill_the_lightest_days_of_each_week() {
        let tasks = vec![
            task(60, Cadence::Once { day_offset: 0 }),
            task(30, Cadence::NPerWeek { count: 3 }),
        ];
        let load = project(&tasks, 7);

        // Monday already carries 60, so none of the three sessions go there.
        assert_eq!(load[0], 60);
        assert_eq!(load.iter().filter(|minutes| **minutes == 30).count(), 3);
        assert_eq!(load.iter().sum::<u32>(), 60 + 90);
    }

    #[test]
    fn floating_sessions_do_not_stack_on_one_day() {
        // Regression: picking the lightest day once and reusing it puts every
        // session on the same day.
        let load = project(&[task(20, Cadence::NPerWeek { count: 3 })], 7);
        assert!(
            load.iter().all(|minutes| *minutes <= 20),
            "sessions stacked: {load:?}"
        );
    }

    // --- the four remedies, in order ----------------------------------------

    #[test]
    fn a_long_pinned_task_is_split_across_consecutive_days() {
        let result = calibrate(vec![pinned(90, 0)], 4, Intensity::Standard);

        assert_eq!(result.tasks.len(), 2);
        assert_eq!(
            total_minutes(&result.tasks),
            90,
            "minutes must be conserved"
        );
        assert!(result
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::TaskSplit { parts: 2, .. })));
    }

    #[test]
    fn splitting_conserves_minutes_that_do_not_divide_evenly() {
        let result = calibrate(vec![pinned(100, 0)], 10, Intensity::Standard);

        assert_eq!(total_minutes(&result.tasks), 100);
        assert_eq!(result.tasks.len(), 3);
        let mut minutes: Vec<u16> = result
            .tasks
            .iter()
            .map(|task| task.estimated_minutes)
            .collect();
        minutes.sort_unstable();
        assert_eq!(minutes, vec![33, 33, 34]);
    }

    #[test]
    fn overflow_is_pushed_to_a_later_day_rather_than_dropped() {
        // Three 40-minute tasks all pinned to day 0, standard cap of 45.
        let tasks = vec![pinned(40, 0), pinned(40, 0), pinned(40, 0)];
        let result = calibrate(tasks, 5, Intensity::Standard);

        assert_eq!(total_minutes(&result.tasks), 120, "nothing was dropped");
        assert!(result
            .projected_minutes
            .iter()
            .all(|minutes| *minutes <= u32::from(Intensity::Standard.tolerated_minutes())));
        assert!(result
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::TaskMoved { .. })));
    }

    #[test]
    fn the_duration_stretches_when_there_is_nowhere_to_push_to() {
        // Four 40-minute tasks but only two days to hold them.
        let tasks = vec![pinned(40, 0), pinned(40, 0), pinned(40, 1), pinned(40, 1)];
        let result = calibrate(tasks, 2, Intensity::Standard);

        assert!(result.duration_days > 2, "expected the program to stretch");
        assert!(result.duration_days <= 3, "1.5x of 2 days is 3");
        assert_eq!(total_minutes(&result.tasks), 160);
        assert!(result
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::DurationExtended { .. })));
    }

    #[test]
    fn a_structurally_overloaded_program_is_flagged_not_silently_trimmed() {
        // A 90-minute daily task cannot fit a light day by any remedy: it
        // recurs, so it cannot be moved, and stretching changes nothing.
        let result = calibrate(vec![task(90, Cadence::Daily)], 5, Intensity::Light);

        assert_eq!(total_minutes(&result.tasks), 90, "the task still exists");
        assert_eq!(result.duration_days, 5, "no pointless stretching");
        assert!(result
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::DayOverCapacity { .. })));
    }

    #[test]
    fn empty_days_inside_the_duration_are_reported() {
        let result = calibrate(vec![pinned(20, 0)], 3, Intensity::Standard);

        let empty: Vec<u16> = result
            .warnings
            .iter()
            .filter_map(|w| match w {
                Warning::EmptyDay { day_index } => Some(*day_index),
                _ => None,
            })
            .collect();
        assert_eq!(empty, vec![1, 2]);
    }

    #[test]
    fn a_program_already_within_budget_is_left_alone() {
        let tasks = vec![task(20, Cadence::Daily), pinned(15, 2)];
        let result = calibrate(tasks.clone(), 7, Intensity::Standard);

        assert_eq!(result.duration_days, 7);
        assert_eq!(result.tasks, tasks, "no gratuitous rewriting");
        assert!(!result
            .warnings
            .iter()
            .any(|w| matches!(w, Warning::DayOverCapacity { .. })));
    }

    // --- properties ---------------------------------------------------------

    fn any_task() -> impl Strategy<Value = GeneratedTask> {
        (1u16..200, 0u32..30, 0u8..4).prop_map(|(minutes, day, shape)| {
            let cadence = match shape {
                0 => Cadence::Daily,
                1 => Cadence::WeeklyDays { days: vec![1, 4] },
                2 => Cadence::NPerWeek { count: 3 },
                _ => Cadence::Once { day_offset: day },
            };
            task(minutes, cadence)
        })
    }

    proptest! {
        /// The rule the system design states outright: never silently drop a
        /// task to make the arithmetic work.
        #[test]
        fn calibration_never_loses_a_minute(
            tasks in prop::collection::vec(any_task(), 0..12),
            duration in 1u16..40,
        ) {
            let before = total_minutes(&tasks);
            let result = calibrate(tasks, duration, Intensity::Standard);
            prop_assert_eq!(total_minutes(&result.tasks), before);
        }

        #[test]
        fn the_duration_never_exceeds_one_and_a_half_times_the_original(
            tasks in prop::collection::vec(any_task(), 0..12),
            duration in 1u16..40,
        ) {
            let result = calibrate(tasks, duration, Intensity::Standard);
            prop_assert!(result.duration_days >= duration);
            prop_assert!(u32::from(result.duration_days) <= (u32::from(duration) * 3) / 2 + 1);
        }

        /// PRD F1 acceptance: either every day is within 20% of the cap, or the
        /// user is warned about the ones that are not. Never silence.
        #[test]
        fn every_over_capacity_day_is_warned_about(
            tasks in prop::collection::vec(any_task(), 0..12),
            duration in 1u16..40,
        ) {
            let intensity = Intensity::Standard;
            let result = calibrate(tasks, duration, intensity);
            let tolerated = u32::from(intensity.tolerated_minutes());

            for (index, minutes) in result.projected_minutes.iter().enumerate() {
                if *minutes > tolerated {
                    prop_assert!(
                        result.warnings.iter().any(|w| matches!(
                            w,
                            Warning::DayOverCapacity { day_index, .. } if *day_index == index as u16
                        )),
                        "day {} at {} minutes was not reported", index, minutes
                    );
                }
            }
        }

        #[test]
        fn calibration_terminates_and_stays_consistent(
            tasks in prop::collection::vec(any_task(), 0..12),
            duration in 1u16..40,
        ) {
            let result = calibrate(tasks, duration, Intensity::Heavy);
            prop_assert_eq!(result.projected_minutes.len(), usize::from(result.duration_days));
        }
    }
}
