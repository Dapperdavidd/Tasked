use actix_web::{post, web, HttpResponse};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tracked_core::{cadence::Cadence, scoring};
use tracked_db::{rls, rows::EnrollmentRow};
use tracked_ingest::{
    GeneratedProgram, GeneratedTask, Intensity as GeneratedIntensity, ProgramKind,
};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
#[serde(untagged)]
pub enum CreateProgramRequest {
    Draft(CreateProgramFromDraftBody),
    Direct(CreateProgramBody),
}

#[derive(Deserialize)]
pub struct CreateProgramFromDraftBody {
    ingest_job_id: Uuid,
    start_date: NaiveDate,
    timezone: Option<String>,
    day_boundary_hour: Option<i16>,
}

#[derive(Deserialize)]
pub struct CreateProgramBody {
    title: String,
    summary: Option<String>,
    kind: ProgramKindBody,
    duration_days: i32,
    intensity: IntensityBody,
    start_date: NaiveDate,
    timezone: Option<String>,
    day_boundary_hour: Option<i16>,
    tasks: Vec<CreateTaskBody>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProgramKindBody {
    Curriculum,
    Routine,
    Project,
}

impl ProgramKindBody {
    fn as_db_value(&self) -> &'static str {
        match self {
            Self::Curriculum => "curriculum",
            Self::Routine => "routine",
            Self::Project => "project",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum IntensityBody {
    Light,
    Standard,
    Heavy,
}

impl IntensityBody {
    fn as_db_value(&self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Standard => "standard",
            Self::Heavy => "heavy",
        }
    }
}

#[derive(Deserialize)]
pub struct CreateTaskBody {
    title: String,
    description: Option<String>,
    category: Option<String>,
    difficulty: u8,
    estimated_minutes: u16,
    cadence: Cadence,
}

#[post("/v1/programs")]
pub async fn create_program(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<CreateProgramRequest>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    enforce_focus_constraint(&mut tx, user_id.0).await?;

    let resolved = match body.into_inner() {
        CreateProgramRequest::Direct(body) => resolve_direct_program(body)?,
        CreateProgramRequest::Draft(body) => {
            resolve_draft_program(&mut tx, user_id.0, body).await?
        }
    };

    let (settings_timezone, settings_boundary): (String, i16) =
        sqlx::query_as("select timezone, day_boundary_hour from user_settings where user_id = $1")
            .bind(user_id.0)
            .fetch_one(&mut *tx)
            .await?;

    let timezone = resolved.timezone.unwrap_or(settings_timezone);
    let day_boundary_hour = resolved.day_boundary_hour.unwrap_or(settings_boundary);
    if !(0..=4).contains(&day_boundary_hour) {
        return Err(ApiError::BadRequest("invalid day boundary hour".to_owned()));
    }

    let program_id = Uuid::now_v7();
    sqlx::query(
        r#"
        insert into programs (
          id, author_id, title, summary, kind, duration_days, intensity, source_id
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(program_id)
    .bind(user_id.0)
    .bind(&resolved.title)
    .bind(&resolved.summary)
    .bind(resolved.kind)
    .bind(resolved.duration_days)
    .bind(resolved.intensity)
    .bind(resolved.source_id)
    .execute(&mut *tx)
    .await?;

    for (index, task) in resolved.tasks.iter().enumerate() {
        let cadence = serde_json::to_value(&task.cadence)
            .map_err(|_| ApiError::BadRequest("invalid cadence".to_owned()))?;
        let position = i32::try_from(index + 1)
            .map_err(|_| ApiError::BadRequest("too many tasks".to_owned()))?;
        let points = scoring::task_points(task.difficulty, task.estimated_minutes);
        sqlx::query(
            r#"
            insert into task_templates (
              id, program_id, position, title, description, category,
              difficulty, estimated_minutes, cadence, points
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(program_id)
        .bind(position)
        .bind(&task.title)
        .bind(&task.description)
        .bind(&task.category)
        .bind(i16::from(task.difficulty))
        .bind(i32::from(task.estimated_minutes))
        .bind(cadence)
        .bind(points)
        .execute(&mut *tx)
        .await?;
    }

    let enrollment_id = Uuid::now_v7();
    let enrollment = sqlx::query_as::<_, EnrollmentRow>(
        r#"
        insert into enrollments (
          id, user_id, program_id, timezone, day_boundary_hour,
          start_date, is_standing, status
        )
        values ($1, $2, $3, $4, $5, $6, false, 'active')
        returning id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
                  start_date, is_standing, status, materialised_through, created_at
        "#,
    )
    .bind(enrollment_id)
    .bind(user_id.0)
    .bind(program_id)
    .bind(&timezone)
    .bind(day_boundary_hour)
    .bind(resolved.start_date)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query("insert into streak_states (enrollment_id) values ($1)")
        .bind(enrollment_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(HttpResponse::Ok().json(CreateProgramResponse {
        program_id,
        enrollment_id: enrollment.id,
        start_date: resolved.start_date,
    }))
}

fn validate_program_body(body: &CreateProgramBody) -> Result<(), ApiError> {
    if body.duration_days < 1 || body.duration_days > 730 {
        return Err(ApiError::BadRequest("invalid duration".to_owned()));
    }
    if body.tasks.is_empty() {
        return Err(ApiError::BadRequest("program requires tasks".to_owned()));
    }
    for task in &body.tasks {
        if task.title.trim().is_empty() {
            return Err(ApiError::BadRequest("task title is required".to_owned()));
        }
        if !(1..=5).contains(&task.difficulty) {
            return Err(ApiError::BadRequest("invalid difficulty".to_owned()));
        }
        if !(1..=480).contains(&task.estimated_minutes) {
            return Err(ApiError::BadRequest("invalid task duration".to_owned()));
        }
        task.cadence
            .validate_for_program(false)
            .map_err(|_| ApiError::BadRequest("invalid cadence".to_owned()))?;
    }
    Ok(())
}

fn resolve_direct_program(body: CreateProgramBody) -> Result<ResolvedProgram, ApiError> {
    validate_program_body(&body)?;

    Ok(ResolvedProgram {
        title: body.title,
        summary: body.summary,
        kind: body.kind.as_db_value(),
        duration_days: body.duration_days,
        intensity: body.intensity.as_db_value(),
        start_date: body.start_date,
        timezone: body.timezone,
        day_boundary_hour: body.day_boundary_hour,
        tasks: body.tasks,
        source_id: None,
    })
}

async fn resolve_draft_program(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    body: CreateProgramFromDraftBody,
) -> Result<ResolvedProgram, ApiError> {
    let row: (Option<Uuid>, String, String, Option<serde_json::Value>) = sqlx::query_as(
        r#"
        select source_id, intensity, status, draft
        from ingestion_jobs
        where id = $1
          and user_id = $2
        "#,
    )
    .bind(body.ingest_job_id)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| ApiError::BadRequest("ingest job not found".to_owned()))?;

    if row.2 != "ready" {
        return Err(ApiError::BadRequest("ingest job is not ready".to_owned()));
    }

    let draft: GeneratedProgram = serde_json::from_value(
        row.3
            .ok_or_else(|| ApiError::BadRequest("ingest draft is missing".to_owned()))?,
    )
    .map_err(|_| ApiError::BadRequest("ingest draft is invalid".to_owned()))?;

    let tasks = draft
        .tasks
        .into_iter()
        .map(create_task_from_generated)
        .collect::<Vec<_>>();

    Ok(ResolvedProgram {
        title: draft.title,
        summary: Some(draft.summary),
        kind: program_kind_db_value(draft.kind),
        duration_days: i32::from(draft.duration_days),
        intensity: intensity_db_value_from_generated(intensity_from_db_value(&row.1)?),
        start_date: body.start_date,
        timezone: body.timezone,
        day_boundary_hour: body.day_boundary_hour,
        tasks,
        source_id: row.0,
    })
}

fn create_task_from_generated(task: GeneratedTask) -> CreateTaskBody {
    CreateTaskBody {
        title: task.title,
        description: task.description,
        category: task.category,
        difficulty: task.difficulty,
        estimated_minutes: task.estimated_minutes,
        cadence: task.cadence,
    }
}

fn intensity_from_db_value(value: &str) -> Result<GeneratedIntensity, ApiError> {
    match value {
        "light" => Ok(GeneratedIntensity::Light),
        "standard" => Ok(GeneratedIntensity::Standard),
        "heavy" => Ok(GeneratedIntensity::Heavy),
        _ => Err(ApiError::BadRequest("invalid stored intensity".to_owned())),
    }
}

fn intensity_db_value_from_generated(intensity: GeneratedIntensity) -> &'static str {
    match intensity {
        GeneratedIntensity::Light => "light",
        GeneratedIntensity::Standard => "standard",
        GeneratedIntensity::Heavy => "heavy",
    }
}

fn program_kind_db_value(kind: ProgramKind) -> &'static str {
    match kind {
        ProgramKind::Curriculum => "curriculum",
        ProgramKind::Routine => "routine",
        ProgramKind::Project => "project",
    }
}

async fn enforce_focus_constraint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<(), ApiError> {
    let allow_multi_active: bool =
        sqlx::query_scalar("select allow_multi_active from user_settings where user_id = $1")
            .bind(user_id)
            .fetch_one(&mut **tx)
            .await?;
    if allow_multi_active {
        return Ok(());
    }
    let active_count: i64 = sqlx::query_scalar(
        r#"
        select count(*)
        from enrollments
        where user_id = $1
          and status = 'active'
          and not is_standing
        "#,
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    if active_count > 0 {
        return Err(ApiError::BadRequest(
            "pause or finish the active program first".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct CreateProgramResponse {
    program_id: Uuid,
    enrollment_id: Uuid,
    start_date: NaiveDate,
}

struct ResolvedProgram {
    title: String,
    summary: Option<String>,
    kind: &'static str,
    duration_days: i32,
    intensity: &'static str,
    start_date: NaiveDate,
    timezone: Option<String>,
    day_boundary_hour: Option<i16>,
    tasks: Vec<CreateTaskBody>,
    source_id: Option<Uuid>,
}
