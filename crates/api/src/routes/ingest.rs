use actix_web::{get, post, web, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tracked_db::jobs::{self, JobRetryPolicy};
use tracked_ingest::{
    calibrate, normalise, validate, Extracted, GeneratedProgram, Intensity, NormaliseError,
    SourceKind, Warning,
};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct CreateIngestBody {
    source_text: String,
    mime_type: Option<String>,
    instruction: Option<String>,
    intensity: Intensity,
    draft: Option<GeneratedProgram>,
}

#[derive(Serialize)]
struct CreateIngestResponse {
    job_id: Uuid,
    status: String,
    cached: bool,
}

#[derive(Serialize)]
struct IngestStatusResponse {
    job_id: Uuid,
    source_id: Option<Uuid>,
    intensity: Intensity,
    status: String,
    instruction: Option<String>,
    draft: Option<GeneratedProgram>,
    warnings: Vec<Warning>,
    error_code: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct IngestEventEnvelope {
    event: &'static str,
    payload: IngestStatusResponse,
}

#[derive(FromRow)]
struct CachedIngestRow {
    job_id: Uuid,
    status: String,
}

#[derive(FromRow)]
struct IngestRow {
    id: Uuid,
    source_id: Option<Uuid>,
    intensity: String,
    status: String,
    instruction: Option<String>,
    draft: Option<serde_json::Value>,
    warnings: Option<serde_json::Value>,
    error_code: Option<String>,
    created_at: DateTime<Utc>,
}

#[post("/v1/ingest")]
pub async fn create_ingest(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<CreateIngestBody>,
) -> Result<HttpResponse, ApiError> {
    if body.source_text.trim().is_empty() {
        return Err(ApiError::BadRequest("source text is required".to_owned()));
    }

    let mime_type = body
        .mime_type
        .clone()
        .unwrap_or_else(|| "text/plain".to_owned());
    let source_kind = SourceKind::from_mime(&mime_type)
        .ok_or_else(|| ApiError::BadRequest("unsupported source type".to_owned()))?;

    let normalised = normalise(
        &Extracted {
            text: body.source_text.clone(),
            pages: None,
        },
        source_kind,
    )
    .map_err(map_normalise_error)?;

    let mut tx = state.pool.begin().await?;
    tracked_db::rls::set_request_user(&mut tx, user_id.0).await?;

    if let Some(cached) = sqlx::query_as::<_, CachedIngestRow>(
        r#"
        select ij.id as job_id, ij.status
        from ingestion_jobs ij
        join source_documents sd on sd.id = ij.source_id
        where ij.user_id = $1
          and sd.content_hash = $2
          and ij.intensity = $3
          and ij.status = 'ready'
        order by ij.created_at desc
        limit 1
        "#,
    )
    .bind(user_id.0)
    .bind(normalised.content_hash.as_slice())
    .bind(intensity_db_value(body.intensity))
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(HttpResponse::Ok().json(CreateIngestResponse {
            job_id: cached.job_id,
            status: cached.status,
            cached: true,
        }));
    }

    let source_id = Uuid::now_v7();
    sqlx::query(
        r#"
        insert into source_documents (
          id, user_id, content_hash, mime_type, extracted_text
        )
        values ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(source_id)
    .bind(user_id.0)
    .bind(normalised.content_hash.as_slice())
    .bind(&mime_type)
    .bind(&normalised.text)
    .execute(&mut *tx)
    .await?;

    let (status, draft_value, warnings_value) = if let Some(draft) = &body.draft {
        let ready = ready_draft(draft.clone(), body.intensity)?;
        (
            "ready",
            Some(
                serde_json::to_value(&ready)
                    .map_err(|_| ApiError::BadRequest("invalid draft".to_owned()))?,
            ),
            Some(
                serde_json::to_value(&ready.warnings)
                    .map_err(|_| ApiError::BadRequest("invalid draft warnings".to_owned()))?,
            ),
        )
    } else {
        ("queued", None, None)
    };

    let job_id = Uuid::now_v7();
    sqlx::query(
        r#"
        insert into ingestion_jobs (
          id, source_id, user_id, instruction, intensity, status, draft, warnings
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(job_id)
    .bind(source_id)
    .bind(user_id.0)
    .bind(&body.instruction)
    .bind(intensity_db_value(body.intensity))
    .bind(status)
    .bind(draft_value)
    .bind(warnings_value)
    .execute(&mut *tx)
    .await?;

    if status == "queued" {
        jobs::enqueue_job(
            &mut tx,
            "ingest_process",
            serde_json::json!({ "ingestion_job_id": job_id }),
            Utc::now(),
            JobRetryPolicy::default(),
        )
        .await?;
    }

    tx.commit().await?;

    if status == "queued" {
        tracked_worker::ingest::process_ingestion_job(&state.pool, job_id)
            .await
            .map(|_| ())
            .map_err(|error| ApiError::Worker(error.to_string()))?;
    }

    Ok(HttpResponse::Ok().json(CreateIngestResponse {
        job_id,
        status: status.to_owned(),
        cached: false,
    }))
}

#[get("/v1/ingest/{id}/events")]
pub async fn ingest_events(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let payload = fetch_ingest_status(&state, user_id.0, path.into_inner()).await?;
    let body = format!(
        "retry: 1000\nevent: progress\ndata: {}\n\n",
        serde_json::to_string(&IngestEventEnvelope {
            event: "progress",
            payload,
        })
        .map_err(|_| ApiError::BadRequest("invalid ingest event".to_owned()))?
    );

    Ok(HttpResponse::Ok()
        .content_type("text/event-stream")
        .body(body))
}

#[get("/v1/ingest/{id}")]
pub async fn get_ingest(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    Ok(HttpResponse::Ok().json(fetch_ingest_status(&state, user_id.0, path.into_inner()).await?))
}

fn ready_draft(
    draft: GeneratedProgram,
    intensity: Intensity,
) -> Result<GeneratedProgram, ApiError> {
    let validated = validate(draft).map_err(|error| ApiError::BadRequest(error.to_string()))?;
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
    ready.warnings = warnings;
    Ok(ready)
}

fn intensity_db_value(intensity: Intensity) -> &'static str {
    match intensity {
        Intensity::Light => "light",
        Intensity::Standard => "standard",
        Intensity::Heavy => "heavy",
    }
}

fn intensity_from_db(value: &str) -> Result<Intensity, ApiError> {
    match value {
        "light" => Ok(Intensity::Light),
        "standard" => Ok(Intensity::Standard),
        "heavy" => Ok(Intensity::Heavy),
        _ => Err(ApiError::BadRequest("invalid stored intensity".to_owned())),
    }
}

fn decode_optional_json<T: serde::de::DeserializeOwned>(
    value: Option<serde_json::Value>,
) -> Result<Option<T>, ApiError> {
    value
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|_| ApiError::BadRequest("invalid stored ingest payload".to_owned()))
        })
        .transpose()
}

