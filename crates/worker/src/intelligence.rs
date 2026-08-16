//! Model-backed planning intelligence.
//!
//! The model proposes domain-specific structure; the worker still validates,
//! calibrates, and persists the result. A missing key is an explicit
//! deterministic mode, never a fake claim that a model was used.

use reqwest::StatusCode;
use serde_json::{json, Value};
use tracked_ingest::{generate, GeneratedProgram, Intensity, ProgramKind};

const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const DEFAULT_MODEL: &str = "gpt-5-mini";

#[derive(Debug, thiserror::Error)]
pub enum IntelligenceError {
    #[error("OpenAI request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("OpenAI returned {status}: {body}")]
    Response { status: StatusCode, body: String },
    #[error("OpenAI response did not contain structured plan output")]
    MissingOutput,
    #[error("OpenAI plan was invalid: {0}")]
    InvalidPlan(String),
}

pub fn configured() -> bool {
    std::env::var("OPENAI_API_KEY")
        .ok()
        .is_some_and(|key| !key.trim().is_empty())
}

pub fn deterministic_mode() -> bool {
    std::env::var("TASKED_AI_PROVIDER").as_deref() == Ok("deterministic")
}

pub async fn generate_program(
    source: &str,
    instruction: Option<&str>,
    kind: ProgramKind,
    intensity: Intensity,
    duration_hint: Option<u16>,
) -> Result<GeneratedProgram, IntelligenceError> {
    let key = std::env::var("OPENAI_API_KEY").map_err(|_| IntelligenceError::MissingOutput)?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let cap = intensity.daily_cap_minutes();
    let schema = generate::response_schema();
    let input = format!(
        "User direction: {}\nProgram shape: {:?}\nDaily capacity: {} minutes\nDuration hint: {:?}\n\nSOURCE CONTEXT:\n{}",
        instruction.unwrap_or("No extra direction"),
        kind,
        cap,
        duration_hint,
        source.chars().take(120_000).collect::<String>()
    );
    let instructions = r#"
You are Tasked's product intelligence and planning engine. Turn source context into a realistic execution system, not a copied checklist.

Reason over the domain before writing tasks. Infer the user's target outcome, identify the prerequisite progression, and use the source as evidence. Preserve useful source wording in descriptions when it matters, but do not mirror day numbers or headings literally.

Every task must be a small focused-session action that starts with a concrete verb. Replace topics such as “learn ownership” with actions such as reading the relevant section, writing a specific example, fixing a specific failure, solving an exercise, building a small artifact, or passing a check. Every task description must state why it matters, what to do, and how completion will be verified. Prefer the smallest set of actions that advances the goal.

Use the requested duration and capacity. Create meaningful progression across multiple days. Respect prerequisites: foundations before dependent concepts, practice before projects, and review/checks after application. Do not invent URLs or claim research you did not perform. If the source names a resource, retain that source reference in the task description.

Return only the requested structured object. Do not put markdown or commentary outside the schema.
"#;
    let body = json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "tasked_generated_program",
                "strict": true,
                "schema": schema
            }
        }
    });

    let response = reqwest::Client::new()
        .post(OPENAI_RESPONSES_URL)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let payload: Value = response.json().await?;
    if !status.is_success() {
        return Err(IntelligenceError::Response {
            status,
            body: payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown provider error")
                .to_owned(),
        });
    }

    let raw = payload
        .get("output_text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            payload
                .get("output")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        item.get("content")?.as_array()?.iter().find_map(|content| {
                            (content.get("type")?.as_str()? == "output_text")
                                .then(|| content.get("text")?.as_str().map(ToOwned::to_owned))
                                .flatten()
                        })
                    })
                })
        })
        .ok_or(IntelligenceError::MissingOutput)?;

    generate::parse(&raw).map_err(|error| IntelligenceError::InvalidPlan(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_is_disabled_without_a_key() {
        std::env::remove_var("OPENAI_API_KEY");
        assert!(!configured());
    }
}
