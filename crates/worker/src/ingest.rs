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
            .unwrap_or_else(|| starter_duration_days(intensity))
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

fn starter_duration_days(intensity: Intensity) -> u16 {
    match intensity {
        Intensity::Light => 21,
        Intensity::Standard => STARTER_DURATION_DAYS,
        Intensity::Heavy => 10,
    }
}

fn starter_tasks_per_day(intensity: Intensity) -> usize {
    match intensity {
        Intensity::Light => 3,
        Intensity::Standard => 5,
        Intensity::Heavy => 7,
    }
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

    let topics: Vec<(&str, &str)> = if include_rust {
        vec![
            ("Rust setup, rustup, and Cargo", "rust"),
            ("variables, mutability, and scalar types", "rust"),
            ("functions, expressions, and control flow", "rust"),
            ("ownership, moves, and stack versus heap", "rust"),
            ("borrowing, references, and slices", "rust"),
            ("structs, methods, and associated functions", "rust"),
            ("enums, match, Option, and pattern matching", "rust"),
            ("vectors, strings, hash maps, and iterators", "rust"),
            ("Result, recoverable errors, and compiler messages", "rust"),
            ("a small command-line Rust project", "rust"),
            ("modules, packages, crates, and visibility", "rust"),
            (
                "file input, parsing, and small data transformations",
                "rust",
            ),
            ("unit tests, integration tests, and cargo test", "rust"),
            (
                "refactoring, documentation, and final project polish",
                "rust",
            ),
        ]
    } else {
        match (include_fitness, include_productivity) {
            (true, true) => vec![
                ("movement baseline and priority planning", "fitness"),
                ("mobility, walking, and one focused work block", "fitness"),
                ("beginner strength and distraction control", "fitness"),
                ("recovery, hydration, and workspace reset", "recovery"),
                ("cardio intervals and small deliverables", "fitness"),
                ("core strength and weekly review", "fitness"),
                ("active recovery and meal planning", "recovery"),
                ("strength repeat and protected deep work", "fitness"),
                ("walking intervals and attention cleanup", "fitness"),
                ("mobility plus task batching", "recovery"),
                ("longer beginner workout and visible output", "fitness"),
                ("stretching, reflection, and scope cleanup", "recovery"),
                ("baseline retest and progress comparison", "fitness"),
                ("next weekly routine design", "productivity"),
            ],
            (true, false) => vec![
                ("movement baseline", "fitness"),
                ("brisk walking and stretching", "fitness"),
                ("beginner strength circuit", "fitness"),
                ("mobility and recovery breathing", "recovery"),
                ("easy cardio intervals", "fitness"),
                ("core stability basics", "fitness"),
                ("active recovery walk", "recovery"),
                ("strength circuit repeat", "fitness"),
                ("gentle cardio progression", "fitness"),
                ("hips, back, and shoulders mobility", "recovery"),
                ("full beginner workout", "fitness"),
                ("light recovery and hydration prep", "recovery"),
                ("baseline retest", "fitness"),
                ("next fitness target", "fitness"),
            ],
            (false, true) => vec![
                ("current routine audit", "productivity"),
                ("priority choice and workspace cleanup", "productivity"),
                ("focused work block practice", "productivity"),
                ("simple task capture system", "productivity"),
                ("small-task batching", "productivity"),
                ("no-distraction work block", "productivity"),
                ("weekly review and blocker removal", "productivity"),
                ("tomorrow planning ritual", "productivity"),
                ("deeper focus block", "productivity"),
                ("calendar and commitment cleanup", "productivity"),
                ("visible deliverable completion", "productivity"),
                ("template or automation for repeated work", "productivity"),
                ("real-condition routine retest", "productivity"),
                ("next weekly operating system", "productivity"),
            ],
            (false, false) => vec![
                ("goal definition and success measure", "planning"),
                ("small milestone breakdown", "planning"),
                ("first starter action", "execution"),
                ("friction review and plan adjustment", "review"),
                ("second practical step", "execution"),
                ("core skill practice block", "execution"),
                ("progress review and scope simplification", "review"),
                ("highest-value action repeat", "execution"),
                ("small challenge increase", "execution"),
                ("working-process documentation", "review"),
                ("useful mini deliverable", "execution"),
                ("loose-end cleanup", "execution"),
                ("success measure retest", "review"),
                ("next cycle planning", "planning"),
            ],
        }
    };

    build_day_bundles(topics, intensity, duration_days, include_rust)
}

