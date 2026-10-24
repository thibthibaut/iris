//! VLM checks through OpenRouter, pinned to Mistral with zero data retention. Budgeted,
//! cached by content hash, and fail-closed: without a key or a private endpoint, nothing
//! is sent.

use std::{collections::HashSet, io::Cursor};

use anyhow::{Context, Result};
use base64::Engine;
use iris_core::{decisions::Decisions, model::Source};
use serde_json::json;

use crate::{
    album::current,
    db::{Database, PhotoRecord},
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
    stages::load_preview,
    state::AlbumState,
};

const PROMPT_VERSION: i64 = 1;
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
/// Rough price of one call with one 768 px image, for the estimate shown before running.
const COST_PER_CALL_EUR: f64 = 0.002;

#[derive(Clone, Copy, PartialEq)]
pub enum Task {
    ChapterTiebreak,
    PickReview,
}

impl Task {
    fn key(self) -> &'static str {
        match self {
            Self::ChapterTiebreak => "chapter_tiebreak",
            Self::PickReview => "pick_review",
        }
    }
}

pub struct Vlm(pub Task);

/// Photos this task would send.
fn targets(task: Task, db: &Database) -> Result<Vec<PhotoRecord>> {
    let state = AlbumState::load(db)?;
    Ok(match task {
        Task::ChapterTiebreak => state
            .photos
            .iter()
            .filter(|p| p.chapter_source.as_deref() == Some("visual") && p.chapter_margin.is_some_and(|m| m < 0.15))
            .filter(|p| !state.decisions.chapter_moves.contains_key(&p.id))
            .cloned()
            .collect(),
        Task::PickReview => {
            let Some(album) = current(db, &state)? else { return Ok(Vec::new()) };
            let heroes: HashSet<i64> = album
                .spreads
                .iter()
                .filter(|s| s.templates.iter().any(|t| t == "hero"))
                .flat_map(|s| s.photos.iter().map(|p| p.photo))
                .collect();
            album
                .spreads
                .iter()
                .flat_map(|s| s.photos.iter().map(|p| p.photo))
                .filter_map(|id| state.photo(id))
                .filter(|p| p.source == Some(Source::Guest) || heroes.contains(&p.id))
                .cloned()
                .collect()
        }
    })
}

fn uncached(task: Task, db: &Database, photos: Vec<PhotoRecord>) -> Result<Vec<PhotoRecord>> {
    let mut out = Vec::new();
    for photo in photos {
        let Some(hash) = &photo.blake3_hash else { continue };
        if db.vlm_cached(hash, task.key(), PROMPT_VERSION)?.is_none() {
            out.push(photo);
        }
    }
    Ok(out)
}

