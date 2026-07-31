use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::rows::JobRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobRetryPolicy {
    pub max_attempts: i32,
    pub delay_seconds: i64,
}

impl Default for JobRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            delay_seconds: 60,
        }
    }
}

pub async fn enqueue_job(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    payload: impl Serialize,
    run_at: DateTime<Utc>,
    policy: JobRetryPolicy,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    let payload =
        serde_json::to_value(payload).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;

    sqlx::query(
        r#"
        insert into jobs (id, kind, payload, run_at, max_attempts)
        values ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(kind)
    .bind(payload)
    .bind(run_at)
    .bind(policy.max_attempts)
    .execute(&mut **tx)
    .await?;

    Ok(id)
}

pub async fn claim_next_job(
    pool: &PgPool,
    lock_for: Duration,
) -> Result<Option<JobRow>, sqlx::Error> {
    sqlx::query_as::<_, JobRow>(
        r#"
        update jobs
        set locked_until = now() + ($1::bigint * interval '1 second'),
            attempts = attempts + 1
        where id = (
          select id
          from jobs
          where failed_at is null
            and run_at <= now()
            and attempts < max_attempts
            and (locked_until is null or locked_until < now())
          order by run_at
          for update skip locked
          limit 1
        )
        returning id, kind, payload, run_at, attempts, max_attempts, locked_until, failed_at, last_error
        "#,
    )
    .bind(lock_for.num_seconds())
    .fetch_optional(pool)
    .await
}

pub async fn complete_job(
    tx: &mut Transaction<'_, Postgres>,
    job_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from jobs where id = $1")
        .bind(job_id)
        .execute(&mut **tx)
        .await?;

    Ok(())
}

pub async fn fail_job(
    tx: &mut Transaction<'_, Postgres>,
    job: &JobRow,
    error: &str,
    policy: JobRetryPolicy,
) -> Result<(), sqlx::Error> {
    let should_stop = job.attempts >= job.max_attempts;

    if should_stop {
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
        .bind(error)
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query(
            r#"
            update jobs
            set run_at = now() + ($2::bigint * interval '1 second'),
                locked_until = null,
                last_error = $3
            where id = $1
            "#,
        )
        .bind(job.id)
        .bind(policy.delay_seconds)
        .bind(error)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

pub fn payload_as<T>(job: &JobRow) -> Result<T, serde_json::Error>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<T>(job.payload.clone())
}

pub fn raw_payload(job: &JobRow) -> &Value {
    &job.payload
}
