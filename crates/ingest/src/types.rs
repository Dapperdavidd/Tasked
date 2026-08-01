//! The typed contract between the model and the rest of the system.
//!
//! The model never writes to the database. It produces a [`GeneratedProgram`],
//! which is validated, calibrated in deterministic code, shown to the user for
//! editing, and only then confirmed into `programs` and `task_templates`.

use serde::{Deserialize, Serialize};
use tracked_core::cadence::Cadence;

/// How much of the user's day the program is allowed to claim, per PRD F1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intensity {
    Light,
    Standard,
    Heavy,
}

impl Intensity {
    /// The daily budget in minutes. Chosen by the user before generation.
    pub const fn daily_cap_minutes(self) -> u16 {
        match self {
            Self::Light => 20,
            Self::Standard => 45,
            Self::Heavy => 90,
        }
    }

    /// The hard ceiling a calibrated day may reach.
    ///
    /// PRD F1 accepts up to 20% over the cap; past that the program is flagged
    /// rather than quietly overloaded. Rounding is deliberate: a light day may
    /// reach 24 minutes, not 24.0.
    pub const fn tolerated_minutes(self) -> u16 {
        self.daily_cap_minutes() * 12 / 10
    }
}

/// The three shapes a source can take, per PRD F1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramKind {
    /// Sequential: task X belongs to day N.
    Curriculum,
    /// Recurring: tasks repeat on a cadence.
    Routine,
    /// Deliverables with a deadline, distributed across the available days.
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedTask {
    /// Imperative, starts with a verb, 80 characters or fewer.
    pub title: String,
    pub description: Option<String>,
    pub category: Option<String>,
    /// 1 to 5.
    pub difficulty: u8,
    /// Estimated for a beginner in the domain, not an expert.
    pub estimated_minutes: u16,
    pub cadence: Cadence,
}

impl GeneratedTask {
    /// The day offsets this task lands on, given a program duration.
    ///
    /// Only `Once` is pinned to a specific day; the rest are decided by cadence
    /// at materialisation time.
    pub fn pinned_day(&self) -> Option<u32> {
        match self.cadence {
            Cadence::Once { day_offset } => Some(day_offset),
            _ => None,
        }
    }
}

// No `Eq`: `confidence` is a float, and an equality that silently compares
// floats is worse than not having one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeneratedProgram {
    pub title: String,
    pub summary: String,
    pub kind: ProgramKind,
    pub duration_days: u16,
    pub confidence: f32,
    pub tasks: Vec<GeneratedTask>,
    pub warnings: Vec<Warning>,
}

/// Something the pipeline guessed at, changed, or could not fix.
///
/// Warnings carry a **task index, never a task title**. They travel through
/// logs, metrics, and error reporting, and task content is never allowed in any
/// of those. The client resolves the index against the draft it already holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    /// A day is still over the tolerated ceiling after every remedy was tried.
    /// The program is not silently overloaded; the user is told.
    DayOverCapacity {
        day_index: u16,
        projected_minutes: u16,
        cap_minutes: u16,
    },
    /// The duration was stretched to fit the load under the cap.
    DurationExtended { from_days: u16, to_days: u16 },
    /// A long task was broken into consecutive sub-tasks.
    TaskSplit { task_index: u16, parts: u16 },
    /// A task was pushed to a later day to relieve an overloaded one.
    TaskMoved {
        task_index: u16,
        from_day: u16,
        to_day: u16,
    },
    /// A day inside the duration ended up with nothing on it. PRD F1 requires
    /// every day to carry at least one task.
    EmptyDay { day_index: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intensity_caps_match_the_product_spec() {
        assert_eq!(Intensity::Light.daily_cap_minutes(), 20);
        assert_eq!(Intensity::Standard.daily_cap_minutes(), 45);
        assert_eq!(Intensity::Heavy.daily_cap_minutes(), 90);
    }

    #[test]
    fn tolerance_is_twenty_percent_over_the_cap() {
        assert_eq!(Intensity::Light.tolerated_minutes(), 24);
        assert_eq!(Intensity::Standard.tolerated_minutes(), 54);
        assert_eq!(Intensity::Heavy.tolerated_minutes(), 108);
    }

    #[test]
    fn warnings_carry_no_task_content() {
        // Guard against someone "helpfully" adding a title field later: this
        // structure is allowed in logs, and task content never is.
        let warning = Warning::TaskSplit {
            task_index: 3,
            parts: 2,
        };
        let encoded = serde_json::to_string(&warning).expect("warning serialises");
        assert_eq!(encoded, r#"{"kind":"task_split","task_index":3,"parts":2}"#);
    }
}
