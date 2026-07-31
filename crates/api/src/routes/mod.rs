use actix_web::web;

pub mod health;
pub mod standing;
pub mod tasks;
pub mod today;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(health::health)
        .service(today::today)
        .service(tasks::complete_task)
        .service(tasks::uncomplete_task)
        .service(tasks::skip_task)
        .service(standing::get_standing);
}
