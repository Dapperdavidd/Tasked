use actix_web::{get, patch, web, HttpResponse};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tracked_core::artifact::{self, Cell, DaySummary};
use tracked_db::{artifacts as artifacts_db, rls, rows::EnrollmentRow};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[get("/v1/enrollments")]
pub async fn list_enrollments(
    state: web::Data<ApiState>,
    user_id: UserId,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let rows = sqlx::query_as::<_, EnrollmentRow>(
        r#"
        select id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
               start_date, is_standing, status, materialised_through, created_at
        from enrollments
        where user_id = $1
        order by is_standing asc, created_at desc
        "#,
    )
    .bind(user_id.0)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(
        rows.into_iter()
            .map(EnrollmentResponse::from)
            .collect::<Vec<_>>(),
    ))
}

#[derive(Deserialize)]
pub struct PatchEnrollmentBody {
    status: Option<String>,
    timezone: Option<String>,
    day_boundary_hour: Option<i16>,
}

#[patch("/v1/enrollments/{id}")]
pub async fn patch_enrollment(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
    body: web::Json<PatchEnrollmentBody>,
) -> Result<HttpResponse, ApiError> {
    if let Some(hour) = body.day_boundary_hour {
        if !(0..=4).contains(&hour) {
            return Err(ApiError::BadRequest("invalid day boundary hour".to_owned()));
        }
    }
    if let Some(status) = &body.status {
        if !matches!(
            status.as_str(),
            "active" | "paused" | "completed" | "abandoned"
        ) {
            return Err(ApiError::BadRequest("invalid enrollment status".to_owned()));
        }
    }

    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let row = sqlx::query_as::<_, EnrollmentRow>(
        r#"
        update enrollments
        set status = coalesce($3, status),
            timezone = coalesce($4, timezone),
            day_boundary_hour = coalesce($5, day_boundary_hour)
        where id = $1
          and user_id = $2
        returning id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
                  start_date, is_standing, status, materialised_through, created_at
        "#,
    )
    .bind(*path)
    .bind(user_id.0)
    .bind(&body.status)
    .bind(&body.timezone)
    .bind(body.day_boundary_hour)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(EnrollmentResponse::from(row)))
}

