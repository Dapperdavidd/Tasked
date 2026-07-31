use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::IdempotencyKeyRow;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdempotencyLookup {
    Missing,
    Replay {
        status_code: i32,
        response_body: Value,
    },
    Conflict,
}

pub async fn lookup(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    key: &str,
    request_hash: &[u8],
) -> Result<IdempotencyLookup, sqlx::Error> {
    let Some(row) = sqlx::query_as::<_, IdempotencyKeyRow>(
        r#"
        select user_id, key, method, path, request_hash, status_code, response_body, created_at, expires_at
        from idempotency_keys
        where user_id = $1
          and key = $2
          and expires_at > now()
        "#,
    )
    .bind(user_id)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(IdempotencyLookup::Missing);
    };

    if row.request_hash == request_hash {
        Ok(IdempotencyLookup::Replay {
            status_code: row.status_code,
            response_body: row.response_body,
        })
    } else {
        Ok(IdempotencyLookup::Conflict)
    }
}

pub async fn store<T>(
    tx: &mut Transaction<'_, Postgres>,
    record: NewIdempotencyRecord<'_>,
    response_body: &T,
) -> Result<(), sqlx::Error>
where
    T: Serialize,
{
    let response_body = serde_json::to_value(response_body)
        .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;

    sqlx::query(
        r#"
        insert into idempotency_keys (
          user_id,
          key,
          method,
          path,
          request_hash,
          status_code,
          response_body,
          expires_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8)
        on conflict (user_id, key) do nothing
        "#,
    )
    .bind(record.user_id)
    .bind(record.key)
    .bind(record.method)
    .bind(record.path)
    .bind(record.request_hash)
    .bind(record.status_code)
    .bind(response_body)
    .bind(record.expires_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct NewIdempotencyRecord<'a> {
    pub user_id: Uuid,
    pub key: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub request_hash: &'a [u8],
    pub status_code: i32,
    pub expires_at: DateTime<Utc>,
}