fn build_day_bundles(
    topics: Vec<(&str, &str)>,
    intensity: Intensity,
    duration_days: u16,
    is_rust: bool,
) -> Vec<GeneratedTask> {
    let actions = starter_actions(intensity, is_rust);
    debug_assert_eq!(actions.len(), starter_tasks_per_day(intensity));
    let mut tasks = Vec::with_capacity(usize::from(duration_days) * actions.len());

    for day in 0..duration_days {
        let (topic, category) = topics
            .get(usize::from(day) % topics.len())
            .copied()
            .unwrap_or(("core skill practice", "execution"));
        for (position, action) in actions.iter().enumerate() {
            tasks.push(GeneratedTask {
                title: format_starter_task(action, topic),
                description: None,
                category: Some(category.to_owned()),
                difficulty: starter_difficulty(usize::from(day), position),
                estimated_minutes: starter_minutes(intensity, position),
                cadence: tracked_core::cadence::Cadence::Once {
                    day_offset: u32::from(day),
                },
            });
        }
    }

    tasks
}

fn starter_actions(intensity: Intensity, is_rust: bool) -> Vec<&'static str> {
    if is_rust {
        return match intensity {
            Intensity::Light => vec![
                "Read the core idea for",
                "Code one example of",
                "Write notes about",
            ],
            Intensity::Standard => vec![
                "Read the core idea for",
                "Code two examples of",
                "Break and fix one compiler error around",
                "Do a mini exercise using",
                "Write a short recap of",
            ],
            Intensity::Heavy => vec![
                "Read the official chapter section for",
                "Code two examples of",
                "Trace one compiler error involving",
                "Do a focused drill using",
                "Refactor yesterday's code with",
                "Build one tiny feature around",
                "Write tests or notes for",
            ],
        };
    }

    match intensity {
        Intensity::Light => vec![
            "Learn the core idea for",
            "Practice one small rep of",
            "Log progress on",
        ],
        Intensity::Standard => vec![
            "Learn the core idea for",
            "Practice two focused reps of",
            "Apply the idea to",
            "Review friction around",
            "Plan tomorrow's next step for",
        ],
        Intensity::Heavy => vec![
            "Study the core idea for",
            "Practice three reps of",
            "Apply the idea to",
            "Increase difficulty on",
            "Review mistakes around",
            "Produce a small deliverable from",
            "Plan the next upgrade for",
        ],
    }
}

fn format_starter_task(action: &str, topic: &str) -> String {
    let title = format!("{action} {topic}");
    if title.len() > 80 {
        title.chars().take(80).collect()
    } else {
        title
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn starter_difficulty(day_index: usize, task_position: usize) -> u8 {
    match (day_index, task_position) {
        (0..=2, 0..=2) => 1,
        (0..=6, _) => 2,
        (7..=11, 0..=4) => 3,
        (7..=11, _) => 4,
        (_, 0..=4) => 3,
        _ => 4,
    }
}

fn starter_minutes(intensity: Intensity, task_position: usize) -> u16 {
    let minutes = match intensity {
        Intensity::Light => [7, 7, 6].get(task_position).copied().unwrap_or(5),
        Intensity::Standard => [10, 10, 8, 9, 8].get(task_position).copied().unwrap_or(8),
        Intensity::Heavy => [14, 14, 12, 12, 12, 13, 13]
            .get(task_position)
            .copied()
            .unwrap_or(10),
    };
    minutes.min(intensity.daily_cap_minutes())
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
        assert_eq!(
            generated.tasks.len(),
            usize::from(STARTER_DURATION_DAYS) * starter_tasks_per_day(Intensity::Standard)
        );
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
        assert_eq!(
            generated.tasks.len(),
            10 * starter_tasks_per_day(Intensity::Standard)
        );
        assert_eq!(
            generated.tasks[0].title,
            "Read the core idea for Rust setup, rustup, and Cargo"
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
    fn intensity_changes_starter_daily_load() {
        let prompt = "beginner Rust learning plan";
        let classification = classify_source(prompt, None);
        let light = generate_program(prompt, None, &classification, Intensity::Light);
        let heavy = generate_program(prompt, None, &classification, Intensity::Heavy);

        assert_eq!(light.duration_days, 21);
        assert_eq!(heavy.duration_days, 10);
        assert_eq!(
            light.tasks.len(),
            21 * starter_tasks_per_day(Intensity::Light)
        );
        assert_eq!(
            heavy.tasks.len(),
            10 * starter_tasks_per_day(Intensity::Heavy)
        );
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
