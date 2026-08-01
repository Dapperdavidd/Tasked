use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use tracked_ingest::{
    calibrate, generate, normalise, Extracted, GeneratedProgram, GeneratedTask, Intensity,
    ProgramKind, SourceKind,
};
use uuid::Uuid;

const INGEST_JOB_KIND: &str = "ingest_process";
const DEFAULT_DURATION_DAYS: u16 = 28;
const STARTER_DURATION_DAYS: u16 = 14;
const DEFAULT_TASK_MINUTES: u16 = 30;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("payload error: {0}")]
    Payload(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IngestPayload {
    ingestion_job_id: Uuid,
}

#[derive(Clone, Debug, FromRow)]
struct ClaimedJob {
    id: Uuid,
    attempts: i32,
    max_attempts: i32,
    payload: serde_json::Value,
}

#[derive(Clone, Debug, FromRow)]
struct IngestSource {
    source_id: Uuid,
    mime_type: Option<String>,
    extracted_text: String,
    instruction: Option<String>,
    intensity: String,
}

#[derive(Clone, Debug)]
struct HeuristicClassification {
    kind: ProgramKind,
    confidence: f32,
    suggested_duration_days: Option<u16>,
}

pub async fn process_due(pool: &PgPool, max_jobs: u32) -> Result<u32, IngestError> {
    let mut processed = 0_u32;

    for _ in 0..max_jobs {
        let Some(job) = claim_next_ingest_job(pool).await? else {
            break;
        };
        process_one(pool, job).await?;
        processed += 1;
    }

    Ok(processed)
}

pub async fn process_ingestion_job(
    pool: &PgPool,
    ingestion_job_id: Uuid,
) -> Result<bool, IngestError> {
    let Some(job) = claim_ingest_job(pool, ingestion_job_id).await? else {
        return Ok(false);
    };
    process_one(pool, job).await?;
    Ok(true)
}

async fn process_one(pool: &PgPool, job: ClaimedJob) -> Result<(), IngestError> {
    let payload: IngestPayload = serde_json::from_value(job.payload.clone())?;
    let mut tx = pool.begin().await?;

    let Some(source) = load_source(&mut tx, payload.ingestion_job_id).await? else {
        release_job(&mut tx, job.id).await?;
        tx.commit().await?;
        return Ok(());
    };

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "normalising",
        None,
        None,
        None,
    )
    .await?;

    let source_kind = source
        .mime_type
        .as_deref()
        .and_then(SourceKind::from_mime)
        .unwrap_or(SourceKind::Text);
    let normalised = match normalise(
        &Extracted {
            text: source.extracted_text.clone(),
            pages: None,
        },
        source_kind,
    ) {
        Ok(normalised) => normalised,
        Err(error) => {
            fail_ingestion(
                &mut tx,
                payload.ingestion_job_id,
                job,
                ingest_error_code(&error.to_string()),
                &error.to_string(),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
    };

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "classifying",
        None,
        None,
        None,
    )
    .await?;
    let classification = classify_source(&normalised.text, source.instruction.as_deref());

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "generating",
        None,
        None,
        None,
    )
    .await?;
    let generated = generate_program(
        &normalised.text,
        source.instruction.as_deref(),
        &classification,
        intensity_from_db(&source.intensity),
    );

    let validated = match generate::validate(generated) {
        Ok(validated) => validated,
        Err(error) => {
            fail_ingestion(
                &mut tx,
                payload.ingestion_job_id,
                job,
                "invalid_generated_program",
                &error.to_string(),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
    };

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "calibrating",
        None,
        None,
        None,
    )
    .await?;
    let intensity = intensity_from_db(&source.intensity);
    let calibration = calibrate(
        validated.program.tasks.clone(),
        validated.program.duration_days,
        intensity,
    );

    let mut ready = validated.program;
    let mut warnings = validated.warnings;
    warnings.extend(calibration.warnings);
    ready.duration_days = calibration.duration_days;
    ready.tasks = calibration.tasks;
    ready.warnings = warnings.clone();

    update_status(
        &mut tx,
        payload.ingestion_job_id,
        "ready",
        Some(serde_json::to_value(&ready)?),
        Some(serde_json::to_value(&warnings)?),
        None,
    )
    .await?;
    sqlx::query(
        r#"
        update source_documents
        set content_hash = $2,
            extracted_text = $3
        where id = $1
        "#,
    )
    .bind(source.source_id)
    .bind(normalised.content_hash.as_slice())
    .bind(&normalised.text)
    .execute(&mut *tx)
    .await?;
    release_job(&mut tx, job.id).await?;
    tx.commit().await?;

    Ok(())
}

async fn claim_ingest_job(
    pool: &PgPool,
    ingestion_job_id: Uuid,
) -> Result<Option<ClaimedJob>, sqlx::Error> {
    sqlx::query_as::<_, ClaimedJob>(
        r#"
        update jobs
        set locked_until = now() + interval '5 minutes',
            attempts = attempts + 1
        where id = (
          select id
          from jobs
          where failed_at is null
            and kind = $1
            and payload->>'ingestion_job_id' = $2
            and attempts < max_attempts
            and (locked_until is null or locked_until < now())
          order by run_at
          for update skip locked
          limit 1
        )
        returning id, attempts, max_attempts, payload
        "#,
    )
    .bind(INGEST_JOB_KIND)
    .bind(ingestion_job_id.to_string())
    .fetch_optional(pool)
    .await
}

async fn claim_next_ingest_job(pool: &PgPool) -> Result<Option<ClaimedJob>, sqlx::Error> {
    sqlx::query_as::<_, ClaimedJob>(
        r#"
        update jobs
        set locked_until = now() + interval '5 minutes',
            attempts = attempts + 1
        where id = (
          select id
          from jobs
          where failed_at is null
            and kind = $1
            and run_at <= now()
            and attempts < max_attempts
            and (locked_until is null or locked_until < now())
          order by run_at
          for update skip locked
          limit 1
        )
        returning id, attempts, max_attempts, payload
        "#,
    )
    .bind(INGEST_JOB_KIND)
    .fetch_optional(pool)
    .await
}

async fn load_source(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion_job_id: Uuid,
) -> Result<Option<IngestSource>, sqlx::Error> {
    sqlx::query_as::<_, IngestSource>(
        r#"
        select sd.id as source_id, sd.mime_type, sd.extracted_text, ij.instruction, ij.intensity
        from ingestion_jobs ij
        join source_documents sd on sd.id = ij.source_id
        where ij.id = $1
        "#,
    )
    .bind(ingestion_job_id)
    .fetch_optional(&mut **tx)
    .await
}

async fn update_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion_job_id: Uuid,
    status: &str,
    draft: Option<serde_json::Value>,
    warnings: Option<serde_json::Value>,
    error_code: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        update ingestion_jobs
        set status = $2,
            draft = coalesce($3, draft),
            warnings = coalesce($4, warnings),
            error_code = $5
        where id = $1
        "#,
    )
    .bind(ingestion_job_id)
    .bind(status)
    .bind(draft)
    .bind(warnings)
    .bind(error_code)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn fail_ingestion(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion_job_id: Uuid,
    job: ClaimedJob,
    error_code: &str,
    last_error: &str,
) -> Result<(), sqlx::Error> {
    let stop = job.attempts >= job.max_attempts;

    sqlx::query(
        r#"
        update ingestion_jobs
        set status = 'failed',
            error_code = $2
        where id = $1
        "#,
    )
    .bind(ingestion_job_id)
    .bind(error_code)
    .execute(&mut **tx)
    .await?;

    if stop {
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
        .bind(last_error)
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query(
            r#"
            update jobs
            set run_at = now() + interval '60 seconds',
                locked_until = null,
                last_error = $2
            where id = $1
            "#,
        )
        .bind(job.id)
        .bind(last_error)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

async fn release_job(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from jobs where id = $1")
        .bind(job_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn classify_source(text: &str, instruction: Option<&str>) -> HeuristicClassification {
    let combined = format!("{text}\n{}", instruction.unwrap_or_default()).to_lowercase();
    let suggested_duration_days = infer_duration_days(&combined);

    if has_routine_markers(&combined) {
        return HeuristicClassification {
            kind: ProgramKind::Routine,
            confidence: 0.72,
            suggested_duration_days,
        };
    }

    if has_curriculum_markers(&combined) {
        return HeuristicClassification {
            kind: ProgramKind::Curriculum,
            confidence: 0.7,
            suggested_duration_days,
        };
    }

    HeuristicClassification {
        kind: ProgramKind::Project,
        confidence: 0.62,
        suggested_duration_days,
    }
}

fn generate_program(
    text: &str,
    instruction: Option<&str>,
    classification: &HeuristicClassification,
    intensity: Intensity,
) -> GeneratedProgram {
    let lines = extract_task_lines(text);
    let is_starter_prompt = should_expand_prompt(text, instruction, &lines);
    let inferred_duration = if is_starter_prompt {
        classification
            .suggested_duration_days
            .unwrap_or(STARTER_DURATION_DAYS)
    } else {
        classification
            .suggested_duration_days
            .unwrap_or_else(|| default_duration_for_tasks(classification.kind, lines.len()))
    }
    .clamp(1, 730);
    let tasks = if is_starter_prompt {
        starter_plan_tasks(text, instruction, intensity, inferred_duration)
    } else if lines.is_empty() {
        vec![GeneratedTask {
            title: fallback_title(text),
            description: compact_description(text),
            category: None,
            difficulty: 2,
            estimated_minutes: intensity.daily_cap_minutes().min(45),
            cadence: tracked_core::cadence::Cadence::Daily,
        }]
    } else {
        lines
            .iter()
            .enumerate()
            .map(|(index, line)| task_from_line(line, index, classification.kind))
            .collect()
    };

    GeneratedProgram {
        title: infer_title(text, instruction, classification.kind),
        summary: instruction
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                compact_description(text).unwrap_or_else(|| "Generated plan".to_owned())
            }),
        kind: classification.kind,
        duration_days: inferred_duration,
        confidence: classification.confidence,
        tasks,
        warnings: Vec::new(),
    }
}

fn should_expand_prompt(text: &str, instruction: Option<&str>, lines: &[String]) -> bool {
    if lines.len() > 1 {
        return false;
    }

    let combined = format!("{text}\n{}", instruction.unwrap_or_default()).to_lowercase();
    let word_count = combined.split_whitespace().count();
    let has_schedule_markers = [
        "day 1",
        "week 1",
        "module",
        "chapter",
        "lesson",
        "session",
        "every monday",
        "mon/wed/fri",
    ]
    .iter()
    .any(|marker| combined.contains(marker));

    word_count <= 18 && !has_schedule_markers
}

fn starter_plan_tasks(
    text: &str,
    instruction: Option<&str>,
    intensity: Intensity,
    duration_days: u16,
) -> Vec<GeneratedTask> {
    let combined = format!("{text}\n{}", instruction.unwrap_or_default()).to_lowercase();
    let include_rust = contains_any(
        &combined,
        &["rust", "rustlang", "borrow checker", "cargo", "ownership"],
    );
    let include_fitness = contains_any(
        &combined,
        &["fitness", "workout", "gym", "run", "exercise", "health"],
    );
    let include_productivity = contains_any(
        &combined,
        &[
            "productivity",
            "focus",
            "work",
            "study",
            "routine",
            "discipline",
        ],
    );

    let titles: Vec<(&str, &str)> = if include_rust {
        vec![
            ("Install Rust and run Hello World with Cargo", "rust"),
            ("Learn variables, mutability, and basic types", "rust"),
            ("Practice functions, expressions, and control flow", "rust"),
            ("Understand ownership with move examples", "rust"),
            ("Practice borrowing and references", "rust"),
            ("Use structs, methods, and associated functions", "rust"),
            ("Model choices with enums and match", "rust"),
            ("Work with vectors, strings, and iterators", "rust"),
            ("Handle errors with Option and Result", "rust"),
            ("Build a small command-line Rust project", "rust"),
            ("Organize code with modules and packages", "rust"),
            ("Read files and parse simple input", "rust"),
            ("Write tests for your Rust code", "rust"),
            ("Refactor and document the final project", "rust"),
        ]
    } else {
        match (include_fitness, include_productivity) {
            (true, true) => vec![
                ("Set your baseline and plan tomorrow", "fitness"),
                ("Complete a mobility walk and clear one priority", "fitness"),
                (
                    "Do a beginner strength circuit and a 25-minute focus block",
                    "fitness",
                ),
                ("Stretch, hydrate, and reset your workspace", "recovery"),
                ("Walk briskly and finish one small deliverable", "fitness"),
                ("Train core basics and review your week", "fitness"),
                ("Take an active recovery walk and plan meals", "recovery"),
                (
                    "Repeat the strength circuit and protect one deep-work block",
                    "fitness",
                ),
                (
                    "Add intervals to your walk and remove one distraction",
                    "fitness",
                ),
                ("Do mobility work and batch tomorrow's tasks", "recovery"),
                (
                    "Complete a longer beginner workout and ship one task",
                    "fitness",
                ),
                ("Stretch, reflect, and simplify your task list", "recovery"),
                ("Retest your baseline and compare progress", "fitness"),
                ("Build your next weekly routine", "productivity"),
            ],
            (true, false) => vec![
                ("Set your movement baseline", "fitness"),
                ("Complete a brisk walk and stretch", "fitness"),
                ("Do a beginner strength circuit", "fitness"),
                ("Practice mobility and recovery breathing", "recovery"),
                ("Walk with short easy intervals", "fitness"),
                ("Train core stability basics", "fitness"),
                ("Take an active recovery walk", "recovery"),
                ("Repeat the strength circuit", "fitness"),
                ("Add gentle cardio intervals", "fitness"),
                ("Stretch hips, back, and shoulders", "recovery"),
                ("Complete a full beginner workout", "fitness"),
                ("Do light recovery and hydration prep", "recovery"),
                ("Retest your baseline", "fitness"),
                ("Choose your next fitness target", "fitness"),
            ],
            (false, true) => vec![
                ("Audit your current routine", "productivity"),
                ("Pick one priority and clear your workspace", "productivity"),
                ("Run a 25-minute focus block", "productivity"),
                ("Create a simple task capture system", "productivity"),
                ("Batch small tasks into one session", "productivity"),
                ("Protect a no-distraction work block", "productivity"),
                ("Review the week and remove one blocker", "productivity"),
                ("Plan tomorrow before ending work", "productivity"),
                ("Deepen one focus block", "productivity"),
                ("Clean up your calendar and commitments", "productivity"),
                ("Finish one visible deliverable", "productivity"),
                ("Automate or template one repeated task", "productivity"),
                ("Retest your routine under real conditions", "productivity"),
                ("Design your next weekly operating system", "productivity"),
            ],
            (false, false) => vec![
                ("Define the goal and success measure", "planning"),
                ("Break the goal into small milestones", "planning"),
                ("Complete the first starter task", "execution"),
                ("Review friction and adjust the plan", "review"),
                ("Build the second practical step", "execution"),
                ("Practice the core skill for one block", "execution"),
                ("Review progress and simplify scope", "review"),
                ("Repeat the highest-value action", "execution"),
                ("Add one small challenge", "execution"),
                ("Document what is working", "review"),
                ("Finish a useful mini deliverable", "execution"),
                ("Clean up loose ends", "execution"),
                ("Retest the success measure", "review"),
                ("Plan the next cycle", "planning"),
            ],
        }
    };

    expand_titles(titles, usize::from(duration_days))
        .into_iter()
        .enumerate()
        .map(|(index, (title, category))| GeneratedTask {
            title: title.to_owned(),
            description: None,
            category: Some(category.to_owned()),
            difficulty: starter_difficulty(index),
            estimated_minutes: starter_minutes(intensity, index),
            cadence: tracked_core::cadence::Cadence::Once {
                day_offset: u32::try_from(index).unwrap_or(u32::MAX),
            },
        })
        .collect()
}

fn expand_titles<'a>(
    titles: Vec<(&'a str, &'a str)>,
    duration_days: usize,
) -> Vec<(&'a str, &'a str)> {
    if duration_days <= titles.len() {
        return titles.into_iter().take(duration_days).collect();
    }

    let mut expanded = Vec::with_capacity(duration_days);
    while expanded.len() < duration_days {
        for item in &titles {
            if expanded.len() == duration_days {
                break;
            }
            expanded.push(*item);
        }
    }
    expanded
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn starter_difficulty(index: usize) -> u8 {
    match index {
        0..=3 => 1,
        4..=9 => 2,
        10..=12 => 3,
        _ => 2,
    }
}

fn starter_minutes(intensity: Intensity, index: usize) -> u16 {
    let base = match intensity {
        Intensity::Light => 15,
        Intensity::Standard => 30,
        Intensity::Heavy => 45,
    };
    let extra = if index >= 10 {
        10
    } else if index >= 4 {
        5
    } else {
        0
    };
    (base + extra).min(intensity.daily_cap_minutes())
}

fn task_from_line(line: &str, index: usize, kind: ProgramKind) -> GeneratedTask {
    let lowered = line.to_lowercase();
    let estimated_minutes = infer_minutes(line);
    let difficulty = infer_difficulty(estimated_minutes, &lowered);
    let cadence = match kind {
        ProgramKind::Routine => infer_routine_cadence(&lowered, index),
        ProgramKind::Curriculum | ProgramKind::Project => tracked_core::cadence::Cadence::Once {
            day_offset: u32::try_from(index).unwrap_or(u32::MAX),
        },
    };

    GeneratedTask {
        title: compact_title(line),
        description: None,
        category: infer_category(&lowered),
        difficulty,
        estimated_minutes,
        cadence,
    }
}

fn infer_title(text: &str, instruction: Option<&str>, kind: ProgramKind) -> String {
    if let Some(instruction) = instruction.map(str::trim).filter(|value| !value.is_empty()) {
        return compact_title(instruction);
    }

    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let prefix = match kind {
        ProgramKind::Curriculum => "Curriculum",
        ProgramKind::Routine => "Routine",
        ProgramKind::Project => "Project",
    };
    let candidate = compact_title(first);
    if candidate.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}: {candidate}")
    }
}

