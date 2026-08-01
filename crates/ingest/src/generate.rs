//! Stage 3: generate, and the validation that stands between the model and the
//! database.
//!
//! The response type is a Rust struct, the schema sent to the model is derived
//! from it, and the response is validated against that schema before it is
//! deserialised. What is left after that is *domain* validation, which a schema
//! cannot express: difficulty is 1 to 5, a `once` task cannot land past the end
//! of the program, an unknown cadence never reaches `task_templates`.
//!
//! The governing rule is that a model mistake is repaired and reported, never
//! silently dropped and never silently trusted. Anything that cannot be
//! repaired without inventing content is a hard failure.
//!
//! Long sources are chunked by [`crate::source::chunk`], generated per chunk,
//! and reduced here by [`merge`]. A single call over an eighty-page syllabus
//! drops the second half.

use crate::classify::strip_code_fence;
use crate::types::{ClampedField, GeneratedProgram, GeneratedTask, ProgramKind, Warning};
use tracked_core::cadence::{Cadence, CadenceError};

pub const MAX_DURATION_DAYS: u16 = 730;
pub const MAX_ESTIMATED_MINUTES: u16 = 480;
pub const MIN_ESTIMATED_MINUTES: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GenerateError {
    #[error("generated response was malformed: {0}")]
    Malformed(String),
    /// Repairing this would mean inventing the user's content.
    #[error("generated program contained no tasks")]
    NoTasks,
    #[error("generated task {task_index} had an empty title")]
    EmptyTitle { task_index: u16 },
    /// Never accept an unvalidated cadence. Discovering it in the materialiser
    /// means a user's day silently has no tasks in it.
    #[error("generated task {task_index} had an invalid cadence")]
    InvalidCadence {
        task_index: u16,
        // Not named `source`: thiserror would treat it as an error source and
        // require `CadenceError: std::error::Error`, which core deliberately
        // does not implement.
        cause: CadenceError,
    },
    #[error("nothing to merge")]
    NothingToMerge,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Validated {
    pub program: GeneratedProgram,
    pub warnings: Vec<Warning>,
}

/// Parse raw model output into a generated program.
pub fn parse(raw: &str) -> Result<GeneratedProgram, GenerateError> {
    serde_json::from_str(strip_code_fence(raw))
        .map_err(|error| GenerateError::Malformed(error.to_string()))
}

/// Pull a generated program into the ranges the schema and the domain allow.
///
/// Out-of-range numbers are clamped and reported. Missing content is a failure,
/// because the alternative is making it up.
pub fn validate(program: GeneratedProgram) -> Result<Validated, GenerateError> {
    let mut program = program;
    let mut warnings = Vec::new();

    if program.tasks.is_empty() {
        return Err(GenerateError::NoTasks);
    }

    if !program.confidence.is_finite() || !(0.0..=1.0).contains(&program.confidence) {
        program.confidence = program.confidence.clamp(0.0, 1.0);
        if !program.confidence.is_finite() {
            program.confidence = 0.0;
        }
        warnings.push(Warning::ValueClamped {
            task_index: None,
            field: ClampedField::Confidence,
        });
    }

    let clamped_duration = program.duration_days.clamp(1, MAX_DURATION_DAYS);
    if clamped_duration != program.duration_days {
        program.duration_days = clamped_duration;
        warnings.push(Warning::ValueClamped {
            task_index: None,
            field: ClampedField::DurationDays,
        });
    }

    for (index, task) in program.tasks.iter_mut().enumerate() {
        let task_index = index as u16;

        task.title = task.title.trim().to_owned();
        if task.title.is_empty() {
            return Err(GenerateError::EmptyTitle { task_index });
        }

        let clamped = task.difficulty.clamp(1, 5);
        if clamped != task.difficulty {
            task.difficulty = clamped;
            warnings.push(Warning::ValueClamped {
                task_index: Some(task_index),
                field: ClampedField::Difficulty,
            });
        }

        let clamped = task
            .estimated_minutes
            .clamp(MIN_ESTIMATED_MINUTES, MAX_ESTIMATED_MINUTES);
        if clamped != task.estimated_minutes {
            task.estimated_minutes = clamped;
            warnings.push(Warning::ValueClamped {
                task_index: Some(task_index),
                field: ClampedField::EstimatedMinutes,
            });
        }

        // Ingestion produces bounded programs only, so `once` is always legal
        // and standing rules never apply here.
        task.cadence
            .validate_for_program(false)
            .map_err(|cause| GenerateError::InvalidCadence { task_index, cause })?;

        // A task pinned past the end of the program would never materialise.
        // Pull it onto the last day rather than dropping it.
        if let Cadence::Once { day_offset } = task.cadence {
            let last = u32::from(program.duration_days.saturating_sub(1));
            if day_offset > last {
                task.cadence = Cadence::Once { day_offset: last };
                warnings.push(Warning::ValueClamped {
                    task_index: Some(task_index),
                    field: ClampedField::DayOffset,
                });
            }
        }
    }

    warnings.extend(program.warnings.iter().copied());
    program.warnings = warnings.clone();

    Ok(Validated { program, warnings })
}

/// Reduce per-chunk generations into one program.
///
/// Sequential shapes — a curriculum or a project — continue across chunks, so
/// each chunk's pinned days are shifted past the ones before it and the
/// durations add up. A routine repeats, so chunks overlap instead: durations
/// take the maximum and identical recurring tasks are collapsed rather than
/// appearing once per chunk.
pub fn merge(chunks: Vec<GeneratedProgram>) -> Result<GeneratedProgram, GenerateError> {
    let mut chunks = chunks.into_iter();
    let mut merged = chunks.next().ok_or(GenerateError::NothingToMerge)?;

    let sequential = matches!(merged.kind, ProgramKind::Curriculum | ProgramKind::Project);
    let mut day_offset_base = if sequential {
        u32::from(merged.duration_days)
    } else {
        0
    };

    for chunk in chunks {
        for mut task in chunk.tasks {
            if sequential {
                if let Cadence::Once { day_offset } = task.cadence {
                    task.cadence = Cadence::Once {
                        day_offset: day_offset.saturating_add(day_offset_base),
                    };
                }
            } else if is_recurring(&task.cadence) && already_present(&merged.tasks, &task) {
                // The same weekly session described in two chunks is one
                // session, not two.
                continue;
            }
            merged.tasks.push(task);
        }

        merged.warnings.extend(chunk.warnings);
        merged.confidence = merged.confidence.min(chunk.confidence);

        if sequential {
            merged.duration_days = merged.duration_days.saturating_add(chunk.duration_days);
            day_offset_base = day_offset_base.saturating_add(u32::from(chunk.duration_days));
        } else {
            merged.duration_days = merged.duration_days.max(chunk.duration_days);
        }
    }

    merged.duration_days = merged.duration_days.clamp(1, MAX_DURATION_DAYS);
    Ok(merged)
}

fn is_recurring(cadence: &Cadence) -> bool {
    !matches!(cadence, Cadence::Once { .. })
}

fn already_present(tasks: &[GeneratedTask], candidate: &GeneratedTask) -> bool {
    tasks
        .iter()
        .any(|task| task.title == candidate.title && task.cadence == candidate.cadence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(title: &str, cadence: Cadence) -> GeneratedTask {
        GeneratedTask {
            title: title.to_owned(),
            description: None,
            category: None,
            difficulty: 3,
            estimated_minutes: 30,
            cadence,
        }
    }

    fn program(
        kind: ProgramKind,
        duration_days: u16,
        tasks: Vec<GeneratedTask>,
    ) -> GeneratedProgram {
        GeneratedProgram {
            title: "8 week plan".to_owned(),
            summary: "a plan".to_owned(),
            kind,
            duration_days,
            confidence: 0.9,
            tasks,
            warnings: Vec::new(),
        }
    }

    // --- validation ---------------------------------------------------------

    #[test]
    fn a_clean_program_passes_untouched() {
        let input = program(
            ProgramKind::Routine,
            30,
            vec![task("Run 5k", Cadence::Daily)],
        );
        let result = validate(input.clone()).expect("valid");

        assert!(result.warnings.is_empty());
        assert_eq!(result.program.tasks, input.tasks);
    }

    #[test]
    fn out_of_range_numbers_are_clamped_and_reported() {
        let mut input = program(ProgramKind::Routine, 0, vec![task("Run", Cadence::Daily)]);
        input.tasks[0].difficulty = 9;
        input.tasks[0].estimated_minutes = 9_000;
        input.confidence = 4.2;

        let result = validate(input).expect("repairable");

        assert_eq!(result.program.tasks[0].difficulty, 5);
        assert_eq!(
            result.program.tasks[0].estimated_minutes,
            MAX_ESTIMATED_MINUTES
        );
        assert_eq!(result.program.duration_days, 1);
        assert_eq!(result.program.confidence, 1.0);

        for field in [
            ClampedField::Difficulty,
            ClampedField::EstimatedMinutes,
            ClampedField::DurationDays,
            ClampedField::Confidence,
        ] {
            assert!(
                result
                    .warnings
                    .iter()
                    .any(|w| matches!(w, Warning::ValueClamped { field: f, .. } if *f == field)),
                "{field:?} was clamped without telling anyone"
            );
        }
    }

    #[test]
    fn a_task_pinned_past_the_end_is_pulled_back_not_dropped() {
        let input = program(
            ProgramKind::Curriculum,
            10,
            vec![task("Read chapter 40", Cadence::Once { day_offset: 400 })],
        );
        let result = validate(input).expect("repairable");

        assert_eq!(result.program.tasks.len(), 1, "the task still exists");
        assert_eq!(
            result.program.tasks[0].cadence,
            Cadence::Once { day_offset: 9 }
        );
    }

    #[test]
    fn an_invalid_cadence_is_refused_outright() {
        let input = program(
            ProgramKind::Routine,
            10,
            vec![task("Run", Cadence::WeeklyDays { days: vec![9] })],
        );

        assert!(matches!(
            validate(input),
            Err(GenerateError::InvalidCadence { task_index: 0, .. })
        ));
    }

    #[test]
    fn missing_content_is_a_failure_rather_than_an_invention() {
        assert_eq!(
            validate(program(ProgramKind::Routine, 10, vec![])),
            Err(GenerateError::NoTasks)
        );

        let blank = program(ProgramKind::Routine, 10, vec![task("   ", Cadence::Daily)]);
        assert_eq!(
            validate(blank),
            Err(GenerateError::EmptyTitle { task_index: 0 })
        );
    }

    #[test]
    fn parses_model_output_through_a_code_fence() {
        let raw = r#"```json
        {"title":"P","summary":"s","kind":"routine","duration_days":7,
         "confidence":0.9,"tasks":[{"title":"Run","description":null,"category":null,
         "difficulty":2,"estimated_minutes":30,"cadence":{"type":"daily"}}],"warnings":[]}
        ```"#;

        let parsed = parse(raw).expect("parses");
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].cadence, Cadence::Daily);
    }

    // --- reduce -------------------------------------------------------------

    #[test]
    fn a_chunked_curriculum_continues_rather_than_overlapping() {
        // Two halves of a syllabus, each generated independently with its own
        // day offsets starting at zero.
        let first = program(
            ProgramKind::Curriculum,
            10,
            vec![task("Read chapter 1", Cadence::Once { day_offset: 0 })],
        );
        let second = program(
            ProgramKind::Curriculum,
            10,
            vec![task("Read chapter 11", Cadence::Once { day_offset: 0 })],
        );

        let merged = merge(vec![first, second]).expect("merges");

        assert_eq!(merged.duration_days, 20);
        assert_eq!(
            merged.tasks[1].cadence,
            Cadence::Once { day_offset: 10 },
            "the second half must not land on top of the first"
        );
    }

    #[test]
    fn a_chunked_routine_overlaps_and_collapses_duplicates() {
        let first = program(
            ProgramKind::Routine,
            30,
            vec![
                task("Run 5k", Cadence::Daily),
                task("Stretch", Cadence::Daily),
            ],
        );
        let second = program(
            ProgramKind::Routine,
            14,
            vec![
                task("Run 5k", Cadence::Daily),
                task("Swim", Cadence::NPerWeek { count: 2 }),
            ],
        );

        let merged = merge(vec![first, second]).expect("merges");

        assert_eq!(
            merged.duration_days, 30,
            "a routine repeats, it does not extend"
        );
        let titles: Vec<&str> = merged.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Run 5k", "Stretch", "Swim"]);
    }

    #[test]
    fn merging_takes_the_least_confident_chunk() {
        let mut first = program(ProgramKind::Routine, 7, vec![task("Run", Cadence::Daily)]);
        first.confidence = 0.95;
        let mut second = program(ProgramKind::Routine, 7, vec![task("Swim", Cadence::Daily)]);
        second.confidence = 0.4;

        let merged = merge(vec![first, second]).expect("merges");
        assert_eq!(
            merged.confidence, 0.4,
            "confidence is as weak as its weakest chunk"
        );
    }

    #[test]
    fn merging_nothing_is_an_error_not_an_empty_program() {
        assert_eq!(merge(Vec::new()), Err(GenerateError::NothingToMerge));
    }

    /// End to end over the deterministic half of the pipeline.
    #[test]
    fn chunked_output_survives_merge_validate_and_calibrate() {
        let chunks = (0..4)
            .map(|n| {
                program(
                    ProgramKind::Curriculum,
                    5,
                    vec![task(
                        &format!("Study unit {n}"),
                        Cadence::Once { day_offset: 0 },
                    )],
                )
            })
            .collect();

        let merged = merge(chunks).expect("merges");
        let validated = validate(merged).expect("validates");
        let calibrated = crate::calibrate(
            validated.program.tasks,
            validated.program.duration_days,
            crate::Intensity::Standard,
        );

        assert_eq!(calibrated.tasks.len(), 4, "no unit was lost");
        assert_eq!(calibrated.total_minutes(), 4 * 30);
    }
}
