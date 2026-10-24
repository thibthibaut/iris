//! Pipeline page: every stage, its progress, and the inventory report.

use askama::Template;
use axum::{
    extract::{Path, State},
    response::{Html, Response},
};

use super::{AppState, WebResult, blocking, render, toast};
use crate::{
    db::PhotoRecord,
    jobs::{StageStatus, stage_status},
};

#[derive(Template)]
#[template(path = "pipeline.html")]
struct PipelinePage {
    nav: &'static str,
    cards: StageCards,
}

#[derive(Template)]
#[template(path = "stage_cards.html")]
struct StageCards {
    stages: Vec<StageCard>,
    any_stale: bool,
    busy: bool,
    disk: Vec<(String, String)>,
    mounts: Vec<(String, bool)>,
    photo_count: i64,
}

struct StageCard {
    status: StageStatus,
    confirm: Option<String>,
    state: &'static str,
    last_run: String,
    last_error: Option<String>,
    last_message: Option<String>,
}

impl StageCard {
    fn total(&self) -> i64 {
        self.status.done + self.status.failed + self.status.pending
    }

    fn percent(&self) -> i64 {
        if self.total() == 0 {
            if self.status.stale { 0 } else { 100 }
        } else {
            (self.status.done + self.status.failed) * 100 / self.total()
        }
    }
}

fn load_cards(state: &AppState) -> anyhow::Result<StageCards> {
    let db = state.db()?;
    let snapshot = state.snapshot(&db)?;
    let jobs = state.jobs();
    let running = jobs.running();
    let queued = jobs.queued();
    let mut stages = Vec::new();
    for stage in jobs.stages().iter().filter(|s| !s.name().starts_with("export:")) {
        let status = stage_status(&db, jobs.stages(), stage.as_ref(), &snapshot.decisions)?;
        let last_job = db.last_job(stage.name())?;
        let state = if running.as_ref().is_some_and(|job| job.stage == status.name) {
            "running"
        } else if queued.iter().any(|queued| queued == status.name) {
            "queued"
        } else if last_job.as_ref().is_some_and(|job| job.status == "failed") {
            "failed"
        } else if status.stale {
            "stale"
        } else {
            "done"
        };
        stages.push(StageCard {
            confirm: stage.confirm(&db, &snapshot.decisions),
            last_run: status.last_run_ms.map(ago).unwrap_or_else(|| "never".into()),
            last_error: last_job.as_ref().filter(|job| job.status == "failed").and_then(|job| job.error.clone()),
            last_message: last_job.and_then(|job| job.message),
            state,
            status,
        });
    }
    let config = state.config();
    Ok(StageCards {
        any_stale: stages.iter().any(|card| card.status.stale),
        busy: running.is_some() || !queued.is_empty(),
        disk: vec![
            ("WebP proxies".into(), human_bytes(dir_size(&config.webp_dir()))),
            ("Previews".into(), human_bytes(dir_size(&config.preview_dir()))),
            ("Temp".into(), human_bytes(dir_size(&config.temp_dir()))),
            ("Exports".into(), human_bytes(dir_size(&config.exports_dir))),
            ("Free on disk".into(), free_disk(&config.cache_dir)),
        ],
        mounts: vec![
            ("Photographer".into(), mounted(&config.photographer_root)),
            ("Guests".into(), mounted(&config.guests_root)),
        ],
        photo_count: db.count_by("source")?.iter().map(|(_, count)| count).sum(),
        stages,
    })
}

pub async fn page(State(state): State<AppState>) -> WebResult<Html<String>> {
    let cards = blocking(move || load_cards(&state)).await?;
    render(&PipelinePage { nav: "index", cards })
}

pub async fn cards(State(state): State<AppState>) -> WebResult<Html<String>> {
    let cards = blocking(move || load_cards(&state)).await?;
    render(&cards)
}

pub async fn run(State(state): State<AppState>, Path(name): Path<String>) -> WebResult<Response> {
    let title = state.jobs().stage(&name).map(|stage| stage.title()).unwrap_or("stage");
    state.jobs().enqueue(&name)?;
    let cards = blocking(move || load_cards(&state)).await?;
    Ok(toast(render(&cards)?, &format!("{title} queued")))
}

pub async fn cancel(State(state): State<AppState>, Path(name): Path<String>) -> WebResult<Response> {
    state.jobs().cancel(Some(&name));
    let cards = blocking(move || load_cards(&state)).await?;
    Ok(toast(render(&cards)?, "Cancelling…"))
}

