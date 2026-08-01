//! The completion artifact: what a finished bounded program leaves behind.
//!
//! PRD F10. Composed here so the numbers on it are computed once, from the same
//! day rows everything else reads, rather than assembled by whatever renders
//! the PNG and the PDF.
//!
//! Two rules are load bearing and both are enforced by the type system or by
//! [`compile`] refusing:
//!
//! - **The standing list never produces an artifact.** It has no duration, no
//!   finish line, and nothing to commemorate. An artifact is a record of a
//!   finished commitment.
//! - **Standing data never contributes to a bounded program's numbers.** The
//!   caller passes one enrollment's days; there is no signature here that
//!   accepts two.

use chrono::NaiveDate;

use crate::stats::{completion_rate, DayTotals};

/// How one day is drawn on the heatmap.
///
/// Rest and frozen are distinct from each other and from everything else,
/// because the record has to stay honest: a day off that was declared and a day
/// that a banked freeze absorbed are different facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Complete,
    Partial,
    Missed,
    Rest,
    Frozen,
}

impl Cell {
    /// Whether the day counts toward the trailing completion figures.
    ///
    /// Only a declared rest day leaves the denominator. A frozen day was still
    /// a day the work did not happen; the freeze protected the streak, not the
    /// completion rate.
    pub fn counts_toward_completion(self) -> bool {
        !matches!(self, Self::Rest)
    }
}

/// One finalised day, as the artifact needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaySummary {
    pub local_date: NaiveDate,
    pub cell: Cell,
    pub earned_points: i32,
    pub available_points: i32,
    pub tasks_completed: u32,
    pub minutes_invested: u32,
    /// The user's own line about the day. Compiled into the artifact verbatim
    /// and never logged anywhere.
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    /// The standing list has no finish line, so it has nothing to commemorate.
    #[error("the standing list does not produce a completion artifact")]
    StandingEnrollment,
    #[error("a completed program must have at least one finalised day")]
    NoDays,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionArtifact {
    pub title: String,
    pub started_on: NaiveDate,
    pub finished_on: NaiveDate,
    pub days_total: u32,
    /// Days on which anything at all was completed.
    pub days_logged: u32,
    /// Earned over available across the run, rest days excluded. `None` when
    /// there was never anything available, which is a dash rather than a zero.
    pub completion_rate: Option<u8>,
    pub longest_streak: u32,
    pub tasks_completed: u32,
    pub hours_invested: u32,
    /// One cell per day, in date order, for the heatmap.
    pub cells: Vec<(NaiveDate, Cell)>,
    /// The user's daily notes, in date order, blank days omitted.
    pub notes: Vec<(NaiveDate, String)>,
}

