use sqlx::PgPool;

#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
}