pub async fn run_stale(State(state): State<AppState>) -> WebResult<Response> {
    let jobs = state.jobs().clone();
    let count = blocking(move || jobs.enqueue_stale()).await?;
    let cards = blocking(move || load_cards(&state)).await?;
    Ok(toast(render(&cards)?, &format!("{count} stages queued")))
}

pub async fn cancel_all(State(state): State<AppState>) -> WebResult<Response> {
    state.jobs().cancel_all();
    let cards = blocking(move || load_cards(&state)).await?;
    Ok(toast(render(&cards)?, "Cancelled"))
}

#[derive(Template)]
#[template(path = "failures.html")]
struct Failures {
    title: String,
    failures: Vec<(PhotoRecord, String)>,
}

pub async fn failures(State(state): State<AppState>, Path(name): Path<String>) -> WebResult<Html<String>> {
    let title = state.jobs().stage(&name).map(|stage| stage.title().to_string()).unwrap_or_default();
    let failures = blocking(move || state.db()?.stage_failures(&name)).await?;
    render(&Failures { title, failures })
}

#[derive(Template)]
#[template(path = "inventory.html")]
struct Inventory {
    nav: &'static str,
    tables: Vec<(&'static str, Vec<(String, i64)>)>,
    duplicates: Vec<Vec<i64>>,
    videos_skipped: i64,
    with_time: i64,
    total: i64,
    sent_by_day: Vec<(String, i64)>,
    megapixels: Vec<(String, i64, i64)>,
}

pub async fn inventory(State(state): State<AppState>) -> WebResult<Html<String>> {
    let page = blocking(move || {
        let db = state.db()?;
        let photos = db.all_photos()?;
        let mut tables = Vec::new();
        for (title, column) in [
            ("Source", "source"),
            ("Photographer folder", "source_folder"),
            ("Format", "format"),
            ("Camera", "camera_model"),
            ("Capture time from", "taken_at_source"),
        ] {
            tables.push((title, db.count_by(column)?));
        }
        let summary: Option<serde_json::Value> = db.blob("scan_summary")?;
        let mut sent_by_day = std::collections::BTreeMap::<String, i64>::new();
        for photo in &photos {
            if let Some(sent) = photo.sent_at_utc {
                let day = chrono::DateTime::from_timestamp_millis(sent)
                    .map(|t| t.with_timezone(&chrono_tz::Europe::Paris).format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                *sent_by_day.entry(day).or_default() += 1;
            }
        }
        let buckets = [(0.0, 2.0, "< 2 MP"), (2.0, 4.0, "2–4 MP"), (4.0, 8.0, "4–8 MP"), (8.0, 16.0, "8–16 MP"), (16.0, 1e9, "≥ 16 MP")];
        let megapixels = buckets
            .iter()
            .map(|(lo, hi, label)| {
                let count = |source: &str| {
                    photos
                        .iter()
                        .filter(|p| p.source.map(|s| s.as_str()) == Some(source))
                        .filter(|p| p.megapixels().is_some_and(|mp| mp >= *lo && mp < *hi))
                        .count() as i64
                };
                ((*label).to_string(), count("photographer"), count("guest"))
            })
            .collect();
        Ok(Inventory {
            nav: "index",
            tables,
            duplicates: db.exact_duplicates()?,
            videos_skipped: summary.and_then(|s| s["videos_skipped"].as_i64()).unwrap_or(0),
            with_time: photos.iter().filter(|p| p.taken_at_utc.is_some()).count() as i64,
            total: photos.len() as i64,
            sent_by_day: sent_by_day.into_iter().collect(),
            megapixels,
        })
    })
    .await?;
    render(&page)
}

pub fn ago(ms: i64) -> String {
    let seconds = ((crate::db::now_ms() - ms) / 1000).max(0);
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86400),
    }
}

fn dir_size(path: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum()
}

pub fn human_bytes(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes / 1000),
        1_000_000..1_000_000_000 => format!("{:.0} MB", bytes as f64 / 1e6),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

fn free_disk(path: &std::path::Path) -> String {
    let output = std::process::Command::new("df").arg("-k").arg(path).output();
    output
        .ok()
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            let line = text.lines().nth(1)?.to_string();
            let available_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
            Some(human_bytes(available_kb * 1024))
        })
        .unwrap_or_else(|| "?".into())
}

fn mounted(path: &std::path::Path) -> bool {
    std::fs::read_dir(path).map(|mut entries| entries.next().is_some()).unwrap_or(false)
}
