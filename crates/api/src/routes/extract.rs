use std::{fs, path::PathBuf, process::Command};

use actix_web::{post, web, HttpResponse};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

#[derive(Deserialize)]
pub struct ExtractSourceBody {
    filename: String,
    mime_type: String,
    data_base64: String,
}

#[derive(Serialize)]
struct ExtractSourceResponse {
    text: String,
    mime_type: String,
}

#[post("/v1/extract")]
pub async fn extract_source(
    _state: web::Data<ApiState>,
    _user_id: UserId,
    body: web::Json<ExtractSourceBody>,
) -> Result<HttpResponse, ApiError> {
    let bytes = STANDARD
        .decode(body.data_base64.trim())
        .map_err(|_| ApiError::BadRequest("file data is not valid base64".to_owned()))?;
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(ApiError::BadRequest("file is too large".to_owned()));
    }

    let path = temp_upload_path(&body.filename);
    fs::write(&path, &bytes).map_err(|error| ApiError::Worker(error.to_string()))?;
    let extracted = extract_text(&path, &body.mime_type, &body.filename, &bytes);
    let _ = fs::remove_file(&path);
    let text = extracted?;

    Ok(HttpResponse::Ok().json(ExtractSourceResponse {
        text,
        mime_type: "text/plain".to_owned(),
    }))
}

fn temp_upload_path(filename: &str) -> PathBuf {
    let extension = filename
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| {
            extension
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        })
        .unwrap_or("upload");
    std::env::temp_dir().join(format!("tracked-{}.{}", Uuid::now_v7(), extension))
}

fn extract_text(
    path: &PathBuf,
    mime_type: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, ApiError> {
    let lower_name = filename.to_lowercase();
    let mime_type = mime_type.split(';').next().unwrap_or(mime_type).trim();

    if mime_type.starts_with("text/")
        || [".txt", ".md", ".csv", ".json"]
            .iter()
            .any(|suffix| lower_name.ends_with(suffix))
    {
        return String::from_utf8(bytes.to_vec())
            .map_err(|_| ApiError::BadRequest("text file is not valid UTF-8".to_owned()));
    }

    if mime_type == "application/pdf" || lower_name.ends_with(".pdf") {
        return run_extractor("extract_pdf_text.swift", path);
    }

    if mime_type.starts_with("image/") {
        return run_extractor("extract_image_text.swift", path);
    }

    if mime_type == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        || lower_name.ends_with(".docx")
        || lower_name.ends_with(".rtf")
        || lower_name.ends_with(".html")
    {
        return run_textutil(path);
    }

    Err(ApiError::BadRequest(
        "unsupported file type for extraction".to_owned(),
    ))
}

fn run_textutil(path: &PathBuf) -> Result<String, ApiError> {
    let output = Command::new("textutil")
        .args(["-convert", "txt", "-stdout"])
        .arg(path)
        .output()
        .map_err(|error| ApiError::Worker(error.to_string()))?;
    command_text(output, "textutil could not extract this document")
}

fn run_extractor(script: &str, path: &PathBuf) -> Result<String, ApiError> {
    let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(script);
    let output = Command::new("swift")
        .arg(script_path)
        .arg(path)
        .output()
        .map_err(|error| ApiError::Worker(error.to_string()))?;
    command_text(output, "could not extract text from this file")
}

fn command_text(output: std::process::Output, fallback: &str) -> Result<String, ApiError> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ApiError::BadRequest(if stderr.is_empty() {
            fallback.to_owned()
        } else {
            stderr
        }));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        Err(ApiError::BadRequest(
            "file did not contain readable text".to_owned(),
        ))
    } else {
        Ok(text)
    }
}
