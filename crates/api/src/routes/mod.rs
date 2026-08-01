use actix_web::web;

pub mod cohorts;
pub mod days;
pub mod devices;
pub mod enrollments;
pub mod extract;
pub mod health;
pub mod ingest;
pub mod notifications;
pub mod programs;
pub mod rest_days;
pub mod sessions;
pub mod standing;
pub mod stats;
pub mod sync;
pub mod tasks;
pub mod today;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(health::health)
        .service(sessions::create_session)
        .service(extract::extract_source)
        .service(ingest::create_ingest)
        .service(ingest::get_ingest)
        .service(ingest::ingest_events)
        .service(programs::create_program)
        .service(sync::sync)
        .service(today::today)
        .service(tasks::complete_task)
        .service(tasks::uncomplete_task)
        .service(tasks::skip_task)
        .service(standing::get_standing)
        .service(standing::create_standing)
        .service(standing::pause_standing)
        .service(rest_days::declare_rest_day)
        .service(rest_days::delete_rest_day)
        .service(devices::register_device)
        .service(devices::disable_device)
        .service(notifications::enqueue_test_notification)
        .service(notifications::list_notifications)
        .service(stats::stats)
        .service(cohorts::create_cohort)
        .service(cohorts::create_invite)
        .service(cohorts::join_cohort)
        .service(cohorts::presence)
        .service(days::get_days)
        .service(days::patch_day)
        .service(days::repair_day)
        .service(enrollments::list_enrollments)
        .service(enrollments::enrollment_summary)
        .service(enrollments::return_enrollment)
        .service(enrollments::patch_enrollment);
}
