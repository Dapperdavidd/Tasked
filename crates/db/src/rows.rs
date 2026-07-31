use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ProgramKind {
    Curriculum,
    Routine,
    Project,
    Standing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Intensity {
    Light,
    Standard,
    Heavy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentStatus {
    Active,
    Paused,
    Completed,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DayStatus {
    Open,
    Complete,
    Partial,
    Missed,
    Rest,
    Frozen,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StreakState {
    Active,
    AtRisk,
    Repairable,
    Broken,
}

#[derive(Clone, Debug, FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct ProgramRow {
    pub id: Uuid,
    pub author_id: Option<Uuid>,
    pub title: String,
    pub summary: Option<String>,
    pub kind: ProgramKind,
    pub duration_days: Option<i32>,
    pub intensity: Option<Intensity>,
    pub source_id: Option<Uuid>,
    pub share_titles: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct TaskTemplateRow {
    pub id: Uuid,
    pub program_id: Uuid,
    pub position: i32,
    pub title: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub difficulty: i16,
    pub estimated_minutes: i32,
    pub cadence: Value,
    pub points: i32,
    pub paused_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct EnrollmentRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub program_id: Uuid,
    pub cohort_id: Option<Uuid>,
    pub timezone: String,
    pub day_boundary_hour: i16,
    pub start_date: NaiveDate,
    pub is_standing: bool,
    pub status: EnrollmentStatus,
    pub materialised_through: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct DayRow {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub local_date: NaiveDate,
    pub day_index: i32,
    pub status: DayStatus,
    pub available_points: i32,
    pub earned_points: i32,
    pub note: Option<String>,
    pub opens_at: DateTime<Utc>,
    pub closes_at: DateTime<Utc>,
    pub finalised_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, FromRow)]
pub struct TaskInstanceRow {
    pub id: Uuid,
    pub day_id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub points: i32,
    pub position: i32,
    pub is_floating: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub skipped_reason: Option<String>,
    pub proof_kind: Option<String>,
    pub proof_value: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub struct JobRow {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub run_at: DateTime<Utc>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PushProvider {
    Expo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DevicePlatform {
    Ios,
    Android,
    Web,
}

#[derive(Clone, Debug, FromRow)]
pub struct IdempotencyKeyRow {
    pub user_id: Uuid,
    pub key: String,
    pub method: String,
    pub path: String,
    pub request_hash: Vec<u8>,
    pub status_code: i32,
    pub response_body: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct RestDayRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub local_date: NaiveDate,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct DeviceRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub push_provider: PushProvider,
    pub push_token: String,
    pub platform: Option<DevicePlatform>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, FromRow)]
pub struct CompletionArtifactRow {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub image_key: Option<String>,
    pub pdf_key: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub struct CohortPresenceRow {
    pub cohort_id: Uuid,
    pub user_id: Uuid,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub streak: i32,
    pub logged_today: bool,
}

#[derive(Clone, Debug, FromRow)]
pub struct StreakStateRow {
    pub enrollment_id: Uuid,
    pub current: i32,
    pub longest: i32,
    pub freezes: i16,
    pub clean_run: i16,
    pub last_counted_date: Option<NaiveDate>,
    pub repair_used_month: Option<NaiveDate>,
    pub state: StreakState,
}

#[derive(Clone, Debug, FromRow)]
pub struct TodaySectionRow {
    pub enrollment_id: Uuid,
    pub day_id: Uuid,
    pub program_id: Uuid,
    pub title: String,
    pub is_standing: bool,
    pub day_index: i32,
    pub duration_days: Option<i32>,
    pub day_status: DayStatus,
    pub available_points: i32,
    pub earned_points: i32,
    pub note: Option<String>,
    pub streak_current: i32,
    pub streak_longest: i32,
    pub streak_freezes: i16,
    pub streak_state: StreakState,
}

#[derive(Clone, Debug, FromRow)]
pub struct WeekBucketRow {
    pub enrollment_id: Uuid,
    pub iso_year: i32,
    pub iso_week: i32,
    pub template_id: Uuid,
    pub required: i32,
    pub completed: i32,
    pub points_each: i32,
}

#[derive(Clone, Debug, FromRow)]
pub struct TaskTemplateCompletionRow {
    pub template_id: Uuid,
    pub title: String,
    pub available_count: Option<i64>,
    pub completed_count: Option<i64>,
}