fn fallback_title(text: &str) -> String {
    compact_title(text.lines().next().unwrap_or("Build plan"))
}

fn compact_title(value: &str) -> String {
    let cleaned = strip_list_marker(value)
        .trim()
        .trim_start_matches(|character: char| {
            matches!(character, '-' | '*' | '•' | '[' | ']' | '(' | ')')
        })
        .trim();
    let mut title = cleaned
        .split(':')
        .next()
        .unwrap_or(cleaned)
        .trim()
        .to_owned();
    if title.len() > 80 {
        title.truncate(80);
    }
    if title.is_empty() {
        "Untitled task".to_owned()
    } else {
        title
    }
}

fn strip_list_marker(value: &str) -> &str {
    let trimmed = value.trim_start();
    let marker_len = trimmed
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit())
        .last()
        .map(|(index, character)| index + character.len_utf8());
    let Some(marker_len) = marker_len else {
        return trimmed;
    };
    let rest = &trimmed[marker_len..];
    if let Some(stripped) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
        stripped.trim_start()
    } else {
        trimmed
    }
}

fn compact_description(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed.chars().take(160).collect())
    }
}

fn extract_task_lines(text: &str) -> Vec<String> {
    let bullet_lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| line.starts_with('-') || line.starts_with('*') || line.starts_with('•'))
        .map(ToOwned::to_owned)
        .collect();
    if !bullet_lines.is_empty() {
        return bullet_lines;
    }

    let numbered: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| is_numbered_list_item(line))
        .map(ToOwned::to_owned)
        .collect();
    if !numbered.is_empty() {
        return numbered;
    }

    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(12)
        .map(ToOwned::to_owned)
        .collect()
}

