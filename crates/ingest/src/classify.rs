//! Stage 2: classify.
//!
//! A cheap model call decides which of three shapes the source is, and — just
//! as importantly — whether it is a plan at all. A shopping list, a CV, or a
//! news article must fail here with a usable error code rather than be turned
//! into fiction by stage 3.
//!
//! Everything in this module is deterministic. The network call belongs to the
//! caller; this interprets what came back.

use crate::types::ProgramKind;
use serde::{Deserialize, Serialize};

/// Below this, the user picks the shape before generation runs. Guessing wrong
/// here produces a plausible program of the wrong shape, which is worse than
/// asking, because the user cannot tell it is wrong until day three.
pub const MIN_CONFIDENCE: f32 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub kind: ProgramKind,
    pub confidence: f32,
    pub suggested_duration_days: Option<u16>,
}

/// The raw shape stage 2 asks the model for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationResponse {
    /// Set when the source is not a plan. Carries the model's reason, which is
    /// shown to the user rather than logged.
    #[serde(default)]
    pub not_a_plan: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub kind: Option<ProgramKind>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub suggested_duration_days: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassifyOutcome {
    /// Confident enough to run stage 3 without asking.
    Proceed(ProgramKind),
    /// Ambiguous. The user picks before generation, per PRD F1.
    AskUser {
        suggested: ProgramKind,
        confidence_percent: u8,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClassifyError {
    /// Not a plan. The client maps this to a specific message and offers the
    /// universal fallback: describe your plan in a sentence.
    #[error("source is not a plan")]
    NotAPlan { reason: Option<String> },
    #[error("classifier response was malformed: {0}")]
    Malformed(String),
}

/// Interpret a classifier response.
pub fn interpret(response: ClassificationResponse) -> Result<Classification, ClassifyError> {
    if response.not_a_plan {
        return Err(ClassifyError::NotAPlan {
            reason: response.reason,
        });
    }

    let kind = response
        .kind
        .ok_or_else(|| ClassifyError::Malformed("missing kind".to_owned()))?;

    let confidence = response
        .confidence
        .ok_or_else(|| ClassifyError::Malformed("missing confidence".to_owned()))?;

    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(ClassifyError::Malformed(format!(
            "confidence out of range: {confidence}"
        )));
    }

    // A duration of zero is not a shorter program, it is a broken response.
    let suggested_duration_days = response
        .suggested_duration_days
        .filter(|days| (1..=730).contains(days));

    Ok(Classification {
        kind,
        confidence,
        suggested_duration_days,
    })
}

/// Decide whether to run stage 3 or ask the user first.
pub fn outcome(classification: &Classification) -> ClassifyOutcome {
    if classification.confidence >= MIN_CONFIDENCE {
        ClassifyOutcome::Proceed(classification.kind)
    } else {
        ClassifyOutcome::AskUser {
            suggested: classification.kind,
            confidence_percent: (classification.confidence * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8,
        }
    }
}

/// Parse a classifier response from raw model output.
pub fn parse(raw: &str) -> Result<Classification, ClassifyError> {
    let response: ClassificationResponse = serde_json::from_str(strip_code_fence(raw))
        .map_err(|error| ClassifyError::Malformed(error.to_string()))?;
    interpret(response)
}

/// Models wrap JSON in a fenced code block more often than they should.
pub fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop an optional language tag on the opening fence.
    let rest = rest.split_once('\n').map_or(rest, |(_, body)| body);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confident(kind: ProgramKind, confidence: f32) -> Classification {
        Classification {
            kind,
            confidence,
            suggested_duration_days: Some(56),
        }
    }

    #[test]
    fn a_confident_classification_proceeds() {
        assert_eq!(
            outcome(&confident(ProgramKind::Curriculum, 0.92)),
            ClassifyOutcome::Proceed(ProgramKind::Curriculum)
        );
    }

    #[test]
    fn an_uncertain_classification_asks_the_user_first() {
        assert_eq!(
            outcome(&confident(ProgramKind::Project, 0.55)),
            ClassifyOutcome::AskUser {
                suggested: ProgramKind::Project,
                confidence_percent: 55,
            }
        );
    }

    #[test]
    fn the_threshold_itself_proceeds() {
        assert!(matches!(
            outcome(&confident(ProgramKind::Routine, MIN_CONFIDENCE)),
            ClassifyOutcome::Proceed(_)
        ));
    }

    #[test]
    fn a_source_that_is_not_a_plan_fails_rather_than_generating_fiction() {
        let raw = r#"{"not_a_plan": true, "reason": "this is a shopping list"}"#;
        assert_eq!(
            parse(raw),
            Err(ClassifyError::NotAPlan {
                reason: Some("this is a shopping list".to_owned())
            })
        );
    }

    #[test]
    fn parses_a_normal_response() {
        let raw = r#"{"kind":"curriculum","confidence":0.88,"suggested_duration_days":56}"#;
        let parsed = parse(raw).expect("parses");
        assert_eq!(parsed.kind, ProgramKind::Curriculum);
        assert_eq!(parsed.suggested_duration_days, Some(56));
    }

    #[test]
    fn parses_through_a_fenced_code_block() {
        let raw = "```json\n{\"kind\":\"routine\",\"confidence\":0.9}\n```";
        assert_eq!(parse(raw).expect("parses").kind, ProgramKind::Routine);
    }

    #[test]
    fn rejects_confidence_outside_zero_to_one() {
        for bad in ["1.4", "-0.2"] {
            let raw = format!(r#"{{"kind":"routine","confidence":{bad}}}"#);
            assert!(matches!(parse(&raw), Err(ClassifyError::Malformed(_))));
        }
    }

    #[test]
    fn rejects_a_response_missing_its_fields() {
        assert!(matches!(
            parse(r#"{"confidence":0.9}"#),
            Err(ClassifyError::Malformed(_))
        ));
        assert!(matches!(
            parse(r#"{"kind":"routine"}"#),
            Err(ClassifyError::Malformed(_))
        ));
    }

    #[test]
    fn a_zero_duration_is_dropped_rather_than_believed() {
        let raw = r#"{"kind":"project","confidence":0.9,"suggested_duration_days":0}"#;
        assert_eq!(parse(raw).expect("parses").suggested_duration_days, None);
    }
}
