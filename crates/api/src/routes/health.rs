use actix_web::{get, HttpResponse, Responder};
use serde::Serialize;

#[get("/health")]
pub async fn health() -> impl Responder {
    HttpResponse::Ok().json(HealthResponse { ok: true })
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}