fn is_numbered_list_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    let digit_len = trimmed
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit())
        .last()
        .map(|(index, character)| index + character.len_utf8());
    let Some(digit_len) = digit_len else {
        return false;
    };
    trimmed[digit_len..]
        .chars()
        .next()
        .is_some_and(|character| character == '.' || character == ')')
}

fn infer_duration_days(text: &str) -> Option<u16> {
    let words = text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for token in words.windows(2) {
        let [amount, unit] = token else {
            continue;
        };
        let Ok(number) = amount.parse::<u16>() else {
            continue;
        };
        if unit.starts_with("week") || unit.starts_with("weeks") {
            return Some(number.saturating_mul(7).clamp(1, 730));
        }
        if unit.starts_with("day") || unit.starts_with("days") {
            return Some(number.clamp(1, 730));
        }
    }
    None
}

fn infer_minutes(line: &str) -> u16 {
    let words = line
        .split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for window in words.windows(2) {
        let [amount, unit] = window else {
            continue;
        };
        let Ok(number) = amount.parse::<u16>() else {
            continue;
        };
        if unit.eq_ignore_ascii_case("min")
            || unit.eq_ignore_ascii_case("mins")
            || unit.eq_ignore_ascii_case("minute")
            || unit.eq_ignore_ascii_case("minutes")
        {
            return number.clamp(1, 480);
        }
        if unit.eq_ignore_ascii_case("hour")
            || unit.eq_ignore_ascii_case("hours")
            || unit.eq_ignore_ascii_case("hr")
            || unit.eq_ignore_ascii_case("hrs")
        {
            return number.saturating_mul(60).clamp(1, 480);
        }
    }

    DEFAULT_TASK_MINUTES
}

