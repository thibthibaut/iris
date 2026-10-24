//! The app: axum + askama + htmx. Pages render full HTML; actions return fragments.

mod album;
mod design;
mod pages;
mod people;
mod pipeline;
mod photos;
mod timeline;

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use askama::Template;
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::stream::Stream;
use iris_core::model::Override;
use rust_embed::RustEmbed;
use tracing::info;

use crate::{
    config::Config,
    db::Database,
    jobs::{JobEvent, JobManager, JobSnapshot},
    stages::{self, ClipText},
    state::AlbumState,
};

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    config: Config,
    jobs: JobManager,
    clip: Mutex<Option<ClipText>>,
    snapshot: Mutex<Option<(String, Arc<AlbumState>)>>,
}

impl AppState {
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    pub fn jobs(&self) -> &JobManager {
        &self.0.jobs
    }

    pub fn db(&self) -> Result<Database> {
        Database::open(&self.0.config.database_path)
    }

    /// The album as the user sees it, cached until a decision or a stage changes it.
    pub fn snapshot(&self, db: &Database) -> Result<Arc<AlbumState>> {
        let mut key = db.overrides_revision()?;
        for stage in ["scan", "ingest", "quality", "embed", "faces", "cluster", "shots", "chapters"] {
            key.push_str(&format!("|{:?}", db.stage_run(stage)?.map(|run| run.finished_at_ms)));
        }
        if let Some((cached_key, snapshot)) = self.0.snapshot.lock().unwrap().as_ref()
            && *cached_key == key
            && self.0.jobs.running().is_none()
        {
            return Ok(snapshot.clone());
        }
        let snapshot = Arc::new(AlbumState::load(db)?);
        *self.0.snapshot.lock().unwrap() = Some((key, snapshot.clone()));
        Ok(snapshot)
    }

    /// Records a decision and tells every open page.
    pub fn decide(&self, db: &Database, item: Override, label: &str) -> Result<()> {
        db.add_override(&item, label)?;
        self.0.jobs.notify("overrides");
        Ok(())
    }

    /// CLIP text embedding of a search query (the model loads on first use).
    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let mut clip = self.0.clip.lock().unwrap();
        if clip.is_none() {
            *clip = Some(ClipText::load()?);
        }
        Ok(clip.as_ref().expect("just loaded").embed(&[query])?.remove(0))
    }
}

pub async fn serve(config: Config, host: String, port: u16) -> Result<()> {
    std::fs::create_dir_all(config.webp_dir())?;
    let jobs = JobManager::start(config.clone(), stages::all())?;
    let state = AppState(Arc::new(Inner {
        config,
        jobs,
        clip: Mutex::new(None),
        snapshot: Mutex::new(None),
    }));

    let app = Router::new()
        .route("/", get(pipeline::page))
        .route("/api/pipeline", get(pipeline::cards))
        .route("/api/stages/{name}/run", post(pipeline::run))
        .route("/api/stages/{name}/cancel", post(pipeline::cancel))
        .route("/api/stages/{name}/failures", get(pipeline::failures))
        .route("/api/run-stale", post(pipeline::run_stale))
        .route("/api/cancel-all", post(pipeline::cancel_all))
        .route("/inventory", get(pipeline::inventory))
        .route("/photos", get(photos::page))
        .route("/api/photos", get(photos::grid))
        .route("/photos/{id}", get(photos::detail))
        .route("/api/photos/{id}/{action}", post(photos::act))
        .route("/img/{id}/{size}", get(photos::image))
        .route("/face/{id}", get(photos::face))
        .route("/people", get(people::page))
        .route("/api/people", get(people::board_fragment))
        .route("/people/{anchor}", get(people::person))
        .route("/api/people/{anchor}/{action}", post(people::act))
        .route("/api/faces/{action}", post(people::faces))
        .route("/timeline", get(timeline::page))
        .route("/album", get(album::page))
        .route("/preview/{uid}", get(album::preview_image))
        .route("/spread/{uid}", get(album::spread_page))
        .route("/api/spread/{uid}/edit", post(album::edit))
        .route("/api/spread/{uid}/tray/{photo}", get(album::tray))
        .route("/design", get(design::page))
        .route("/api/text", post(design::save_text))
        .route("/export", get(design::export_page))
        .route("/exports/{id}/download", get(design::download))
        .route("/api/exports/{id}/delete", post(design::delete_export))
        .route("/settings", get(pages::settings))
        .route("/api/settings", post(pages::save_setting))
        .route("/api/undo", post(pages::undo))
        .route("/api/redo", post(pages::redo))
        .route("/api/history", get(pages::history))
        .route("/api/events", get(events))
        .route("/api/jobbar", get(job_bar))
        .route("/assets/{*path}", get(asset))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    info!("iris album on http://{host}:{port}/");
    axum::serve(listener, app).await?;
    Ok(())
}

