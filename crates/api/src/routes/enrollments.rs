use actix_web::{get, patch, post, web, HttpResponse};
use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use tracked_core::{
    artifact::{self, Cell, DaySummary},
    calendar,
};
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

#[derive(Deserialize)]
pub struct ReturnEnrollmentBody {
    action: ReturnAction,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReturnAction {
    Resume,
    Restart,
    ScaleDown,
}

#[post("/v1/enrollments/{id}/return")]
pub async fn return_enrollment(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
    body: web::Json<ReturnEnrollmentBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;

    let current = sqlx::query_as::<_, EnrollmentRow>(
        r#"
        select id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
               start_date, is_standing, status, materialised_through, created_at
        from enrollments
        where id = $1
          and user_id = $2
          and not is_standing
        "#,
    )
    .bind(*path)
    .bind(user_id.0)
    .fetch_one(&mut *tx)
    .await?;

    let row = match body.action {
        ReturnAction::Resume => {
            sqlx::query_as::<_, EnrollmentRow>(
                r#"
                update enrollments
                set status = 'active'
                where id = $1
                  and user_id = $2
                returning id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
                          start_date, is_standing, status, materialised_through, created_at
                "#,
            )
            .bind(current.id)
            .bind(user_id.0)
            .fetch_one(&mut *tx)
            .await?
        }
        ReturnAction::Restart => {
            let restart_id = Uuid::now_v7();
            let start_date = local_today(&current.timezone, current.day_boundary_hour)?;

            sqlx::query(
                r#"
                update enrollments
                set status = 'abandoned'
                where id = $1
                  and user_id = $2
                "#,
            )
            .bind(current.id)
            .bind(user_id.0)
            .execute(&mut *tx)
            .await?;

            let restarted = sqlx::query_as::<_, EnrollmentRow>(
                r#"
                insert into enrollments (
                  id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
                  start_date, is_standing, status
                )
                values ($1, $2, $3, $4, $5, $6, $7, false, 'active')
                returning id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
                          start_date, is_standing, status, materialised_through, created_at
                "#,
            )
            .bind(restart_id)
            .bind(user_id.0)
            .bind(current.program_id)
            .bind(current.cohort_id)
            .bind(&current.timezone)
            .bind(current.day_boundary_hour)
            .bind(start_date)
            .fetch_one(&mut *tx)
            .await?;

            sqlx::query(
                r#"
                insert into streak_states (enrollment_id)
                values ($1)
                "#,
            )
            .bind(restart_id)
            .execute(&mut *tx)
            .await?;

            restarted
        }
        ReturnAction::ScaleDown => scale_down_enrollment(&mut tx, user_id.0, &current).await?,
    };

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(EnrollmentResponse::from(row)))
}