fn decode_warnings(value: Option<serde_json::Value>) -> Result<Vec<Warning>, ApiError> {
    decode_optional_json(value).map(|warnings| warnings.unwrap_or_default())
}

async fn fetch_ingest_status(
    state: &web::Data<ApiState>,
    user_id: Uuid,
    ingest_job_id: Uuid,
) -> Result<IngestStatusResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    tracked_db::rls::set_request_user(&mut tx, user_id).await?;

    let row = sqlx::query_as::<_, IngestRow>(
        r#"
        select id, source_id, intensity, status, instruction, draft, warnings, error_code, created_at
        from ingestion_jobs
        where id = $1
        "#,
    )
    .bind(ingest_job_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::BadRequest("ingest job not found".to_owned()))?;

    tx.commit().await?;

    Ok(IngestStatusResponse {
        job_id: row.id,
        source_id: row.source_id,
        intensity: intensity_from_db(&row.intensity)?,
        status: row.status,
        instruction: row.instruction,
        draft: decode_optional_json(row.draft)?,
        warnings: decode_warnings(row.warnings)?,
        error_code: row.error_code,
        created_at: row.created_at,
    })
}

fn map_normalise_error(error: NormaliseError) -> ApiError {
    let message = match error {
        NormaliseError::UnsupportedMime => "unsupported source type",
        NormaliseError::Empty => "source produced no usable text",
        NormaliseError::NeedsOcr => "source looks like a scan and needs OCR",
    };
    ApiError::BadRequest(message.to_owned())
}