fn infer_difficulty(minutes: u16, lowered: &str) -> u8 {
    if lowered.contains("advanced") || lowered.contains("exam") || minutes >= 90 {
        4
    } else if lowered.contains("project") || lowered.contains("build") || minutes >= 60 {
        3
    } else {
        2
    }
}

fn infer_category(lowered: &str) -> Option<String> {
    let category = if lowered.contains("run")
        || lowered.contains("workout")
        || lowered.contains("gym")
        || lowered.contains("stretch")
    {
        "fitness"
    } else if lowered.contains("read")
        || lowered.contains("study")
        || lowered.contains("chapter")
        || lowered.contains("lesson")
    {
        "study"
    } else if lowered.contains("build")
        || lowered.contains("ship")
        || lowered.contains("code")
        || lowered.contains("deploy")
    {
        "project"
    } else {
        return None;
    };

    Some(category.to_owned())
}

fn infer_routine_cadence(lowered: &str, index: usize) -> tracked_core::cadence::Cadence {
    if lowered.contains("daily") || lowered.contains("every day") {
        return tracked_core::cadence::Cadence::Daily;
    }
    if lowered.contains("3x") || lowered.contains("3 times") {
        return tracked_core::cadence::Cadence::NPerWeek { count: 3 };
    }
    if lowered.contains("2x") || lowered.contains("2 times") {
        return tracked_core::cadence::Cadence::NPerWeek { count: 2 };
    }

    let weekdays = [
        ("monday", 1_u8),
        ("tuesday", 2_u8),
        ("wednesday", 3_u8),
        ("thursday", 4_u8),
        ("friday", 5_u8),
        ("saturday", 6_u8),
        ("sunday", 7_u8),
    ];
    let matched = weekdays
        .iter()
        .filter_map(|(name, day)| lowered.contains(name).then_some(*day))
        .collect::<Vec<_>>();
    if !matched.is_empty() {
        return tracked_core::cadence::Cadence::WeeklyDays { days: matched };
    }

    if lowered.contains("weekday") {
        return tracked_core::cadence::Cadence::WeeklyDays {
            days: vec![1, 2, 3, 4, 5],
        };
    }

    tracked_core::cadence::Cadence::Once {
        day_offset: u32::try_from(index).unwrap_or(u32::MAX),
    }
}

