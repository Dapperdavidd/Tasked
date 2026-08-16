//! Model-backed planning intelligence.
//!
//! The model proposes domain-specific structure; the worker still validates,
//! calibrates, and persists the result. A missing key is an explicit
//! deterministic mode, never a fake claim that a model was used.

use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;
use tracked_ingest::{generate, GeneratedProgram, Intensity, ProgramKind};

const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const DEFAULT_MODEL: &str = "gpt-5-mini";
const OLLAMA_CHAT_URL: &str = "http://127.0.0.1:11434/api/chat";
const DEFAULT_OLLAMA_MODEL: &str = "qwen2.5:1.5b";
const DEFAULT_OLLAMA_TIMEOUT_SECONDS: u64 = 180;
const DEFAULT_OLLAMA_MAX_OUTPUT_TOKENS: u64 = 700;

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
    provider() != "deterministic"
}

pub fn deterministic_mode() -> bool {
    provider() == "deterministic"
}

fn provider() -> String {
    std::env::var("TASKED_AI_PROVIDER").unwrap_or_else(|_| "ollama".to_owned())
}

pub async fn generate_program(
    source: &str,
    instruction: Option<&str>,
    kind: ProgramKind,
    intensity: Intensity,
    duration_hint: Option<u16>,
) -> Result<GeneratedProgram, IntelligenceError> {
    if provider() == "ollama" {
        return generate_with_ollama(source, instruction, kind, intensity, duration_hint).await;
    }

    generate_with_openai(source, instruction, kind, intensity, duration_hint).await
}

async fn generate_with_openai(
    source: &str,
    instruction: Option<&str>,
    kind: ProgramKind,
    intensity: Intensity,
    duration_hint: Option<u16>,
) -> Result<GeneratedProgram, IntelligenceError> {
    let key = std::env::var("OPENAI_API_KEY").map_err(|_| IntelligenceError::MissingOutput)?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let (instructions, input, _) =
        request_parts(source, instruction, kind, intensity, duration_hint);
    let schema = ollama_schema();
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

async fn generate_with_ollama(
    source: &str,
    instruction: Option<&str>,
    kind: ProgramKind,
    intensity: Intensity,
    duration_hint: Option<u16>,
) -> Result<GeneratedProgram, IntelligenceError> {
    let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_OLLAMA_MODEL.to_owned());
    let (instructions, input, schema) =
        request_parts(source, instruction, kind, intensity, duration_hint);
    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": instructions },
            { "role": "user", "content": input }
        ],
        "stream": false,
        "format": schema,
        "options": {
            "temperature": 0,
            "num_predict": std::env::var("OLLAMA_MAX_OUTPUT_TOKENS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(DEFAULT_OLLAMA_MAX_OUTPUT_TOKENS)
        }
    });
    let timeout = std::env::var("OLLAMA_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_OLLAMA_TIMEOUT_SECONDS);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout))
        .build()?;
    let response = client
        .post(std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| OLLAMA_CHAT_URL.to_owned()))
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
                .and_then(Value::as_str)
                .unwrap_or("Ollama request failed")
                .to_owned(),
        });
    }
    let raw = payload
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or(IntelligenceError::MissingOutput)?;
    generate::parse(raw).map_err(|error| IntelligenceError::InvalidPlan(error.to_string()))
}

fn request_parts(
    source: &str,
    instruction: Option<&str>,
    kind: ProgramKind,
    intensity: Intensity,
    duration_hint: Option<u16>,
) -> (String, String, Value) {
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

For curriculum and project plans, every task must use cadence {"type":"once","day_offset":N}; use zero-based offsets from 0 through duration_days - 1. Use daily or weekly cadences only for routine plans. Never add fields that do not belong to the selected cadence variant.

Keep the response compact: generate no more than 2 tasks per day of the plan, and keep each description under 160 characters.

Return only the requested structured object. Do not put markdown or commentary outside the schema.
"#;
    (instructions.to_owned(), input, schema)
}

/// Ollama's structured decoder performs better with a compact schema than with
/// the full recursive `schemars` document. The Rust type remains the source of
/// truth: this is only a transport optimization for the local provider, and
/// `generate::parse` plus validation still reject anything outside the contract.
fn ollama_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "summary": { "type": "string" },
            "kind": { "type": "string", "enum": ["curriculum", "routine", "project"] },
            "duration_days": { "type": "integer" },
            "confidence": { "type": "number" },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "category": { "type": "string" },
                        "difficulty": { "type": "integer" },
                        "estimated_minutes": { "type": "integer" },
                        "cadence": {
                            "type": "object",
                            "properties": {
                                "type": { "type": "string", "enum": ["daily", "weekly_days", "n_per_week", "once"] },
                                "days": { "type": "array", "items": { "type": "integer" } },
                                "count": { "type": "integer" },
                                "day_offset": { "type": "integer" }
                            },
                            "required": ["type", "day_offset"]
                        }
                    },
                    "required": ["title", "description", "category", "difficulty", "estimated_minutes", "cadence"]
                }
            },
            "warnings": { "type": "array", "items": { "type": "object" } }
        },
        "required": ["title", "summary", "kind", "duration_days", "confidence", "tasks", "warnings"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_mode_is_explicit() {
        std::env::set_var("TASKED_AI_PROVIDER", "deterministic");
        assert!(!configured());
        std::env::remove_var("TASKED_AI_PROVIDER");
    }
}
