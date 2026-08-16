//! Deterministic goal compiler.
//!
//! A source is context, not a task list. This module turns the useful units in
//! that context into an ordered sequence of small actions before cadence and
//! capacity calibration run. It deliberately has no network or model
//! dependency, so a plan is still useful when no paid AI provider is enabled.

use crate::types::{GeneratedProgram, GeneratedTask, Intensity, ProgramKind};
use tracked_core::cadence::Cadence;

const ACTIONS: [(&str, &str, u16); 3] = [
    (
        "Read and outline",
        "Capture the key ideas and vocabulary",
        12,
    ),
    ("Practice", "Complete one small example or exercise", 18),
    (
        "Check",
        "Explain the result and record one open question",
        10,
    ),
];

/// Compile source context into executable work.
pub fn compile(
    text: &str,
    instruction: Option<&str>,
    kind: ProgramKind,
    confidence: f32,
    duration_hint: Option<u16>,
    intensity: Intensity,
) -> GeneratedProgram {
    let topics = extract_topics(text);
    let topics = if topics.is_empty() {
        fallback_topics(text, instruction)
    } else {
        topics
    };
    let duration_days = duration_hint
        .unwrap_or_else(|| u16::try_from(topics.len()).unwrap_or(28))
        .clamp(1, 730);
    let topic_count = topics.len().max(1);
    let mut tasks = Vec::with_capacity(usize::from(duration_days) * ACTIONS.len());

    for day in 0..duration_days {
        let index = usize::from(day) * topic_count / usize::from(duration_days);
        let topic = &topics[index.min(topic_count - 1)];
        for (action, done, minutes) in ACTIONS {
            let scaled = scale_minutes(minutes, intensity);
            tasks.push(GeneratedTask {
                title: format!("{action} {topic}"),
                description: Some(format!(
                    "Why: move toward {topic}. Done when: {done} for {topic}. Source: {topic}."
                )),
                category: Some(category_for(topic)),
                difficulty: difficulty_for(index, topic_count),
                estimated_minutes: scaled,
                cadence: Cadence::Once {
                    day_offset: u32::from(day),
                },
            });
        }
    }

    GeneratedProgram {
        title: title_for(text, instruction, kind),
        summary: instruction
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("A dependency-ordered plan for {}.", topics.join(", "))),
        kind,
        duration_days,
        confidence,
        tasks,
        warnings: Vec::new(),
    }
}

fn extract_topics(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let lower = line.to_lowercase();
            let is_heading = lower.starts_with("day ")
                || lower.starts_with("week ")
                || lower.starts_with("module ")
                || lower.starts_with("chapter ")
                || line.starts_with('-')
                || line.starts_with('*')
                || line.starts_with('•')
                || is_numbered(line);
            if !is_heading {
                return None;
            }
            let topic = line
                .split_once(':')
                .map(|(_, value)| value)
                .unwrap_or(line)
                .trim()
                .trim_start_matches(['-', '*', '•', ' ']);
            let topic = strip_number(topic).trim();
            (!topic.is_empty()).then(|| compact(topic))
        })
        .filter(|topic| !is_schedule_label(topic))
        .collect()
}

fn fallback_topics(text: &str, instruction: Option<&str>) -> Vec<String> {
    let goal = instruction
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| text.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or("your goal");
    let goal = compact(goal);
    vec![
        format!("Define success for {goal}"),
        format!("Prepare the foundations for {goal}"),
        format!("Complete a first practical attempt at {goal}"),
        format!("Review evidence and improve {goal}"),
    ]
}

fn strip_number(value: &str) -> &str {
    let trimmed = value.trim_start();
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && matches!(trimmed.chars().nth(digits), Some('.') | Some(')')) {
        trimmed[digits + 1..].trim_start()
    } else {
        trimmed
    }
}

fn is_numbered(value: &str) -> bool {
    let trimmed = value.trim_start();
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && matches!(trimmed.chars().nth(digits), Some('.') | Some(')'))
}

fn is_schedule_label(value: &str) -> bool {
    let lower = value.to_lowercase();
    ["day", "week", "module", "chapter", "lesson"]
        .iter()
        .any(|label| lower == *label)
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn scale_minutes(minutes: u16, intensity: Intensity) -> u16 {
    match intensity {
        Intensity::Light => minutes.saturating_mul(3) / 4,
        Intensity::Standard => minutes,
        Intensity::Heavy => minutes.saturating_mul(5) / 4,
    }
    .clamp(5, 480)
}

fn difficulty_for(index: usize, total: usize) -> u8 {
    if index + 1 == total {
        3
    } else if index > total / 2 {
        2
    } else {
        1
    }
}

fn category_for(topic: &str) -> String {
    let lower = topic.to_lowercase();
    if lower.contains("build") || lower.contains("practical") {
        "build".to_owned()
    } else if lower.contains("review") || lower.contains("check") {
        "review".to_owned()
    } else {
        "core".to_owned()
    }
}

fn title_for(text: &str, instruction: Option<&str>, kind: ProgramKind) -> String {
    instruction
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(compact)
        .or_else(|| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .map(compact)
        })
        .unwrap_or_else(|| {
            match kind {
                ProgramKind::Curriculum => "Curriculum",
                ProgramKind::Routine => "Routine",
                ProgramKind::Project => "Project",
            }
            .to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_day_topics_into_atomic_actions() {
        let program = compile(
            "Day 1: ownership\nDay 2: borrowing",
            None,
            ProgramKind::Curriculum,
            0.9,
            Some(2),
            Intensity::Standard,
        );
        assert_eq!(program.tasks.len(), 6);
        assert!(program.tasks.iter().all(|task| task.description.is_some()));
        assert!(matches!(
            program.tasks[0].cadence,
            Cadence::Once { day_offset: 0 }
        ));
        assert!(matches!(
            program.tasks[3].cadence,
            Cadence::Once { day_offset: 1 }
        ));
    }

    #[test]
    fn vague_input_gets_a_real_progression_without_ai() {
        let program = compile(
            "Learn Rust",
            None,
            ProgramKind::Curriculum,
            0.6,
            Some(14),
            Intensity::Standard,
        );
        assert_eq!(program.tasks.len(), 42);
        assert!(program
            .tasks
            .iter()
            .any(|task| task.title.starts_with("Practice")));
    }
}
