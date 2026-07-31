use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::CompletionArtifactRow;

pub async fn upsert_completion_artifact<T>(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    image_key: Option<&str>,
    pdf_key: Option<&str>,
    payload: &T,
) -> Result<CompletionArtifactRow, sqlx::Error>
where
    T: Serialize,
{
    let id = Uuid::now_v7();
    let payload =
        serde_json::to_value(payload).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;

    sqlx::query_as::<_, CompletionArtifactRow>(
        r#"
        insert into completion_artifacts (
          id,
          enrollment_id,
          image_key,
          pdf_key,
          payload
        )
        values ($1, $2, $3, $4, $5)
        on conflict (enrollment_id)
        do update set
          image_key = excluded.image_key,
          pdf_key = excluded.pdf_key,
          payload = excluded.payload
        returning id, enrollment_id, image_key, pdf_key, payload, created_at
        "#,
    )
    .bind(id)
    .bind(enrollment_id)
    .bind(image_key)
    .bind(pdf_key)
    .bind(payload)
    .fetch_one(&mut **tx)
    .await
}

pub async fn get_completion_artifact(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
) -> Result<Option<CompletionArtifactRow>, sqlx::Error> {
    sqlx::query_as::<_, CompletionArtifactRow>(
        r#"
        select id, enrollment_id, image_key, pdf_key, payload, created_at
        from completion_artifacts
        where enrollment_id = $1
        "#,
    )
    .bind(enrollment_id)
    .fetch_optional(&mut **tx)
    .await
}