#[get("/v1/enrollments/{id}/summary")]
pub async fn enrollment_summary(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;

    let enrollment_id = *path;
    let metadata = sqlx::query_as::<_, ArtifactMetadata>(
        r#"
        select e.is_standing, p.title, s.longest
        from enrollments e
        join programs p on p.id = e.program_id
        join streak_states s on s.enrollment_id = e.id
        where e.id = $1
          and e.user_id = $2
        "#,
    )
    .bind(enrollment_id)
    .bind(user_id.0)
    .fetch_one(&mut *tx)
    .await?;

    let rows = sqlx::query_as::<_, ArtifactDayRow>(
        r#"
        select d.local_date,
               d.status,
               d.earned_points,
               d.available_points,
               d.note,
               count(ti.id) filter (
                 where ti.completed_at is not null
                   and ti.skipped_reason is null
                   and not ti.is_floating
               ) as tasks_completed,
               coalesce(sum(tt.estimated_minutes) filter (
                 where ti.completed_at is not null
                   and ti.skipped_reason is null
                   and not ti.is_floating
               ), 0) as minutes_invested
        from days d
        left join task_instances ti on ti.day_id = d.id
        left join task_templates tt on tt.id = ti.template_id
        where d.enrollment_id = $1
          and d.finalised_at is not null
        group by d.id
        order by d.local_date
        "#,
    )
    .bind(enrollment_id)
    .fetch_all(&mut *tx)
    .await?;

    let days = rows
        .into_iter()
        .map(DaySummary::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let longest = u32::try_from(metadata.longest)
        .map_err(|_| ApiError::BadRequest("streak value out of range".to_owned()))?;
    let artifact = artifact::compile(&metadata.title, metadata.is_standing, &days, longest)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let payload = CompletionArtifactResponse::from(artifact);
    let stored =
        artifacts_db::upsert_completion_artifact(&mut tx, enrollment_id, None, None, &payload)
            .await?;

    tx.commit().await?;

    Ok(HttpResponse::Ok().json(EnrollmentSummaryResponse {
        id: stored.id,
        enrollment_id,
        image_key: stored.image_key,
        pdf_key: stored.pdf_key,
        payload,
    }))
}

#[derive(Serialize)]
struct EnrollmentResponse {
    id: Uuid,
    program_id: Uuid,
    cohort_id: Option<Uuid>,
    timezone: String,
    day_boundary_hour: i16,
    start_date: NaiveDate,
    is_standing: bool,
    status: String,
    materialised_through: Option<NaiveDate>,
}

impl From<EnrollmentRow> for EnrollmentResponse {
    fn from(row: EnrollmentRow) -> Self {
        Self {
            id: row.id,
            program_id: row.program_id,
            cohort_id: row.cohort_id,
            timezone: row.timezone,
            day_boundary_hour: row.day_boundary_hour,
            start_date: row.start_date,
            is_standing: row.is_standing,
            status: format!("{:?}", row.status),
            materialised_through: row.materialised_through,
        }
    }
}

#[derive(FromRow)]
struct ArtifactMetadata {
    is_standing: bool,
    title: String,
    longest: i32,
}

#[derive(FromRow)]
struct ArtifactDayRow {
    local_date: NaiveDate,
    status: tracked_db::rows::DayStatus,
    earned_points: i32,
    available_points: i32,
    note: Option<String>,
    tasks_completed: i64,
    minutes_invested: i64,
}

impl TryFrom<ArtifactDayRow> for DaySummary {
    type Error = ApiError;

    fn try_from(row: ArtifactDayRow) -> Result<Self, Self::Error> {
        Ok(Self {
            local_date: row.local_date,
            cell: cell_from_status(row.status)?,
            earned_points: row.earned_points,
            available_points: row.available_points,
            tasks_completed: i64_to_u32(row.tasks_completed)?,
            minutes_invested: i64_to_u32(row.minutes_invested)?,
            note: row.note,
        })
    }
}

fn cell_from_status(status: tracked_db::rows::DayStatus) -> Result<Cell, ApiError> {
    match status {
        tracked_db::rows::DayStatus::Complete => Ok(Cell::Complete),
        tracked_db::rows::DayStatus::Partial => Ok(Cell::Partial),
        tracked_db::rows::DayStatus::Missed => Ok(Cell::Missed),
        tracked_db::rows::DayStatus::Rest => Ok(Cell::Rest),
        tracked_db::rows::DayStatus::Frozen => Ok(Cell::Frozen),
        tracked_db::rows::DayStatus::Open => Err(ApiError::BadRequest(
            "open day cannot be compiled into completion artifact".to_owned(),
        )),
    }
}

fn i64_to_u32(value: i64) -> Result<u32, ApiError> {
    u32::try_from(value).map_err(|_| ApiError::BadRequest("artifact value out of range".to_owned()))
}

#[derive(Serialize)]
struct EnrollmentSummaryResponse {
    id: Uuid,
    enrollment_id: Uuid,
    image_key: Option<String>,
    pdf_key: Option<String>,
    payload: CompletionArtifactResponse,
}

#[derive(Serialize)]
struct CompletionArtifactResponse {
    title: String,
    started_on: NaiveDate,
    finished_on: NaiveDate,
    days_total: u32,
    days_logged: u32,
    completion_rate: Option<u8>,
    longest_streak: u32,
    tasks_completed: u32,
    hours_invested: u32,
    cells: Vec<ArtifactCellResponse>,
    notes: Vec<ArtifactNoteResponse>,
}

impl From<artifact::CompletionArtifact> for CompletionArtifactResponse {
    fn from(artifact: artifact::CompletionArtifact) -> Self {
        Self {
            title: artifact.title,
            started_on: artifact.started_on,
            finished_on: artifact.finished_on,
            days_total: artifact.days_total,
            days_logged: artifact.days_logged,
            completion_rate: artifact.completion_rate,
            longest_streak: artifact.longest_streak,
            tasks_completed: artifact.tasks_completed,
            hours_invested: artifact.hours_invested,
            cells: artifact
                .cells
                .into_iter()
                .map(|(local_date, cell)| ArtifactCellResponse {
                    local_date,
                    status: cell_name(cell),
                })
                .collect(),
            notes: artifact
                .notes
                .into_iter()
                .map(|(local_date, note)| ArtifactNoteResponse { local_date, note })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct ArtifactCellResponse {
    local_date: NaiveDate,
    status: &'static str,
}

#[derive(Serialize)]
struct ArtifactNoteResponse {
    local_date: NaiveDate,
    note: String,
}

fn cell_name(cell: Cell) -> &'static str {
    match cell {
        Cell::Complete => "complete",
        Cell::Partial => "partial",
        Cell::Missed => "missed",
        Cell::Rest => "rest",
        Cell::Frozen => "frozen",
    }
}
