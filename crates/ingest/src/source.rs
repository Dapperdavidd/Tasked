//! Stage 1: normalise.
//!
//! Route a source to plain text, hash it, and cut it into chunks a model can
//! actually read to the end of.
//!
//! Text extraction for binary formats — PDF via pdfium, DOCX, OCR via a
//! tesseract subprocess, audio via a hosted ASR API — is I/O and lives at the
//! call site behind [`Extracted`]. What lives here is everything deterministic:
//! routing, normalisation, hashing, chunking, and the judgement about whether a
//! PDF came back so empty that it must have been a scan.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Below this many characters per page, a PDF is almost certainly a scan and
/// needs OCR rather than text extraction.
pub const SCANNED_PDF_CHARS_PER_PAGE: usize = 100;

/// Chunk size for the map stage. A single call over an eighty-page syllabus
/// drops the second half, so long sources are cut and reduced.
pub const CHUNK_TARGET_CHARS: usize = 12_000;

/// Below this, there is nothing to send a model.
///
/// Deliberately low. "Run 5k in 8 weeks" is a legitimate source — the PRD's own
/// first-run example is a typed sentence — so this only catches the genuinely
/// empty. Deciding whether something *is* a plan is stage 2's job, and it has a
/// model to do it with.
pub const MIN_USABLE_CHARS: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Text,
    Markdown,
    Pdf,
    Docx,
    Image,
    Audio,
    Url,
}

impl SourceKind {
    /// Route a MIME type to the extractor that handles it.
    pub fn from_mime(mime: &str) -> Option<Self> {
        let mime = mime.split(';').next().unwrap_or(mime).trim();
        match mime {
            "text/plain" => Some(Self::Text),
            "text/markdown" | "text/x-markdown" => Some(Self::Markdown),
            "application/pdf" => Some(Self::Pdf),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                Some(Self::Docx)
            }
            "text/html" => Some(Self::Url),
            other if other.starts_with("image/") => Some(Self::Image),
            other if other.starts_with("audio/") => Some(Self::Audio),
            _ => None,
        }
    }

    /// Whether this kind needs an out-of-process extractor.
    pub fn needs_external_extraction(self) -> bool {
        matches!(self, Self::Pdf | Self::Docx | Self::Image | Self::Audio)
    }
}

/// What an extractor produced. `pages` is only meaningful for paged formats and
/// is what makes the scanned-PDF check possible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    pub text: String,
    pub pages: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalised {
    pub text: String,
    /// sha256 of the normalised text. Stage 1 returns a cached draft when this
    /// already exists at the same intensity, which is the main lever on model
    /// spend.
    pub content_hash: [u8; 32],
}

impl Normalised {
    pub fn hash_hex(&self) -> String {
        self.content_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NormaliseError {
    #[error("unsupported source type")]
    UnsupportedMime,
    #[error("source produced no usable text")]
    Empty,
    #[error("source looks like a scan and needs OCR")]
    NeedsOcr,
}

/// Collapse a raw extraction into the canonical text the hash is taken over.
///
/// Normalising *before* hashing is what makes the cache actually hit: the same
/// syllabus pasted twice with different trailing whitespace is one document, and
/// paying for a second generation because of a stray `\r` is the kind of cost
/// that only shows up on the invoice.
pub fn normalise(extracted: &Extracted, kind: SourceKind) -> Result<Normalised, NormaliseError> {
    let text = canonicalise(&extracted.text);

    if text.chars().count() < MIN_USABLE_CHARS {
        // A PDF that extracted almost nothing is a scan, not an empty document.
        if kind == SourceKind::Pdf && looks_scanned(&extracted.text, extracted.pages) {
            return Err(NormaliseError::NeedsOcr);
        }
        return Err(NormaliseError::Empty);
    }

    if kind == SourceKind::Pdf && looks_scanned(&extracted.text, extracted.pages) {
        return Err(NormaliseError::NeedsOcr);
    }

    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let content_hash = hasher.finalize().into();

    Ok(Normalised { text, content_hash })
}

/// Whether an extraction is too sparse for the page count to be real text.
pub fn looks_scanned(text: &str, pages: Option<usize>) -> bool {
    let Some(pages) = pages.filter(|count| *count > 0) else {
        return false;
    };
    text.chars().count() / pages < SCANNED_PDF_CHARS_PER_PAGE
}

/// Normalise line endings, strip control characters, collapse runs of blank
/// lines, and trim trailing whitespace from every line.
fn canonicalise(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank_run = 0usize;

    for line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let cleaned: String = line
            .chars()
            .filter(|character| !character.is_control() || *character == '\t')
            .collect();
        let cleaned = cleaned.trim_end();

        if cleaned.is_empty() {
            blank_run += 1;
            // One blank line separates paragraphs; more carries no information.
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }

        out.push_str(cleaned);
        out.push('\n');
    }

    out.trim().to_owned()
}

/// Cut normalised text into chunks for the map stage.
///
/// Splits on blank lines so a chunk boundary never lands mid-paragraph, and
/// never mid-word. A section longer than the target on its own is emitted whole
/// rather than cut, because truncating it would silently drop content.
pub fn chunk(text: &str, target_chars: usize) -> Vec<String> {
    let target = target_chars.max(1);
    if text.chars().count() <= target {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![text.to_owned()]
        };
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for paragraph in text.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }

