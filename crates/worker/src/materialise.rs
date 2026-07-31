use chrono::{Duration, NaiveDate};
use chrono_tz::Tz;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use tracked_core::{cadence::Cadence, calendar};
use tracked_db::{materialise, rows::EnrollmentRow};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum MaterialiseError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("calendar error: {0:?}")]
    Calendar(calendar::CalendarError),
    #[error("invalid timezone: {0}")]
    InvalidTimezone(String),
    #[error("invalid cadence: {0}")]
    InvalidCadence(String),
    #[error("date out of range")]
    DateOutOfRange,
    #[error("day index out of range")]
    DayIndexOutOfRange,
}

pub async fn materialise_due(
    pool: &PgPool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<u64, MaterialiseError> {
    let mut tx = pool.begin().await?;
    let enrollments = materialise::active_enrollments(&mut tx).await?;
    let mut materialised = 0_u64;

    for enrollment in enrollments {
        materialised += materialise_enrollment(&mut tx, &enrollment, now).await?;
    }

    tx.commit().await?;
    Ok(materialised)
}

async fn materialise_enrollment(
    tx: &mut Transaction<'_, Postgres>,
    enrollment: &EnrollmentRow,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<u64, MaterialiseError> {
    let tz = parse_tz(&enrollment.timezone)?;
    let today = calendar::enrollment_today(now, enrollment.day_boundary_hour as u32, tz)
        .map_err(MaterialiseError::Calendar)?;
    let target = today
        .checked_add_signed(Duration::days(2))
        .ok_or(MaterialiseError::DateOutOfRange)?;
    let start = next_date_to_materialise(enrollment);

    if start > target {
        return Ok(0);
    }

    let duration_days = program_duration_days(tx, enrollment.program_id).await?;
    let templates = materialise::active_templates_for_program(tx, enrollment.program_id).await?;
    let mut cursor = start;
    let mut count = 0_u64;

    while cursor <= target {
        let day_index = day_index(enrollment.start_date, cursor)?;
        if !enrollment.is_standing && duration_days.is_some_and(|duration| day_index >= duration) {
            mark_enrollment_completed(tx, enrollment.id).await?;
            break;
        }

        let (opens_at, closes_at) =
            calendar::day_window(cursor, enrollment.day_boundary_hour as u32, tz)
                .map_err(MaterialiseError::Calendar)?;
        let day = materialise::upsert_day(
            tx,
            materialise::NewDay {
                id: Uuid::now_v7(),
                enrollment_id: enrollment.id,
                local_date: cursor,
                day_index,
                opens_at,
                closes_at,
            },
        )
        .await?;

        for template in &templates {
            let cadence = parse_cadence(&template.cadence)?;
            if cadence.fires_on(day_index as u32, cursor) {
                materialise::upsert_task_instance(
                    tx,
                    materialise::NewTaskInstance {
                        id: Uuid::now_v7(),
                        day_id: day.id,
                        template_id: template.id,
                        title: &template.title,
                        points: template.points,
                        position: template.position,
                        is_floating: matches!(cadence, Cadence::NPerWeek { .. }),
                    },
                )
                .await?;
            }
        }

        materialise::refresh_day_available_points(tx, day.id).await?;
        materialise::mark_materialised_through(tx, enrollment.id, cursor).await?;
        count += 1;
        cursor = cursor.succ_opt().ok_or(MaterialiseError::DateOutOfRange)?;
    }

    Ok(count)
}

fn next_date_to_materialise(enrollment: &EnrollmentRow) -> NaiveDate {
    enrollment
        .materialised_through
        .and_then(|date| date.succ_opt())
        .unwrap_or(enrollment.start_date)
}

fn day_index(start_date: NaiveDate, local_date: NaiveDate) -> Result<i32, MaterialiseError> {
    let days = local_date.signed_duration_since(start_date).num_days();
    i32::try_from(days).map_err(|_| MaterialiseError::DayIndexOutOfRange)
}

fn parse_tz(value: &str) -> Result<Tz, MaterialiseError> {
    value
        .parse::<Tz>()
        .map_err(|_| MaterialiseError::InvalidTimezone(value.to_owned()))
}

fn parse_cadence(value: &Value) -> Result<Cadence, MaterialiseError> {
    let cadence_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| MaterialiseError::InvalidCadence("missing type".to_owned()))?;

    match cadence_type {
        "daily" => Ok(Cadence::Daily),
        "weekly_days" => {
            let days = value
                .get("days")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    MaterialiseError::InvalidCadence("weekly_days.days missing".to_owned())
                })?
                .iter()
                .map(|day| {
                    day.as_u64()
                        .and_then(|day| u8::try_from(day).ok())
                        .ok_or_else(|| {
                            MaterialiseError::InvalidCadence("invalid weekday".to_owned())
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Cadence::WeeklyDays { days })
        }
        "n_per_week" => {
            let count = value
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|count| u8::try_from(count).ok())
                .ok_or_else(|| {
                    MaterialiseError::InvalidCadence("n_per_week.count missing".to_owned())
                })?;
            Ok(Cadence::NPerWeek { count })
        }
        "once" => {
            let day_offset = value
                .get("day_offset")
                .and_then(Value::as_u64)
                .and_then(|offset| u32::try_from(offset).ok())
                .ok_or_else(|| {
                    MaterialiseError::InvalidCadence("once.day_offset missing".to_owned())
                })?;
            Ok(Cadence::Once { day_offset })
        }
        other => Err(MaterialiseError::InvalidCadence(format!(
            "unknown cadence type {other}"
        ))),
    }
}

async fn program_duration_days(
    tx: &mut Transaction<'_, Postgres>,
    program_id: Uuid,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar("select duration_days from programs where id = $1")
        .bind(program_id)
        .fetch_one(&mut **tx)
        .await
}

async fn mark_enrollment_completed(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("update enrollments set status = 'completed' where id = $1")
        .bind(enrollment_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_cadence_shapes_from_db_json() {
        assert_eq!(
            parse_cadence(&json!({"type":"daily"})).expect("valid"),
            Cadence::Daily
        );
        assert_eq!(
            parse_cadence(&json!({"type":"weekly_days","days":[1,3,5]})).expect("valid"),
            Cadence::WeeklyDays {
                days: vec![1, 3, 5]
            }
        );
        assert_eq!(
            parse_cadence(&json!({"type":"n_per_week","count":3})).expect("valid"),
            Cadence::NPerWeek { count: 3 }
        );
        assert_eq!(
            parse_cadence(&json!({"type":"once","day_offset":12})).expect("valid"),
            Cadence::Once { day_offset: 12 }
        );
    }

    #[test]
    fn next_date_starts_at_start_date_when_never_materialised() {
        let start_date = NaiveDate::from_ymd_opt(2026, 5, 24).expect("valid");
        let enrollment = EnrollmentRow {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            program_id: Uuid::now_v7(),
            cohort_id: None,
            timezone: "Africa/Lagos".to_owned(),
            day_boundary_hour: 0,
            start_date,
            is_standing: false,
            status: tracked_db::rows::EnrollmentStatus::Active,
            materialised_through: None,
            created_at: chrono::Utc::now(),
        };

        assert_eq!(next_date_to_materialise(&enrollment), start_date);
    }
}