fn default_duration_for_tasks(kind: ProgramKind, task_count: usize) -> u16 {
    match kind {
        ProgramKind::Routine => 28,
        ProgramKind::Curriculum | ProgramKind::Project => {
            u16::try_from(task_count.max(1)).unwrap_or(DEFAULT_DURATION_DAYS)
        }
    }
}

fn has_routine_markers(text: &str) -> bool {
    [
        "daily",
        "every day",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
        "weekdays",
        "3x",
        "2x",
        "times a week",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn has_curriculum_markers(text: &str) -> bool {
    if (text.contains("learning plan") || text.contains("study plan") || text.contains("course"))
        && (text.contains("learn") || text.contains("beginner") || text.contains("study"))
    {
        return true;
    }

    [
        "day 1",
        "week 1",
        "chapter 1",
        "lesson 1",
        "module 1",
        "session 1",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn intensity_from_db(value: &str) -> Intensity {
    match value {
        "light" => Intensity::Light,
        "heavy" => Intensity::Heavy,
        _ => Intensity::Standard,
    }
}

fn ingest_error_code(message: &str) -> &str {
    if message.contains("scan") {
        "needs_ocr"
    } else if message.contains("usable text") {
        "empty_source"
    } else {
        "ingest_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracked_core::cadence::Cadence;

    #[test]
    fn classifies_routine_markers() {
        let classification = classify_source("Run daily\nLift on Monday Wednesday Friday", None);
        assert_eq!(classification.kind, ProgramKind::Routine);
    }

    #[test]
    fn infers_duration_from_weeks() {
        assert_eq!(infer_duration_days("8 week 5k plan"), Some(56));
    }

    #[test]
    fn infers_duration_from_hyphenated_days() {
        assert_eq!(
            infer_duration_days("10-day beginner Rust learning plan"),
            Some(10)
        );
    }

    #[test]
    fn routine_generation_maps_common_cadences() {
        let task = task_from_line(
            "Gym Monday Wednesday Friday 45 min",
            0,
            ProgramKind::Routine,
        );
        assert_eq!(
            task.cadence,
            Cadence::WeeklyDays {
                days: vec![1, 3, 5]
            }
        );
        assert_eq!(task.estimated_minutes, 45);
    }

    #[test]
    fn curriculum_generation_keeps_sequence() {
        let task = task_from_line("Chapter 2: ownership", 3, ProgramKind::Curriculum);
        assert_eq!(task.cadence, Cadence::Once { day_offset: 3 });
    }

    #[test]
    fn generation_uses_bullets_when_present() {
        let tasks = extract_task_lines("- Run 30 min\n- Stretch\nNotes");
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn vague_fitness_productivity_prompt_expands_into_starter_plan() {
        let classification = classify_source("beginner fitness and productivity program", None);
        let generated = generate_program(
            "beginner fitness and productivity program",
            None,
            &classification,
            Intensity::Standard,
        );

        assert_eq!(generated.duration_days, STARTER_DURATION_DAYS);
        assert_eq!(generated.tasks.len(), usize::from(STARTER_DURATION_DAYS));
        assert_ne!(
            generated.tasks[0].title.to_lowercase(),
            "beginner fitness and productivity program"
        );
        assert!(generated
            .tasks
            .iter()
            .any(|task| task.category.as_deref() == Some("fitness")));
        assert!(generated
            .tasks
            .iter()
            .any(|task| task.category.as_deref() == Some("productivity")));
    }

    #[test]
    fn rust_learning_prompt_expands_to_requested_curriculum_length() {
        let prompt = "create a 10-day beginner Rust learning plan";
        let classification = classify_source(prompt, None);
        let generated = generate_program(prompt, None, &classification, Intensity::Standard);

        assert_eq!(classification.kind, ProgramKind::Curriculum);
        assert_eq!(generated.duration_days, 10);
        assert_eq!(generated.tasks.len(), 10);
        assert_eq!(
            generated.tasks[0].title,
            "Install Rust and run Hello World with Cargo"
        );
        assert!(generated
            .tasks
            .iter()
            .all(|task| task.category.as_deref() == Some("rust")));
        assert!(generated
            .tasks
            .iter()
            .all(|task| matches!(task.cadence, Cadence::Once { .. })));
    }

    #[test]
    fn duration_prompt_is_not_mistaken_for_numbered_task() {
        let tasks = extract_task_lines("10-day beginner Rust learning plan");
        assert_eq!(tasks.len(), 1);
        assert!(!is_numbered_list_item(&tasks[0]));
    }

    #[test]
    fn listed_source_still_uses_user_items() {
        let classification = classify_source("- Run 30 min\n- Stretch", None);
        let generated = generate_program(
            "- Run 30 min\n- Stretch",
            None,
            &classification,
            Intensity::Standard,
        );

        assert_eq!(generated.tasks.len(), 2);
        assert_eq!(generated.tasks[0].title, "Run 30 min");
    }

    #[test]
    fn compact_title_keeps_meaningful_leading_numbers() {
        assert_eq!(compact_title("5K training plan"), "5K training plan");
        assert_eq!(compact_title("1. Write brief"), "Write brief");
    }
}
