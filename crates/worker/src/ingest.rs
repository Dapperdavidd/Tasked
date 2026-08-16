use crate::intelligence;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use tracked_ingest::{
    calibrate, generate, normalise, planner, Extracted, GeneratedProgram, Intensity, ProgramKind,
    SourceKind,
};
use uuid::Uuid;

const INGEST_JOB_KIND: &str = "ingest_process";

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("payload error: {0}")]
    Payload(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IngestPayload {
    ingestion_job_id: Uuid,
}

#[derive(Clone, Debug, FromRow)]
struct ClaimedJob {
    id: Uuid,
    attempts: i32,
    max_attempts: i32,
    payload: serde_json::Value,
}

#[derive(Clone, Debug, FromRow)]
struct IngestSource {
    source_id: Uuid,
    mime_type: Option<String>,
    extracted_text: String,
    instruction: Option<String>,
    intensity: String,
}

#[derive(Clone, Debug)]
struct HeuristicClassification {
    kind: ProgramKind,
    confidence: f32,
    suggested_duration_days: Option<u16>,
}

pub async fn process_due(pool: &PgPool, max_jobs: u32) -> Result<u32, IngestError> {
    let mut processed = 0_u32;

    for _ in 0..max_jobs {
        let Some(job) = claim_next_ingest_job(pool).await? else {
            break;
        };
        process_one(pool, job).await?;
        processed += 1;
    }

    Ok(processed)
}

pub async fn process_ingestion_job(
    pool: &PgPool,
    ingestion_job_id: Uuid,
) -> Result<bool, IngestError> {
    let Some(job) = claim_ingest_job(pool, ingestion_job_id).await? else {
        return Ok(false);
    };
    process_one(pool, job).await?;
    Ok(true)
}

async fn process_one(pool: &PgPool, job: ClaimedJob) -> Result<(), IngestError> {
    let payload: IngestPayload = serde_json::from_value(job.payload.clone())?;
    let mut tx = pool.begin().await?;

    let Some(source) = load_source(&mut tx, payload.ingestion_job_id).await? else {
        release_job(&mut tx, job.id).await?;
        tx.commit().await?;
        return Ok(());
    };

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "normalising",
        None,
        None,
        None,
    )
    .await?;

    let source_kind = source
        .mime_type
        .as_deref()
        .and_then(SourceKind::from_mime)
        .unwrap_or(SourceKind::Text);
    let normalised = match normalise(
        &Extracted {
            text: source.extracted_text.clone(),
            pages: None,
        },
        source_kind,
    ) {
        Ok(normalised) => normalised,
        Err(error) => {
            fail_ingestion(
                &mut tx,
                payload.ingestion_job_id,
                job,
                ingest_error_code(&error.to_string()),
                &error.to_string(),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
    };

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "classifying",
        None,
        None,
        None,
    )
    .await?;
    let classification = classify_source(&normalised.text, source.instruction.as_deref());

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "generating",
        None,
        None,
        None,
    )
    .await?;
    let intensity = intensity_from_db(&source.intensity);
    let generated = if intelligence::configured() {
        match intelligence::generate_program(
            &normalised.text,
            source.instruction.as_deref(),
            classification.kind,
            intensity,
            classification.suggested_duration_days,
        )
        .await
        {
            Ok(program) => program,
            Err(error) => {
                fail_ingestion(
                    &mut tx,
                    payload.ingestion_job_id,
                    job,
                    "ai_generation_failed",
                    &error.to_string(),
                )
                .await?;
                tx.commit().await?;
                return Ok(());
            }
        }
    } else if intelligence::deterministic_mode() {
        generate_program(
            &normalised.text,
            source.instruction.as_deref(),
            &classification,
            intensity,
        )
    } else {
        fail_ingestion(
            &mut tx,
            payload.ingestion_job_id,
            job,
            "ai_not_configured",
            "Set OPENAI_API_KEY or explicitly choose TASKED_AI_PROVIDER=deterministic",
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    };

    let validated = match generate::validate(generated) {
        Ok(validated) => validated,
        Err(error) => {
            fail_ingestion(
                &mut tx,
                payload.ingestion_job_id,
                job,
                "invalid_generated_program",
                &error.to_string(),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
    };

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "calibrating",
        None,
        None,
        None,
    )
    .await?;
    let calibration = calibrate(
        validated.program.tasks.clone(),
        validated.program.duration_days,
        intensity,
    );

    let mut ready = validated.program;
    let mut warnings = validated.warnings;
    warnings.extend(calibration.warnings);
    ready.duration_days = calibration.duration_days;
    ready.tasks = calibration.tasks;
    ready.warnings = warnings.clone();

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "ready",
        Some(serde_json::to_value(&ready)?),
        Some(serde_json::to_value(&warnings)?),
        None,
    )
    .await?;
    sqlx::query(
        r#"
        update source_documents
        set content_hash = $2,
            extracted_text = $3
        where id = $1
        "#,
    )
    .bind(source.source_id)
    .bind(normalised.content_hash.as_slice())
    .bind(&normalised.text)
    .execute(&mut *tx)
    .await?;
    release_job(&mut tx, job.id).await?;
    tx.commit().await?;

    Ok(())
}

async fn claim_ingest_job(
    pool: &PgPool,
    ingestion_job_id: Uuid,
) -> Result<Option<ClaimedJob>, sqlx::Error> {
    sqlx::query_as::<_, ClaimedJob>(
        r#"
        update jobs
        set locked_until = now() + interval '5 minutes',
            attempts = attempts + 1
        where id = (
          select id
          from jobs
          where failed_at is null
            and kind = $1
            and payload->>'ingestion_job_id' = $2
            and attempts < max_attempts
            and (locked_until is null or locked_until < now())
          order by run_at
          for update skip locked
          limit 1
        )
        returning id, attempts, max_attempts, payload
        "#,
    )
    .bind(INGEST_JOB_KIND)
    .bind(ingestion_job_id.to_string())
    .fetch_optional(pool)
    .await
}

async fn claim_next_ingest_job(pool: &PgPool) -> Result<Option<ClaimedJob>, sqlx::Error> {
    sqlx::query_as::<_, ClaimedJob>(
        r#"
        update jobs
        set locked_until = now() + interval '5 minutes',
            attempts = attempts + 1
        where id = (
          select id
          from jobs
          where failed_at is null
            and kind = $1
            and run_at <= now()
            and attempts < max_attempts
            and (locked_until is null or locked_until < now())
          order by run_at
          for update skip locked
          limit 1
        )
        returning id, attempts, max_attempts, payload
        "#,
    )
    .bind(INGEST_JOB_KIND)
    .fetch_optional(pool)
    .await
}

async fn load_source(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion_job_id: Uuid,
) -> Result<Option<IngestSource>, sqlx::Error> {
    sqlx::query_as::<_, IngestSource>(
        r#"
        select sd.id as source_id, sd.mime_type, sd.extracted_text, ij.instruction, ij.intensity
        from ingestion_jobs ij
        join source_documents sd on sd.id = ij.source_id
        where ij.id = $1
        "#,
    )
    .bind(ingestion_job_id)
    .fetch_optional(&mut **tx)
    .await
}

async fn update_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion_job_id: Uuid,
    status: &str,
    draft: Option<serde_json::Value>,
    warnings: Option<serde_json::Value>,
    error_code: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        update ingestion_jobs
        set status = $2,
            draft = coalesce($3, draft),
            warnings = coalesce($4, warnings),
            error_code = $5
        where id = $1
        "#,
    )
    .bind(ingestion_job_id)
    .bind(status)
    .bind(draft)
    .bind(warnings)
    .bind(error_code)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn fail_ingestion(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion_job_id: Uuid,
    job: ClaimedJob,
    error_code: &str,
    last_error: &str,
) -> Result<(), sqlx::Error> {
    let stop = job.attempts >= job.max_attempts;

    sqlx::query(
        r#"
        update ingestion_jobs
        set status = 'failed',
            error_code = $2
        where id = $1
        "#,
    )
    .bind(ingestion_job_id)
    .bind(error_code)
    .execute(&mut **tx)
    .await?;

    if stop {
        sqlx::query(
            r#"
            update jobs
            set failed_at = now(),
                locked_until = null,
                last_error = $2
            where id = $1
            "#,
        )
        .bind(job.id)
        .bind(last_error)
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query(
            r#"
            update jobs
            set run_at = now() + interval '60 seconds',
                locked_until = null,
                last_error = $2
            where id = $1
            "#,
        )
        .bind(job.id)
        .bind(last_error)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

async fn release_job(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from jobs where id = $1")
        .bind(job_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn classify_source(text: &str, instruction: Option<&str>) -> HeuristicClassification {
    let combined = format!("{text}\n{}", instruction.unwrap_or_default()).to_lowercase();
    let suggested_duration_days = infer_duration_days(&combined);

    if has_routine_markers(&combined) {
        return HeuristicClassification {
            kind: ProgramKind::Routine,
            confidence: 0.72,
            suggested_duration_days,
        };
    }

    if has_curriculum_markers(&combined) {
        return HeuristicClassification {
            kind: ProgramKind::Curriculum,
            confidence: 0.7,
            suggested_duration_days,
        };
    }

    HeuristicClassification {
        kind: ProgramKind::Project,
        confidence: 0.62,
        suggested_duration_days,
    }
}

fn generate_program(
    text: &str,
    instruction: Option<&str>,
    classification: &HeuristicClassification,
    intensity: Intensity,
) -> GeneratedProgram {
    return planner::compile(
        text,
        instruction,
        classification.kind,
        classification.confidence,
        classification.suggested_duration_days,
        intensity,
    );
}

fn intensity_from_db(value: &str) -> Intensity {
    match value {
        "light" => Intensity::Light,
        "heavy" => Intensity::Heavy,
        _ => Intensity::Standard,
    }
}

fn infer_duration_days(text: &str) -> Option<u16> {
    let words = text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for token in words.windows(2) {
        let [amount, unit] = token else {
            continue;
        };
        let Ok(number) = amount.parse::<u16>() else {
            continue;
        };
        if unit.starts_with("week") {
            return Some(number.saturating_mul(7).clamp(1, 730));
        }
        if unit.starts_with("day") {
            return Some(number.clamp(1, 730));
        }
    }
    None
}

fn has_routine_markers(text: &str) -> bool {
    [
        "daily",
        "every day",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
        "weekdays",
        "3x",
        "2x",
        "times a week",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn has_curriculum_markers(text: &str) -> bool {
    if (text.contains("learning plan") || text.contains("study plan") || text.contains("course"))
        && (text.contains("learn") || text.contains("beginner") || text.contains("study"))
    {
        return true;
    }

    [
        "day 1",
        "week 1",
        "chapter 1",
        "lesson 1",
        "module 1",
        "session 1",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn ingest_error_code(message: &str) -> &str {
    if message.contains("scan") {
        "needs_ocr"
    } else if message.contains("usable text") {
        "empty_source"
    } else {
        "ingest_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_routine_markers() {
        let classification = classify_source("Run daily\nLift on Monday Wednesday Friday", None);
        assert_eq!(classification.kind, ProgramKind::Routine);
    }

    #[test]
    fn infers_duration_from_weeks() {
        assert_eq!(infer_duration_days("8 week 5k plan"), Some(56));
    }

    #[test]
    fn infers_duration_from_hyphenated_days() {
        assert_eq!(
            infer_duration_days("10-day beginner Rust learning plan"),
            Some(10)
        );
    }

    #[test]
    fn roadmap_topics_become_atomic_tasks() {
        let classification = classify_source("Day 1: ownership\nDay 2: borrowing", None);
        let generated = generate_program(
            "Day 1: ownership\nDay 2: borrowing",
            None,
            &classification,
            Intensity::Standard,
        );

        assert_eq!(generated.duration_days, 2);
        assert_eq!(generated.tasks.len(), 6);
        assert!(generated
            .tasks
            .iter()
            .all(|task| task.description.is_some()));
        assert!(generated
            .tasks
            .iter()
            .any(|task| task.title.starts_with("Practice")));
    }

    #[test]
    fn vague_goal_gets_a_multi_day_plan_without_ai() {
        let classification = classify_source("Learn Rust", None);
        let generated = generate_program("Learn Rust", None, &classification, Intensity::Standard);

        assert!(generated.duration_days > 1);
        assert!(generated.tasks.len() > 3);
        assert!(generated
            .tasks
            .iter()
            .all(|task| task.description.is_some()));
    }

    #[test]
    fn intensity_changes_action_duration() {
        let classification = classify_source("Learn Rust", None);
        let light = generate_program("Learn Rust", None, &classification, Intensity::Light);
        let heavy = generate_program("Learn Rust", None, &classification, Intensity::Heavy);

        assert!(light
            .tasks
            .iter()
            .zip(heavy.tasks.iter())
            .all(|(light, heavy)| light.estimated_minutes < heavy.estimated_minutes));
    }

    #[test]
    fn listed_source_is_expanded_instead_of_copied() {
        let classification = classify_source("- Run 30 min\n- Stretch", None);
        let generated = generate_program(
            "- Run 30 min\n- Stretch",
            None,
            &classification,
            Intensity::Standard,
        );

        assert_eq!(generated.tasks.len(), 6);
        assert!(generated.tasks[0].title.contains("Run 30 min"));
        assert!(generated
            .tasks
            .iter()
            .any(|task| task.title.starts_with("Check")));
    }
}