// ---- Shared responses ---------------------------------------------------------------

pub struct AppError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = format!("{:#}", self.0), "request failed");
        let message = escape(&format!("{:#}", self.0));
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!("<div class=\"error-box\"><strong>Something went wrong.</strong><pre>{message}</pre></div>")),
        )
            .into_response()
    }
}

pub type WebResult<T> = Result<T, AppError>;

pub fn render(template: &impl Template) -> WebResult<Html<String>> {
    Ok(Html(template.render()?))
}

/// Runs blocking DB/image work off the async runtime.
pub async fn blocking<T, F>(f: F) -> WebResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    Ok(tokio::task::spawn_blocking(f).await??)
}

pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Tells htmx to show a toast with this message.
pub fn toast(html: Html<String>, message: &str) -> Response {
    let trigger = serde_json::json!({ "toast": message }).to_string();
    ([("HX-Trigger", trigger)], html).into_response()
}

async fn asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    match Assets::get(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            // Our own CSS/JS change with every build: always revalidate them.
            let cache = if path.starts_with("vendor/") || path.contains("fonts/") { "public, max-age=86400" } else { "no-cache" };
            (
                [(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, cache.to_string())],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---- Live job updates -----------------------------------------------------------------

#[derive(Template)]
#[template(path = "job_bar.html")]
pub struct JobBar {
    pub running: Option<JobSnapshot>,
    pub queued: Vec<String>,
}

impl JobBar {
    pub fn from(jobs: &JobManager) -> Self {
        Self { running: jobs.running(), queued: jobs.queued() }
    }

    pub fn percent(&self) -> u32 {
        self.running
            .as_ref()
            .filter(|job| job.total > 0)
            .map_or(0, |job| (job.done * 100 / job.total) as u32)
    }

    pub fn eta(&self) -> String {
        self.running.as_ref().and_then(|job| job.eta_seconds).map(format_duration).unwrap_or_default()
    }
}

pub fn format_duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3600 => format!("{} min {:02} s", seconds / 60, seconds % 60),
        _ => format!("{} h {:02} min", seconds / 3600, seconds / 60 % 60),
    }
}

async fn job_bar(State(state): State<AppState>) -> WebResult<Html<String>> {
    render(&JobBar::from(state.jobs()))
}

/// SSE: `job` carries the job bar HTML (throttled by the runner), `changed` tells pages
/// to refresh after a job or a decision.
async fn events(State(state): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.jobs().subscribe();
    let first = JobBar::from(state.jobs()).render().unwrap_or_default();
    let stream = futures_util::stream::unfold(
        (receiver, Some(first), state),
        |(mut receiver, first, state)| async move {
            if let Some(first) = first {
                return Some((Ok(Event::default().event("job").data(first)), (receiver, None, state)));
            }
            loop {
                match receiver.recv().await {
                    Ok(JobEvent::Progress(_)) => {
                        let html = JobBar::from(state.jobs()).render().unwrap_or_default();
                        return Some((Ok(Event::default().event("job").data(html)), (receiver, None, state)));
                    }
                    Ok(JobEvent::Changed { stage }) => {
                        // Pages listening on `changed` refetch what they show; the job bar
                        // follows as the next event.
                        let event = Event::default().event("changed").data(stage);
                        let bar = JobBar::from(state.jobs()).render().unwrap_or_default();
                        return Some((Ok(event), (receiver, Some(bar), state)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
