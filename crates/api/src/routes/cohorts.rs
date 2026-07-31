use actix_web::{get, post, web, HttpResponse};
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tracked_db::{cohorts as cohorts_db, rls};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct CreateCohortBody {
    program_id: Uuid,
    name: Option<String>,
    locked_start: Option<NaiveDate>,
}

#[post("/v1/cohorts")]
pub async fn create_cohort(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<CreateCohortBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        insert into cohorts (id, program_id, owner_id, name, locked_start)
        values ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(body.program_id)
    .bind(user_id.0)
    .bind(&body.name)
    .bind(body.locked_start)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(CohortResponse {
        id,
        program_id: body.program_id,
        name: body.name.clone(),
        locked_start: body.locked_start,
    }))
}

#[post("/v1/cohorts/{id}/invites")]
pub async fn create_invite(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let token = Uuid::now_v7().simple().to_string();
    let expires_at = Utc::now() + Duration::days(14);
    sqlx::query(
        r#"
        insert into cohort_invites (token, cohort_id, expires_at)
        values ($1, $2, $3)
        "#,
    )
    .bind(&token)
    .bind(*path)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(InviteResponse { token }))
}

#[derive(Deserialize)]
pub struct JoinCohortBody {
    token: String,
}

#[post("/v1/cohorts/join")]
pub async fn join_cohort(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<JoinCohortBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let cohort_id: Uuid = sqlx::query_scalar(
        r#"
        update cohort_invites
        set uses = uses + 1
        where token = $1
          and (expires_at is null or expires_at > now())
          and (max_uses is null or uses < max_uses)
        returning cohort_id
        "#,
    )
    .bind(&body.token)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(JoinResponse { cohort_id }))
}

#[derive(Deserialize)]
pub struct PresenceQuery {
    local_date: NaiveDate,
}

#[get("/v1/cohorts/{id}/presence")]
pub async fn presence(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
    query: web::Query<PresenceQuery>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let rows = cohorts_db::cohort_presence_on(&mut tx, *path, query.local_date).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(
        rows.into_iter()
            .map(|row| PresenceResponse {
                user_id: row.user_id,
                display_name: row.display_name,
                avatar_url: row.avatar_url,
                streak: row.streak,
                logged_today: row.logged_today,
            })
            .collect::<Vec<_>>(),
    ))
}

#[derive(Serialize)]
struct CohortResponse {
    id: Uuid,
    program_id: Uuid,
    name: Option<String>,
    locked_start: Option<NaiveDate>,
}

#[derive(Serialize)]
struct InviteResponse {
    token: String,
}

#[derive(Serialize)]
struct JoinResponse {
    cohort_id: Uuid,
}

#[derive(Serialize)]
struct PresenceResponse {
    user_id: Uuid,
    display_name: Option<String>,
    avatar_url: Option<String>,
    streak: i32,
    logged_today: bool,
}