        let projected = current.chars().count() + paragraph.chars().count() + 2;
        if !current.is_empty() && projected > target {
            chunks.push(std::mem::take(&mut current));
        }

        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(paragraph);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extracted(text: &str) -> Extracted {
        Extracted {
            text: text.to_owned(),
            pages: None,
        }
    }

    #[test]
    fn routes_mime_types_to_extractors() {
        assert_eq!(
            SourceKind::from_mime("application/pdf"),
            Some(SourceKind::Pdf)
        );
        assert_eq!(
            SourceKind::from_mime("text/plain; charset=utf-8"),
            Some(SourceKind::Text)
        );
        assert_eq!(SourceKind::from_mime("image/png"), Some(SourceKind::Image));
        assert_eq!(SourceKind::from_mime("audio/mpeg"), Some(SourceKind::Audio));
        assert_eq!(SourceKind::from_mime("application/zip"), None);
    }

    #[test]
    fn normalisation_makes_the_content_hash_stable() {
        // The same document, pasted twice, with different line endings and
        // trailing whitespace. One document, one hash, one generation charge.
        let a = normalise(
            &extracted("Day 1: run  \r\nDay 2: rest\r\n\r\n\r\n"),
            SourceKind::Text,
        )
        .expect("normalises");
        let b =
            normalise(&extracted("Day 1: run\nDay 2: rest"), SourceKind::Text).expect("normalises");

        assert_eq!(a.content_hash, b.content_hash);
        assert_eq!(a.text, "Day 1: run\nDay 2: rest");
    }

    #[test]
    fn different_content_hashes_differently() {
        let a = normalise(&extracted("Day 1: run a mile"), SourceKind::Text).expect("normalises");
        let b =
            normalise(&extracted("Day 1: run two miles"), SourceKind::Text).expect("normalises");
        assert_ne!(a.content_hash, b.content_hash);
        assert_eq!(a.hash_hex().len(), 64);
    }

    #[test]
    fn control_characters_are_stripped_but_tabs_survive() {
        let result = normalise(&extracted("Day 1:\tspeed work\u{0}\u{7}"), SourceKind::Text)
            .expect("normalises");
        assert_eq!(result.text, "Day 1:\tspeed work");
    }

    /// The PRD's own first-run example is a typed sentence, so a terse but real
    /// plan must survive stage 1 and reach the classifier.
    #[test]
    fn a_short_typed_plan_is_not_rejected_as_empty() {
        for plan in [
            "Run 5k in 8 weeks",
            "Learn Rust in 100 days",
            "Physio: 3x a week",
        ] {
            assert!(
                normalise(&extracted(plan), SourceKind::Text).is_ok(),
                "rejected a legitimate plan: {plan}"
            );
        }
    }

    #[test]
    fn an_empty_source_is_refused_rather_than_sent_to_a_model() {
        assert_eq!(
            normalise(&extracted("   \n\n  "), SourceKind::Text),
            Err(NormaliseError::Empty)
        );
    }

    #[test]
    fn a_sparse_pdf_is_routed_to_ocr_not_reported_as_empty() {
        let scan = Extracted {
            text: "Chapter 1 ".repeat(3),
            pages: Some(40),
        };
        assert_eq!(
            normalise(&scan, SourceKind::Pdf),
            Err(NormaliseError::NeedsOcr)
        );

        // The same sparse text without a page count is just short text.
        let unpaged = Extracted {
            text: "Chapter 1 ".repeat(3),
            pages: None,
        };
        assert!(normalise(&unpaged, SourceKind::Pdf).is_ok());
    }

    #[test]
    fn a_dense_pdf_extracts_normally() {
        let real = Extracted {
            text: "Week 1 session detail. ".repeat(500),
            pages: Some(10),
        };
        assert!(normalise(&real, SourceKind::Pdf).is_ok());
    }

    // --- chunking -----------------------------------------------------------

    #[test]
    fn short_text_is_a_single_chunk() {
        assert_eq!(chunk("Day 1: run", 1000), vec!["Day 1: run".to_owned()]);
        assert!(chunk("", 1000).is_empty());
    }

    #[test]
    fn long_text_splits_on_paragraph_boundaries() {
        let text = (1..=10)
            .map(|n| format!("Week {n}\n{}", "session detail. ".repeat(20)))
            .collect::<Vec<_>>()
            .join("\n\n");

        let chunks = chunk(&text, 1000);
        assert!(chunks.len() > 1);
        for piece in &chunks {
            assert!(piece.starts_with("Week"), "cut mid-paragraph: {piece:.40}");
        }
    }

    /// The rule the pipeline exists to honour: never silently drop content.
    #[test]
    fn chunking_preserves_every_paragraph() {
        let text = (1..=40)
            .map(|n| format!("Week {n} detail"))
            .collect::<Vec<_>>()
            .join("\n\n");

        let chunks = chunk(&text, 100);
        let rejoined = chunks.join("\n\n");

        for n in 1..=40 {
            assert!(
                rejoined.contains(&format!("Week {n} detail")),
                "week {n} was dropped"
            );
        }
    }

    #[test]
    fn a_paragraph_longer_than_the_target_is_kept_whole() {
        let huge = "x".repeat(5_000);
        let chunks = chunk(&huge, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chars().count(), 5_000);
    }
}