impl Stage for Vlm {
    fn name(&self) -> &'static str {
        match self.0 {
            Task::ChapterTiebreak => "vlm_chapter_tiebreak",
            Task::PickReview => "vlm_pick_review",
        }
    }

    fn title(&self) -> &'static str {
        match self.0 {
            Task::ChapterTiebreak => "Ask Mistral: unsure Chapters",
            Task::PickReview => "Ask Mistral: review picks",
        }
    }

    fn description(&self) -> &'static str {
        match self.0 {
            Task::ChapterTiebreak => "Optional. Sends the unsure guest photos (768 px, no metadata) to a Mistral model with zero data retention to choose their Chapter.",
            Task::PickReview => "Optional. Sends the selected guest photos and Heroes (768 px, no metadata) to check eyes, blur and unflattering moments.",
        }
    }

    fn version(&self) -> i64 {
        PROMPT_VERSION
    }

    fn deps(&self) -> &'static [&'static str] {
        match self.0 {
            Task::ChapterTiebreak => &["chapters"],
            Task::PickReview => &["select"],
        }
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn optional(&self) -> bool {
        true
    }

    fn confirm(&self, db: &Database, decisions: &Decisions) -> Option<String> {
        let pending = uncached(self.0, db, targets(self.0, db).ok()?).ok()?.len();
        let calls = pending.min(decisions.settings.vlm.max_calls as usize);
        Some(format!(
            "This sends {calls} photos (768 px, no metadata) to {} through OpenRouter, Mistral only, zero data retention. Estimated cost about €{:.2}. Continue?",
            decisions.settings.vlm.model,
            calls as f64 * COST_PER_CALL_EUR
        ))
    }

    fn decisions_key(&self, decisions: &Decisions) -> String {
        decisions.settings.vlm.model.clone()
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        let key = std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .context("no OPENROUTER_API_KEY in the environment: nothing was sent")?;
        let settings = ctx.settings();
        let photos = uncached(self.0, &ctx.db, targets(self.0, &ctx.db)?)?;
        let budget = settings.vlm.max_calls as usize;
        let photos: Vec<_> = photos.into_iter().take(budget).collect();
        sink.start(photos.len());
        let client = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(90)).build()?;
        let chapters: Vec<String> = settings
            .chapters
            .iter()
            .map(|c| format!("- {}: {} ({})", c.key, c.title_fr, c.prompts.join("; ")))
            .collect();
        for photo in photos {
            cancel.check()?;
            let image = load_preview(ctx, &photo)?.thumbnail(768, 768).to_rgb8();
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), 80).encode_image(&image)?;
            let data_url = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&jpeg));
            let prompt = match self.0 {
                Task::ChapterTiebreak => format!(
                    "This photo was taken at a wedding. Which moment of the day is it from? Moments, in order:\n{}\nAnswer only with JSON: {{\"chapter\": \"<key>\", \"confidence\": <0-1>}}",
                    chapters.join("\n")
                ),
                Task::PickReview => "Review this photo for a printed wedding album. Answer only with JSON: {\"eyes_closed\": bool, \"mid_blink_or_mid_speech\": bool, \"unflattering\": bool, \"heads_cut\": bool, \"motion_blur\": bool, \"keep_score\": 0-1, \"note\": \"short reason\"}".to_string(),
            };
            let body = json!({
                "model": settings.vlm.model,
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": data_url}}
                ]}],
                "response_format": {"type": "json_object"},
                "provider": {"order": ["mistral"], "allow_fallbacks": false, "data_collection": "deny", "zdr": true}
            });
            let response = client.post(ENDPOINT).bearer_auth(&key).json(&body).send()?;
            let status = response.status();
            let value: serde_json::Value = response.json().unwrap_or_default();
            // No private endpoint for this model: stop rather than fall back.
            anyhow::ensure!(status.is_success(), "OpenRouter refused ({status}): {}. Nothing else was sent.", value["error"]["message"].as_str().unwrap_or("no details"));
            let text = value["choices"][0]["message"]["content"].as_str().unwrap_or("{}");
            let parsed: serde_json::Value = serde_json::from_str(text.trim().trim_start_matches("```json").trim_end_matches("```").trim()).unwrap_or(json!({"raw": text}));
            ctx.db.put_vlm(photo.blake3_hash.as_deref().unwrap_or_default(), self.0.key(), PROMPT_VERSION, &parsed)?;
            sink.inc(1);
        }
        Ok(())
    }
}

/// The cached VLM review of a Photo, if any.
pub fn review(db: &Database, photo: &PhotoRecord) -> Option<serde_json::Value> {
    db.vlm_cached(photo.blake3_hash.as_deref()?, Task::PickReview.key(), PROMPT_VERSION).ok().flatten()
}

/// The cached VLM Chapter of a Photo, if any.
pub fn chapter(db: &Database, photo: &PhotoRecord) -> Option<String> {
    let value = db.vlm_cached(photo.blake3_hash.as_deref()?, Task::ChapterTiebreak.key(), PROMPT_VERSION).ok().flatten()?;
    value["chapter"].as_str().map(str::to_string)
}