async fn scale_down_enrollment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    current: &EnrollmentRow,
) -> Result<EnrollmentRow, ApiError> {
    let program = sqlx::query_as::<_, ProgramForScaleDown>(
        r#"
        select id, title, summary, kind, duration_days, intensity
        from programs
        where id = $1
          and kind <> 'standing'
        "#,
    )
    .bind(current.program_id)
    .fetch_one(&mut **tx)
    .await?;

    let active_templates = sqlx::query_as::<_, TemplateForScaleDown>(
        r#"
        select id, position, title, description, category, difficulty,
               estimated_minutes, cadence, points
        from task_templates
        where program_id = $1
          and paused_at is null
        order by position, id
        "#,
    )
    .bind(current.program_id)
    .fetch_all(&mut **tx)
    .await?;

    if active_templates.len() <= 1 {
        return Err(ApiError::BadRequest(
            "program has no removable tasks".to_owned(),
        ));
    }

    let performance = sqlx::query_as::<_, TemplatePerformance>(
        r#"
        select ti.template_id,
               count(*) filter (where ti.skipped_reason is null) as available_count,
               count(*) filter (
                 where ti.completed_at is not null
                   and ti.skipped_reason is null
               ) as completed_count
        from task_instances ti
        join days d on d.id = ti.day_id
        where d.enrollment_id = $1
          and not ti.is_floating
        group by ti.template_id
        "#,
    )
    .bind(current.id)
    .fetch_all(&mut **tx)
    .await?;

    let drop_count = (active_templates.len() / 4)
        .max(1)
        .min(active_templates.len() - 1);
    let drop_ids = worst_template_ids(&active_templates, &performance, drop_count);
    let kept_templates = active_templates
        .into_iter()
        .filter(|template| !drop_ids.contains(&template.id))
        .collect::<Vec<_>>();

    let scaled_program_id = Uuid::now_v7();
    let scaled_enrollment_id = Uuid::now_v7();
    let start_date = local_today(&current.timezone, current.day_boundary_hour)?;
    let duration_days =
        scaled_duration_days(program.duration_days, current.start_date, start_date)?;

    sqlx::query(
        r#"
        update enrollments
        set status = 'abandoned'
        where id = $1
          and user_id = $2
        "#,
    )
    .bind(current.id)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
        insert into programs (
          id, author_id, title, summary, kind, duration_days, intensity, source_id,
          share_titles
        )
        values ($1, $2, $3, $4, $5, $6, $7, null, false)
        "#,
    )
    .bind(scaled_program_id)
    .bind(user_id)
    .bind(format!("{} — scaled down", program.title))
    .bind(scaled_summary(&program, drop_count, kept_templates.len()))
    .bind(program.kind)
    .bind(duration_days)
    .bind(program.intensity)
    .execute(&mut **tx)
    .await?;

    for (index, template) in kept_templates.into_iter().enumerate() {
        let position = i32::try_from(index + 1)
            .map_err(|_| ApiError::BadRequest("template position out of range".to_owned()))?;
        sqlx::query(
            r#"
            insert into task_templates (
              id, program_id, position, title, description, category, difficulty,
              estimated_minutes, cadence, points
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(scaled_program_id)
        .bind(position)
        .bind(template.title)
        .bind(template.description)
        .bind(template.category)
        .bind(template.difficulty)
        .bind(template.estimated_minutes)
        .bind(adjust_cadence_for_duration(template.cadence, duration_days))
        .bind(template.points)
        .execute(&mut **tx)
        .await?;
    }

    let scaled = sqlx::query_as::<_, EnrollmentRow>(
        r#"
        insert into enrollments (
          id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
          start_date, is_standing, status
        )
        values ($1, $2, $3, null, $4, $5, $6, false, 'active')
        returning id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
                  start_date, is_standing, status, materialised_through, created_at
        "#,
    )
    .bind(scaled_enrollment_id)
    .bind(user_id)
    .bind(scaled_program_id)
    .bind(&current.timezone)
    .bind(current.day_boundary_hour)
    .bind(start_date)
    .fetch_one(&mut **tx)
    .await?;

    sqlx::query(
        r#"
        insert into streak_states (enrollment_id)
        values ($1)
        "#,
    )
    .bind(scaled_enrollment_id)
    .execute(&mut **tx)
    .await?;

    Ok(scaled)
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

fn local_today(timezone: &str, boundary: i16) -> Result<NaiveDate, ApiError> {
    let tz = timezone.parse::<Tz>().map_err(|_| ApiError::InvalidUser)?;
    calendar::enrollment_today(Utc::now(), boundary as u32, tz).map_err(|_| ApiError::InvalidUser)
}

#[derive(FromRow)]
struct ProgramForScaleDown {
    title: String,
    summary: Option<String>,
    kind: String,
    duration_days: Option<i32>,
    intensity: Option<String>,
}

#[derive(FromRow)]
struct TemplateForScaleDown {
    id: Uuid,
    title: String,
    description: Option<String>,
    category: Option<String>,
    difficulty: i16,
    estimated_minutes: i32,
    cadence: Value,
    points: i32,
}

#[derive(FromRow)]
struct TemplatePerformance {
    template_id: Uuid,
    available_count: Option<i64>,
    completed_count: Option<i64>,
}

fn worst_template_ids(
    templates: &[TemplateForScaleDown],
    performance: &[TemplatePerformance],
    count: usize,
) -> Vec<Uuid> {
    let mut ranked = templates
        .iter()
        .map(|template| {
            let perf = performance
                .iter()
                .find(|item| item.template_id == template.id);
            let available = perf.and_then(|item| item.available_count).unwrap_or(0);
            let completed = perf.and_then(|item| item.completed_count).unwrap_or(0);
            let rate = if available <= 0 {
                1.0
            } else {
                completed as f64 / available as f64
            };
            (template.id, rate, available, template.title.as_str())
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.cmp(right.3))
    });

    ranked
        .into_iter()
        .take(count)
        .map(|(id, _, _, _)| id)
        .collect()
}

fn scaled_duration_days(
    original_duration: Option<i32>,
    original_start: NaiveDate,
    new_start: NaiveDate,
) -> Result<i32, ApiError> {
    let duration = original_duration.ok_or_else(|| {
        ApiError::BadRequest("standing programs cannot be scaled down".to_owned())
    })?;
    let elapsed = (new_start - original_start).num_days().max(0);
    let remaining = i64::from(duration) - elapsed;
    let scaled = remaining.clamp(1, i64::from(duration));
    i32::try_from(scaled)
        .map_err(|_| ApiError::BadRequest("scaled duration out of range".to_owned()))
}

fn scaled_summary(
    program: &ProgramForScaleDown,
    dropped_count: usize,
    kept_count: usize,
) -> Option<String> {
    let base = program.summary.as_deref().unwrap_or("Scaled return plan.");
    Some(format!(
        "{base} Scaled down after a lapse: dropped {dropped_count} low-completion task(s), kept {kept_count} task(s)."
    ))
}

fn adjust_cadence_for_duration(mut cadence: Value, duration_days: i32) -> Value {
    if cadence.get("type").and_then(Value::as_str) != Some("once") {
        return cadence;
    }
    let max_offset = i64::from(duration_days.saturating_sub(1));
    let current = cadence
        .get("day_offset")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .clamp(0, max_offset);
    if let Some(object) = cadence.as_object_mut() {
        object.insert("day_offset".to_owned(), Value::from(current));
    }
    cadence
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