/// Compile a finished bounded program into its artifact.
///
/// `days` must be one enrollment's finalised days. `longest_streak` comes from
/// that enrollment's own streak state — the standing streak is a different
/// number about a different commitment and never appears here.
pub fn compile(
    title: &str,
    is_standing: bool,
    days: &[DaySummary],
    longest_streak: u32,
) -> Result<CompletionArtifact, ArtifactError> {
    if is_standing {
        return Err(ArtifactError::StandingEnrollment);
    }
    if days.is_empty() {
        return Err(ArtifactError::NoDays);
    }

    let mut ordered: Vec<&DaySummary> = days.iter().collect();
    ordered.sort_by_key(|day| day.local_date);

    let totals: Vec<DayTotals> = ordered
        .iter()
        .map(|day| {
            if day.cell.counts_toward_completion() {
                DayTotals::new(day.earned_points, day.available_points)
            } else {
                DayTotals::rest()
            }
        })
        .collect();

    let (earned, available) = crate::stats::totals(&totals);

    let minutes: u32 = ordered
        .iter()
        .map(|day| day.minutes_invested)
        .fold(0u32, |sum, value| sum.saturating_add(value));

    Ok(CompletionArtifact {
        title: title.to_owned(),
        // Unwrap-free: `ordered` is non-empty and sorted.
        started_on: ordered[0].local_date,
        finished_on: ordered[ordered.len() - 1].local_date,
        days_total: ordered.len() as u32,
        days_logged: ordered.iter().filter(|day| day.tasks_completed > 0).count() as u32,
        completion_rate: completion_rate(earned, available),
        longest_streak,
        tasks_completed: ordered
            .iter()
            .map(|day| day.tasks_completed)
            .fold(0u32, |sum, value| sum.saturating_add(value)),
        // Rounded to the nearest hour: "37 hours invested" is the point, and a
        // decimal would invite arguing with an estimate.
        hours_invested: (minutes + 30) / 60,
        cells: ordered
            .iter()
            .map(|day| (day.local_date, day.cell))
            .collect(),
        notes: ordered
            .iter()
            .filter_map(|day| {
                day.note
                    .as_ref()
                    .map(|note| note.trim())
                    .filter(|note| !note.is_empty())
                    .map(|note| (day.local_date, note.to_owned()))
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, day).expect("test date is valid")
    }

    fn day(day_of_month: u32, cell: Cell, earned: i32, available: i32) -> DaySummary {
        DaySummary {
            local_date: date(day_of_month),
            cell,
            earned_points: earned,
            available_points: available,
            tasks_completed: if earned > 0 { 2 } else { 0 },
            minutes_invested: if earned > 0 { 45 } else { 0 },
            note: None,
        }
    }

    #[test]
    fn the_standing_list_never_produces_an_artifact() {
        let days = [day(1, Cell::Complete, 100, 100)];
        assert_eq!(
            compile("Standing", true, &days, 41),
            Err(ArtifactError::StandingEnrollment)
        );
    }

    #[test]
    fn an_empty_program_produces_nothing_rather_than_an_empty_artifact() {
        assert_eq!(compile("P", false, &[], 0), Err(ArtifactError::NoDays));
    }

    #[test]
    fn composes_the_headline_numbers() {
        let days = [
            day(1, Cell::Complete, 100, 100),
            day(2, Cell::Partial, 60, 100),
            day(3, Cell::Missed, 0, 100),
        ];

        let artifact = compile("8 Week 5K Plan", false, &days, 27).expect("compiles");

        assert_eq!(artifact.started_on, date(1));
        assert_eq!(artifact.finished_on, date(3));
        assert_eq!(artifact.days_total, 3);
        assert_eq!(artifact.days_logged, 2);
        assert_eq!(artifact.completion_rate, Some(53)); // 160/300
        assert_eq!(artifact.longest_streak, 27);
        assert_eq!(artifact.tasks_completed, 4);
        assert_eq!(artifact.hours_invested, 2); // 90 minutes
    }

    #[test]
    fn declared_rest_days_leave_the_denominator_but_frozen_days_do_not() {
        let rested = [day(1, Cell::Complete, 100, 100), day(2, Cell::Rest, 0, 0)];
        let frozen = [
            day(1, Cell::Complete, 100, 100),
            day(2, Cell::Frozen, 0, 100),
        ];

        assert_eq!(
            compile("P", false, &rested, 1)
                .expect("compiles")
                .completion_rate,
            Some(100)
        );
        // The freeze saved the streak. It does not launder the missed day.
        assert_eq!(
            compile("P", false, &frozen, 1)
                .expect("compiles")
                .completion_rate,
            Some(50)
        );
    }

    #[test]
    fn days_are_ordered_by_date_whatever_order_they_arrive_in() {
        let days = [
            day(3, Cell::Complete, 100, 100),
            day(1, Cell::Complete, 100, 100),
            day(2, Cell::Missed, 0, 100),
        ];

        let artifact = compile("P", false, &days, 2).expect("compiles");
        let dates: Vec<NaiveDate> = artifact.cells.iter().map(|(date, _)| *date).collect();
        assert_eq!(dates, vec![date(1), date(2), date(3)]);
        assert_eq!(artifact.started_on, date(1));
        assert_eq!(artifact.finished_on, date(3));
    }

    #[test]
    fn notes_compile_in_order_and_blank_ones_are_omitted() {
        let mut days = [
            day(1, Cell::Complete, 100, 100),
            day(2, Cell::Complete, 100, 100),
            day(3, Cell::Complete, 100, 100),
        ];
        days[0].note = Some("Felt strong.".to_owned());
        days[1].note = Some("   ".to_owned());
        days[2].note = Some("  Hard, but done.  ".to_owned());

        let artifact = compile("P", false, &days, 3).expect("compiles");

        assert_eq!(
            artifact.notes,
            vec![
                (date(1), "Felt strong.".to_owned()),
                (date(3), "Hard, but done.".to_owned()),
            ]
        );
    }

    #[test]
    fn the_heatmap_keeps_every_state_distinguishable() {
        let days = [
            day(1, Cell::Complete, 100, 100),
            day(2, Cell::Partial, 60, 100),
            day(3, Cell::Missed, 0, 100),
            day(4, Cell::Rest, 0, 0),
            day(5, Cell::Frozen, 0, 100),
        ];

        let artifact = compile("P", false, &days, 1).expect("compiles");
        let cells: Vec<Cell> = artifact.cells.iter().map(|(_, cell)| *cell).collect();

        assert_eq!(
            cells,
            vec![
                Cell::Complete,
                Cell::Partial,
                Cell::Missed,
                Cell::Rest,
                Cell::Frozen
            ],
            "the record must stay honest about what each day was"
        );
    }

    #[test]
    fn a_program_with_nothing_available_reports_a_dash_not_a_zero() {
        let days = [day(1, Cell::Rest, 0, 0)];
        assert_eq!(
            compile("P", false, &days, 0)
                .expect("compiles")
                .completion_rate,
            None
        );
    }
}
