use std::{
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{Context, Result};
use axum::{
    Form, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use libvips::VipsApp;
use open_clip_inference::TextEmbedder;
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::{
    config::Config,
    db::{Database, PersonDetail, PersonFace, PersonSummary, SearchResult},
    webp_cache,
};

const MOBILE_CLIP_MODEL_ID: &str = "RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX";
const SEARCH_LIMIT: usize = 20;
const FACE_CROP_PADDING: f64 = 1.8;

#[derive(Clone)]
struct WebState {
    db_path: PathBuf,
    webp_cache_dir: PathBuf,
    text_embedder: Arc<Mutex<TextEmbedder>>,
    vips: Arc<VipsApp>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PersonNameForm {
    display_name: String,
}

pub async fn serve(config: Config, host: String, port: u16) -> Result<()> {
    let db_path = config.database_path.clone();
    info!(
        %host,
        port,
        db_path = %db_path.display(),
        webp_cache_dir = %webp_cache::cache_dir(&db_path).display(),
        model = MOBILE_CLIP_MODEL_ID,
        "starting web server"
    );

    let model_start = Instant::now();
    info!("initializing query text embedder");
    let text_embedder = TextEmbedder::from_hf(MOBILE_CLIP_MODEL_ID)
        .build()
        .await
        .context("failed to initialize MobileCLIP text embedder")?;
    info!(
        elapsed_ms = elapsed_ms(model_start),
        "query text embedder initialized"
    );

    let vips_start = Instant::now();
    info!("initializing libvips");
    let vips = VipsApp::default("iris").context("failed to initialize libvips")?;
    info!(
        version = vips.version_string().unwrap_or("unknown"),
        elapsed_ms = elapsed_ms(vips_start),
        "libvips initialized"
    );

    let state = WebState {
        webp_cache_dir: webp_cache::cache_dir(&db_path),
        db_path,
        text_embedder: Arc::new(Mutex::new(text_embedder)),
        vips: Arc::new(vips),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/people", get(people))
        .route("/people/:id", get(person_view))
        .route("/people/:id/name", post(update_person_name))
        .route("/view/:id", get(photo_view))
        .route("/photos/:id", get(photo))
        .fallback(not_found)
        .with_state(state)
        .layer(middleware::from_fn(log_request));
    let address = format!("{host}:{port}");
    info!(address = %address, "binding web server");
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("failed to bind web server on {address}"))?;
    let bound_address = listener
        .local_addr()
        .context("failed to read bound web server address")?;

    info!(
        requested_url = %format!("http://{address}"),
        bound_address = %bound_address,
        "web server listening"
    );
    axum::serve(listener, app)
        .await
        .context("web server failed")
}

async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let start = Instant::now();
    info!(%method, path = %uri.path(), query = uri.query().unwrap_or(""), "http request started");

    let response = next.run(request).await;
    let status = response.status();
    let status_code = status.as_u16();
    if status == StatusCode::NOT_FOUND {
        warn!(
            %method,
            path = %uri.path(),
            query = uri.query().unwrap_or(""),
            status = status_code,
            elapsed_ms = elapsed_ms(start),
            "http request completed with 404"
        );
    } else {
        info!(
            %method,
            path = %uri.path(),
            query = uri.query().unwrap_or(""),
            status = status_code,
            elapsed_ms = elapsed_ms(start),
            "http request completed"
        );
    }

    response
}

async fn index(State(state): State<WebState>, Query(params): Query<SearchParams>) -> Response {
    match render_index(&state, params.q.as_deref()) {
        Ok(html) => Html(html).into_response(),
        Err(error) => {
            error!(%error, "failed to render index page");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_error(&error.to_string())),
            )
                .into_response()
        }
    }
}

async fn photo_view(State(state): State<WebState>, Path(photo_id): Path<i64>) -> Response {
    match render_photo_view(&state, photo_id) {
        Ok(Some(html)) => Html(html).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Html(render_not_found("/view"))).into_response(),
        Err(error) => {
            error!(photo_id, %error, "failed to render photo detail page");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_error(&error.to_string())),
            )
                .into_response()
        }
    }
}

async fn people(State(state): State<WebState>) -> Response {
    match render_people(&state) {
        Ok(html) => Html(html).into_response(),
        Err(error) => {
            error!(%error, "failed to render people page");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_error(&error.to_string())),
            )
                .into_response()
        }
    }
}

async fn person_view(State(state): State<WebState>, Path(person_id): Path<i64>) -> Response {
    match render_person_view(&state, person_id) {
        Ok(Some(html)) => Html(html).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Html(render_not_found("/people"))).into_response(),
        Err(error) => {
            error!(person_id, %error, "failed to render person page");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_error(&error.to_string())),
            )
                .into_response()
        }
    }
}

async fn update_person_name(
    State(state): State<WebState>,
    Path(person_id): Path<i64>,
    Form(form): Form<PersonNameForm>,
) -> Response {
    match save_person_name(&state, person_id, &form.display_name) {
        Ok(true) => Redirect::to(&format!("/people/{person_id}")).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Html(render_not_found("/people"))).into_response(),
        Err(error) => {
            error!(person_id, %error, "failed to update person name");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_error(&error.to_string())),
            )
                .into_response()
        }
    }
}

async fn photo(State(state): State<WebState>, Path(photo_id): Path<i64>) -> Response {
    match photo_response(&state, photo_id) {
        Ok(Some((bytes, content_type))) => Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            error!(photo_id, %error, "failed to serve photo");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_error(&error.to_string())),
            )
                .into_response()
        }
    }
}

fn render_photo_view(state: &WebState, photo_id: i64) -> Result<Option<String>> {
    let start = Instant::now();
    let db = Database::open(&state.db_path)?;
    let Some(photo) = db.photo_detail(photo_id)? else {
        warn!(photo_id, "photo detail not found");
        return Ok(None);
    };

    info!(
        photo_id,
        elapsed_ms = elapsed_ms(start),
        "photo detail rendered"
    );
    Ok(Some(photo_page_html(&photo)))
}

fn render_people(state: &WebState) -> Result<String> {
    let start = Instant::now();
    let db = Database::open(&state.db_path)?;
    let people = db.people()?;
    info!(
        people_count = people.len(),
        elapsed_ms = elapsed_ms(start),
        "people page rendered"
    );
    Ok(people_page_html(&people))
}

fn render_person_view(state: &WebState, person_id: i64) -> Result<Option<String>> {
    let start = Instant::now();
    let db = Database::open(&state.db_path)?;
    let Some(person) = db.person_detail(person_id)? else {
        warn!(person_id, "person not found");
        return Ok(None);
    };
    let faces = db.person_faces(person_id)?;
    info!(
        person_id,
        face_count = faces.len(),
        elapsed_ms = elapsed_ms(start),
        "person page rendered"
    );
    Ok(Some(person_page_html(&person, &faces)))
}

fn save_person_name(state: &WebState, person_id: i64, display_name: &str) -> Result<bool> {
    let db = Database::open(&state.db_path)?;
    let trimmed = display_name.trim();
    let display_name = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    };
    db.update_person_name(person_id, display_name)
}

async fn not_found(State(_state): State<WebState>, uri: Uri) -> Response {
    warn!(path = %uri.path(), "route not found");
    (StatusCode::NOT_FOUND, Html(render_not_found(uri.path()))).into_response()
}

fn render_index(state: &WebState, query: Option<&str>) -> Result<String> {
    let page_start = Instant::now();
    let query = query.map(str::trim).filter(|query| !query.is_empty());
    info!(has_query = query.is_some(), "rendering index page");

    let db_start = Instant::now();
    let db = Database::open(&state.db_path)?;
    let indexed_count = db.indexed_photo_count()?;
    info!(
        indexed_count,
        elapsed_ms = elapsed_ms(db_start),
        "loaded library stats"
    );

    let results = if let Some(query) = query {
        info!(query, limit = SEARCH_LIMIT, "search started");
        let embedding_start = Instant::now();
        let embedding = {
            let embedder = state
                .text_embedder
                .lock()
                .map_err(|_| anyhow::anyhow!("text embedder lock is poisoned"))?;
            embedder
                .embed_text(query)
                .context("failed to embed search query")?
                .iter()
                .copied()
                .collect::<Vec<_>>()
        };
        info!(
            query,
            elapsed_ms = elapsed_ms(embedding_start),
            "search query embedded"
        );

        let search_start = Instant::now();
        let results = db.search_photos(query, &embedding, SEARCH_LIMIT)?;
        info!(
            query,
            result_count = results.len(),
            elapsed_ms = elapsed_ms(search_start),
            "database search completed"
        );
        results
    } else {
        Vec::new()
    };

    info!(
        has_query = query.is_some(),
        result_count = results.len(),
        elapsed_ms = elapsed_ms(page_start),
        "index page rendered"
    );

    Ok(page_html(indexed_count, query, &results))
}

fn photo_response(state: &WebState, photo_id: i64) -> Result<Option<(Vec<u8>, &'static str)>> {
    let start = Instant::now();
    info!(photo_id, "photo lookup started");
    let db = Database::open(&state.db_path)?;
    let Some(path) = db.photo_path(photo_id)? else {
        warn!(photo_id, "photo not found");
        return Ok(None);
    };

    if webp_cache::is_heic_path(FsPath::new(&path)) {
        return heic_webp_response(state, photo_id, &path, start).map(Some);
    }

    let bytes = std::fs::read(&path).with_context(|| format!("failed to read photo {path}"))?;
    let content_type = content_type(&path);
    info!(
        photo_id,
        path = %path,
        bytes = bytes.len(),
        content_type,
        elapsed_ms = elapsed_ms(start),
        "photo loaded"
    );
    Ok(Some((bytes, content_type)))
}

fn heic_webp_response(
    state: &WebState,
    photo_id: i64,
    source_path: &str,
    start: Instant,
) -> Result<(Vec<u8>, &'static str)> {
    let convert_start = Instant::now();
    let cached = webp_cache::ensure_heic_webp(&state.vips, &state.webp_cache_dir, source_path)?;
    let bytes = std::fs::read(&cached.path)
        .with_context(|| format!("failed to read cached WebP {}", cached.path.display()))?;

    if cached.cache_hit {
        info!(
            photo_id,
            source_path,
            cache_path = %cached.path.display(),
            bytes = bytes.len(),
            elapsed_ms = elapsed_ms(start),
            "served cached HEIC WebP"
        );
    } else {
        info!(
            photo_id,
            source_path,
            cache_path = %cached.path.display(),
            bytes = bytes.len(),
            convert_ms = elapsed_ms(convert_start),
            elapsed_ms = elapsed_ms(start),
            "converted and served HEIC WebP"
        );
    }

    Ok((bytes, "image/webp"))
}

fn elapsed_ms(start: Instant) -> u128 {
    start.elapsed().as_millis()
}

fn page_html(indexed_count: i64, query: Option<&str>, results: &[SearchResult]) -> String {
    let query_value = query.map_or_else(String::new, escape_html);
    let count = format_count(indexed_count);
    let results_html = if let Some(query) = query {
        if results.is_empty() {
            format!(
                r#"<section class="empty">No results for <strong>{}</strong>.</section>"#,
                escape_html(query)
            )
        } else {
            format!(
                r#"<section class="results"><div class="result-count">{} results for <strong>{}</strong></div><div class="grid">{}</div></section>"#,
                results.len(),
                escape_html(query),
                results.iter().map(result_card).collect::<String>()
            )
        }
    } else {
        String::new()
    };

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Iris</title>
<style>
:root {{ color-scheme: light; --bg: #f5f5f7; --panel: rgba(255,255,255,.78); --text: #1d1d1f; --muted: #6e6e73; --line: rgba(0,0,0,.08); }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Display", "Segoe UI", sans-serif; color: var(--text); background: radial-gradient(circle at top, #fff 0, var(--bg) 42rem); }}
main {{ width: min(1080px, calc(100% - 32px)); margin: 0 auto; padding: 84px 0 56px; }}
.hero {{ text-align: center; margin: 0 auto 36px; max-width: 720px; }}
h1 {{ margin: 0; font-size: clamp(48px, 8vw, 88px); line-height: .95; letter-spacing: -.06em; font-weight: 700; }}
.acronym {{ margin: 12px 0 0; color: var(--muted); font-size: 13px; letter-spacing: .12em; text-transform: uppercase; }}
.subtitle {{ margin: 18px 0 30px; color: var(--muted); font-size: 19px; }}
.hero-actions {{ display: flex; justify-content: center; margin: -12px 0 18px; }}
.pill {{ display: inline-flex; align-items: center; justify-content: center; min-height: 40px; padding: 0 16px; border: 1px solid var(--line); border-radius: 999px; color: var(--text); background: rgba(255,255,255,.64); text-decoration: none; font-weight: 650; box-shadow: 0 12px 32px rgba(0,0,0,.05); }}
.pill:hover {{ background: rgba(255,255,255,.9); }}
.search {{ display: flex; align-items: center; padding: 8px; border: 1px solid var(--line); border-radius: 24px; background: var(--panel); box-shadow: 0 24px 80px rgba(0,0,0,.08); backdrop-filter: blur(20px); }}
input {{ width: 100%; border: 0; outline: 0; background: transparent; padding: 15px 18px; font: inherit; font-size: 18px; color: var(--text); }}
button {{ border: 0; border-radius: 18px; padding: 13px 20px; font: inherit; font-weight: 600; color: white; background: #0071e3; cursor: pointer; }}
button:hover {{ background: #0077ed; }}
.result-count {{ margin: 0 0 18px; color: var(--muted); font-size: 15px; }}
.grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(190px, 1fr)); gap: 18px; }}
.card {{ overflow: hidden; border: 1px solid var(--line); border-radius: 24px; color: inherit; text-decoration: none; background: rgba(255,255,255,.72); box-shadow: 0 18px 50px rgba(0,0,0,.06); transition: transform .18s ease, box-shadow .18s ease; }}
.card:hover {{ transform: translateY(-2px); box-shadow: 0 24px 60px rgba(0,0,0,.1); }}
.thumb {{ display: block; width: 100%; aspect-ratio: 1; object-fit: cover; background: #e8e8ed; }}
.meta {{ padding: 13px 14px 15px; }}
.name {{ overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; font-size: 14px; }}
.detail {{ margin-top: 5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); font-size: 13px; }}
.empty {{ text-align: center; color: var(--muted); padding: 42px 0; }}
@media (max-width: 640px) {{ main {{ width: min(100% - 24px, 1080px); padding-top: 54px; }} .search {{ border-radius: 20px; }} button {{ padding: 12px 15px; }} .grid {{ grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }} .card {{ border-radius: 18px; }} }}
</style>
</head>
<body>
<main>
<section class="hero">
<h1>iris</h1>
<p class="acronym">intelligent retrieval for image search</p>
<p class="subtitle">{count} indexed photos in the library.</p>
<div class="hero-actions"><a class="pill" href="/people">People</a></div>
<form class="search" action="/" method="get">
<input name="q" type="search" value="{query_value}" placeholder="Search places, text, objects, moments" autofocus>
<button type="submit">Search</button>
</form>
</section>
{results_html}
</main>
</body>
</html>"#
    )
}

fn result_card(result: &SearchResult) -> String {
    let title = std::path::Path::new(&result.path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&result.path);
    let detail = result
        .geo_label
        .as_deref()
        .or(result.taken_at.as_deref())
        .or(result.camera_model.as_deref())
        .unwrap_or("Photo");
    let dimensions = match (result.width, result.height) {
        (Some(width), Some(height)) => format!(" · {width}×{height}"),
        _ => String::new(),
    };
    let quality = result
        .quality_score
        .map(|score| format!(" · quality {:.0}%", score * 100.0))
        .unwrap_or_default();
    let relevance = format!("{:.0}% match", (result.score * 100.0).clamp(0.0, 100.0));

    format!(
        r#"<a class="card" href="/view/{}" title="{}"><img class="thumb" src="/photos/{}" loading="lazy" alt=""><div class="meta"><div class="name">{}</div><div class="detail">{}{dimensions}{quality}</div></div></a>"#,
        result.id,
        escape_html(&relevance),
        result.id,
        escape_html(title),
        escape_html(detail),
    )
}

fn photo_page_html(photo: &SearchResult) -> String {
    let title = std::path::Path::new(&photo.path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&photo.path);
    let dimensions = match (photo.width, photo.height) {
        (Some(width), Some(height)) => Some(format!("{width} x {height}")),
        _ => None,
    };
    let quality = photo
        .quality_score
        .map(|score| format!("{:.0}%", score * 100.0));
    let metadata = [
        ("Location", photo.geo_label.as_deref()),
        ("Taken", photo.taken_at.as_deref()),
        ("Camera", photo.camera_model.as_deref()),
        ("Dimensions", dimensions.as_deref()),
        ("Quality", quality.as_deref()),
    ]
    .into_iter()
    .filter_map(|(label, value)| {
        value.map(|value| {
            format!(
                r#"<div class="metadata-row"><span>{}</span><strong>{}</strong></div>"#,
                escape_html(label),
                escape_html(value),
            )
        })
    })
    .collect::<String>();

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{}</title>
<style>
:root {{ color-scheme: light; --bg: #f5f5f7; --panel: rgba(255,255,255,.82); --text: #1d1d1f; --muted: #6e6e73; --line: rgba(0,0,0,.08); }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Display", "Segoe UI", sans-serif; color: var(--text); background: radial-gradient(circle at top, #fff 0, var(--bg) 42rem); }}
main {{ width: min(1180px, calc(100% - 32px)); margin: 0 auto; padding: 30px 0 56px; }}
.topbar {{ display: flex; align-items: center; justify-content: space-between; gap: 16px; margin-bottom: 24px; }}
.brand {{ color: var(--text); text-decoration: none; font-size: 22px; font-weight: 700; letter-spacing: -.05em; }}
.back {{ color: var(--muted); text-decoration: none; font-size: 15px; }}
.layout {{ display: grid; grid-template-columns: minmax(0, 1fr) 320px; gap: 22px; align-items: start; }}
.photo-panel, .info {{ border: 1px solid var(--line); border-radius: 30px; background: var(--panel); box-shadow: 0 24px 80px rgba(0,0,0,.08); backdrop-filter: blur(20px); }}
.photo-panel {{ display: grid; place-items: center; overflow: hidden; min-height: 60vh; padding: 18px; }}
.photo-panel img {{ display: block; max-width: 100%; max-height: 78vh; border-radius: 18px; object-fit: contain; }}
.info {{ padding: 24px; }}
h1 {{ margin: 0 0 18px; overflow-wrap: anywhere; font-size: 24px; line-height: 1.05; letter-spacing: -.04em; }}
.metadata-row {{ display: grid; gap: 5px; padding: 14px 0; border-top: 1px solid var(--line); }}
.metadata-row span {{ color: var(--muted); font-size: 12px; font-weight: 650; letter-spacing: .08em; text-transform: uppercase; }}
.metadata-row strong {{ font-size: 15px; line-height: 1.35; font-weight: 600; }}
@media (max-width: 860px) {{ main {{ padding-top: 18px; }} .layout {{ grid-template-columns: 1fr; }} .photo-panel {{ min-height: auto; }} }}
</style>
</head>
<body>
<main>
<nav class="topbar"><a class="brand" href="/">iris</a><a class="back" href="/">Back to search</a></nav>
<section class="layout">
<div class="photo-panel"><img src="/photos/{}" alt=""></div>
<aside class="info"><h1>{}</h1>{}</aside>
</section>
</main>
</body>
</html>"#,
        escape_html(title),
        photo.id,
        escape_html(title),
        metadata,
    )
}

fn people_page_html(people: &[PersonSummary]) -> String {
    let people_html = if people.is_empty() {
        r#"<section class="empty-panel"><h1>No people yet</h1><p>Run <code>cargo cluster-faces</code> after face indexing to build people clusters.</p></section>"#.to_string()
    } else {
        format!(
            r#"<section class="people-grid">{}</section>"#,
            people.iter().map(person_card).collect::<String>()
        )
    };

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>People - Iris</title>
<style>
:root {{ color-scheme: light; --bg: #f5f5f7; --panel: rgba(255,255,255,.78); --text: #1d1d1f; --muted: #6e6e73; --line: rgba(0,0,0,.08); --blue: #0071e3; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Display", "Segoe UI", sans-serif; color: var(--text); background: radial-gradient(circle at top, #fff 0, var(--bg) 42rem); }}
main {{ width: min(1180px, calc(100% - 32px)); margin: 0 auto; padding: 30px 0 56px; }}
.topbar {{ display: flex; align-items: center; justify-content: space-between; gap: 16px; margin-bottom: 34px; }}
.brand {{ color: var(--text); text-decoration: none; font-size: 22px; font-weight: 700; letter-spacing: -.05em; }}
.back {{ color: var(--muted); text-decoration: none; font-size: 15px; }}
.heading {{ margin-bottom: 26px; }}
h1 {{ margin: 0; font-size: clamp(42px, 7vw, 76px); line-height: .95; letter-spacing: -.055em; }}
.subtitle {{ margin: 12px 0 0; color: var(--muted); font-size: 18px; }}
.people-grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 18px; }}
.person-card {{ overflow: hidden; border: 1px solid var(--line); border-radius: 26px; color: inherit; text-decoration: none; background: var(--panel); box-shadow: 0 18px 50px rgba(0,0,0,.06); transition: transform .18s ease, box-shadow .18s ease; backdrop-filter: blur(20px); }}
.person-card:hover {{ transform: translateY(-2px); box-shadow: 0 24px 60px rgba(0,0,0,.1); }}
.face-crop {{ position: relative; display: block; width: 100%; aspect-ratio: 1; overflow: hidden; background: #e8e8ed; }}
.face-crop img {{ position: absolute; display: block; max-width: none; height: auto; }}
.face-crop img.full {{ inset: 0; width: 100%; height: 100%; object-fit: cover; }}
.placeholder {{ display: grid; place-items: center; width: 100%; aspect-ratio: 1; color: var(--muted); background: #e8e8ed; font-size: 42px; font-weight: 700; }}
.meta {{ padding: 14px 15px 16px; }}
.name {{ overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 700; font-size: 15px; }}
.detail {{ margin-top: 5px; color: var(--muted); font-size: 13px; }}
.empty-panel {{ width: min(560px, 100%); padding: 38px; border: 1px solid var(--line); border-radius: 32px; background: var(--panel); box-shadow: 0 24px 80px rgba(0,0,0,.08); }}
.empty-panel h1 {{ font-size: 34px; }}
.empty-panel p {{ margin: 14px 0 0; color: var(--muted); line-height: 1.45; }}
code {{ padding: 2px 6px; border-radius: 7px; background: rgba(0,0,0,.06); color: var(--text); }}
@media (max-width: 640px) {{ main {{ width: min(100% - 24px, 1180px); padding-top: 18px; }} .people-grid {{ grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }} .person-card {{ border-radius: 20px; }} }}
</style>
</head>
<body>
<main>
<nav class="topbar"><a class="brand" href="/">iris</a><a class="back" href="/">Back to search</a></nav>
<section class="heading"><h1>People</h1><p class="subtitle">{} clustered people.</p></section>
{}
</main>
</body>
</html>"#,
        people.len(),
        people_html,
    )
}

fn person_card(person: &PersonSummary) -> String {
    let label = person_label(person.id, person.display_name.as_deref());
    let face_count = format_face_count(person.face_count);
    let face_html = person
        .representative_face
        .as_ref()
        .map(face_crop_html)
        .unwrap_or_else(|| placeholder_html(&label));

    format!(
        r#"<a class="person-card" href="/people/{}">{}<div class="meta"><div class="name">{}</div><div class="detail">{}</div></div></a>"#,
        person.id,
        face_html,
        escape_html(&label),
        escape_html(&face_count),
    )
}

fn person_page_html(person: &PersonDetail, faces: &[PersonFace]) -> String {
    let label = person_label(person.id, person.display_name.as_deref());
    let name_value = person.display_name.as_deref().unwrap_or("");
    let faces_html = if faces.is_empty() {
        r#"<section class="empty-panel"><h1>No faces</h1><p>This person does not have any visible assigned faces.</p></section>"#.to_string()
    } else {
        format!(
            r#"<section class="face-grid">{}</section>"#,
            faces.iter().map(person_face_card).collect::<String>()
        )
    };

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{} - Iris</title>
<style>
:root {{ color-scheme: light; --bg: #f5f5f7; --panel: rgba(255,255,255,.82); --text: #1d1d1f; --muted: #6e6e73; --line: rgba(0,0,0,.08); --blue: #0071e3; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Display", "Segoe UI", sans-serif; color: var(--text); background: radial-gradient(circle at top, #fff 0, var(--bg) 42rem); }}
main {{ width: min(1180px, calc(100% - 32px)); margin: 0 auto; padding: 30px 0 56px; }}
.topbar {{ display: flex; align-items: center; justify-content: space-between; gap: 16px; margin-bottom: 28px; }}
.brand {{ color: var(--text); text-decoration: none; font-size: 22px; font-weight: 700; letter-spacing: -.05em; }}
.back {{ color: var(--muted); text-decoration: none; font-size: 15px; }}
.person-header {{ display: grid; gap: 20px; margin-bottom: 26px; padding: 26px; border: 1px solid var(--line); border-radius: 32px; background: var(--panel); box-shadow: 0 24px 80px rgba(0,0,0,.08); backdrop-filter: blur(20px); }}
h1 {{ margin: 0; font-size: clamp(38px, 7vw, 70px); line-height: .95; letter-spacing: -.055em; }}
.subtitle {{ margin: 8px 0 0; color: var(--muted); font-size: 17px; }}
.name-form {{ display: flex; align-items: center; gap: 10px; max-width: 560px; }}
.name-form input {{ min-width: 0; flex: 1; border: 1px solid var(--line); border-radius: 18px; outline: 0; padding: 13px 15px; font: inherit; color: var(--text); background: rgba(255,255,255,.75); }}
.name-form button {{ border: 0; border-radius: 18px; padding: 13px 18px; font: inherit; font-weight: 650; color: white; background: var(--blue); cursor: pointer; }}
.face-grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 14px; }}
.face-card {{ overflow: hidden; border: 1px solid var(--line); border-radius: 22px; color: inherit; text-decoration: none; background: var(--panel); box-shadow: 0 18px 50px rgba(0,0,0,.06); transition: transform .18s ease, box-shadow .18s ease; }}
.face-card:hover {{ transform: translateY(-2px); box-shadow: 0 24px 60px rgba(0,0,0,.1); }}
.face-crop {{ position: relative; display: block; width: 100%; aspect-ratio: 1; overflow: hidden; background: #e8e8ed; }}
.face-crop img {{ position: absolute; display: block; max-width: none; height: auto; }}
.face-crop img.full {{ inset: 0; width: 100%; height: 100%; object-fit: cover; }}
.face-meta {{ padding: 10px 12px 12px; color: var(--muted); font-size: 12px; }}
.empty-panel {{ padding: 34px; border: 1px solid var(--line); border-radius: 30px; background: var(--panel); color: var(--muted); }}
.empty-panel h1 {{ color: var(--text); font-size: 28px; }}
@media (max-width: 640px) {{ main {{ width: min(100% - 24px, 1180px); padding-top: 18px; }} .name-form {{ align-items: stretch; flex-direction: column; }} .face-grid {{ grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }} }}
</style>
</head>
<body>
<main>
<nav class="topbar"><a class="brand" href="/">iris</a><a class="back" href="/people">All people</a></nav>
<section class="person-header">
<div><h1>{}</h1><p class="subtitle">{}</p></div>
<form class="name-form" action="/people/{}/name" method="post">
<input name="display_name" value="{}" placeholder="Name this person" autocomplete="off">
<button type="submit">Save name</button>
</form>
</section>
{}
</main>
</body>
</html>"#,
        escape_html(&label),
        escape_html(&label),
        escape_html(&format_face_count(person.face_count)),
        person.id,
        escape_html(name_value),
        faces_html,
    )
}

fn person_face_card(face: &PersonFace) -> String {
    format!(
        r#"<a class="face-card" href="/view/{}">{}<div class="face-meta">Face #{}</div></a>"#,
        face.photo_id,
        face_crop_html(face),
        face.face_id,
    )
}

fn face_crop_html(face: &PersonFace) -> String {
    if let Some(style) = face_crop_style(face) {
        format!(
            r#"<span class="face-crop"><img src="/photos/{}" loading="lazy" alt="" style="{}"></span>"#,
            face.photo_id, style,
        )
    } else {
        format!(
            r#"<span class="face-crop"><img class="full" src="/photos/{}" loading="lazy" alt=""></span>"#,
            face.photo_id,
        )
    }
}

fn face_crop_style(face: &PersonFace) -> Option<String> {
    let width = face.photo_width? as f64;
    let height = face.photo_height? as f64;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    let x1 = face.bbox_x1.clamp(0.0, 1.0) * width;
    let y1 = face.bbox_y1.clamp(0.0, 1.0) * height;
    let x2 = face.bbox_x2.clamp(0.0, 1.0) * width;
    let y2 = face.bbox_y2.clamp(0.0, 1.0) * height;
    let bbox_width = (x2 - x1).max(1.0);
    let bbox_height = (y2 - y1).max(1.0);
    let side = (bbox_width.max(bbox_height) * FACE_CROP_PADDING)
        .min(width)
        .min(height)
        .max(1.0);
    let center_x = x1 + bbox_width / 2.0;
    let center_y = y1 + bbox_height / 2.0;
    let crop_x = (center_x - side / 2.0).clamp(0.0, (width - side).max(0.0));
    let crop_y = (center_y - side / 2.0).clamp(0.0, (height - side).max(0.0));

    let crop_x_ratio = crop_x / width;
    let crop_y_ratio = crop_y / height;
    let crop_w_ratio = side / width;
    let crop_h_ratio = side / height;
    if crop_w_ratio <= 0.0 || crop_h_ratio <= 0.0 {
        return None;
    }

    Some(format!(
        "width:{:.5}%;left:{:.5}%;top:{:.5}%;",
        100.0 / crop_w_ratio,
        -100.0 * crop_x_ratio / crop_w_ratio,
        -100.0 * crop_y_ratio / crop_h_ratio,
    ))
}

fn placeholder_html(label: &str) -> String {
    let initial = label.chars().next().unwrap_or('?');
    format!(
        r#"<span class="placeholder">{}</span>"#,
        escape_html(&initial.to_string())
    )
}

fn person_label(person_id: i64, display_name: Option<&str>) -> String {
    display_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Person #{person_id}"))
}

fn format_face_count(face_count: i64) -> String {
    if face_count == 1 {
        "1 face".to_string()
    } else {
        format!("{} faces", format_count(face_count))
    }
}

fn render_error(message: &str) -> String {
    format!(
        r#"<!doctype html><title>Iris</title><body style="font-family:-apple-system,BlinkMacSystemFont,sans-serif;padding:48px"><h1>Iris</h1><p>{}</p></body>"#,
        escape_html(message)
    )
}

fn render_not_found(path: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Iris - Not Found</title>
<style>
:root {{ color-scheme: light; --bg: #f5f5f7; --panel: rgba(255,255,255,.78); --text: #1d1d1f; --muted: #6e6e73; --line: rgba(0,0,0,.08); --blue: #0071e3; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Display", "Segoe UI", sans-serif; color: var(--text); background: radial-gradient(circle at top, #fff 0, var(--bg) 42rem); }}
.panel {{ width: min(520px, calc(100% - 32px)); padding: 42px; border: 1px solid var(--line); border-radius: 32px; text-align: center; background: var(--panel); box-shadow: 0 24px 80px rgba(0,0,0,.08); backdrop-filter: blur(20px); }}
.code {{ margin: 0 0 14px; color: var(--muted); font-size: 15px; font-weight: 600; letter-spacing: .08em; text-transform: uppercase; }}
h1 {{ margin: 0; font-size: clamp(38px, 8vw, 64px); line-height: .95; letter-spacing: -.055em; }}
p {{ margin: 18px 0 28px; color: var(--muted); font-size: 17px; line-height: 1.45; }}
a {{ display: inline-flex; align-items: center; justify-content: center; min-height: 46px; padding: 0 18px; border-radius: 999px; color: #fff; background: var(--blue); text-decoration: none; font-weight: 650; }}
code {{ padding: 2px 6px; border-radius: 7px; background: rgba(0,0,0,.06); color: var(--text); }}
</style>
</head>
<body>
<main class="panel">
<p class="code">404</p>
<h1>Page not found</h1>
<p>Iris does not have a route for <code>{}</code>.</p>
<a href="/">Back to Iris</a>
</main>
</body>
</html>"#,
        escape_html(path),
    )
}

fn format_count(count: i64) -> String {
    let value = count.to_string();
    let mut formatted = String::new();
    for (index, ch) in value.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(ch);
    }
    formatted.chars().rev().collect()
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html() {
        assert_eq!(escape_html("a<&\"'>"), "a&lt;&amp;&quot;&#39;&gt;");
    }

    #[test]
    fn formats_counts() {
        assert_eq!(format_count(123), "123");
        assert_eq!(format_count(1234), "1,234");
    }

    #[test]
    fn renders_root_links() {
        let html = page_html(1, None, &[]);
        assert!(html.contains(r#"action="/""#));
        assert!(html.contains(r#"href="/people""#));

        let result = SearchResult {
            id: 7,
            path: "/tmp/photo.jpg".into(),
            taken_at: None,
            camera_model: None,
            geo_label: None,
            quality_score: None,
            width: None,
            height: None,
            score: 0.5,
        };
        let card = result_card(&result);
        assert!(card.contains(r#"href="/view/7""#));
        assert!(card.contains(r#"src="/photos/7""#));
    }

    #[test]
    fn renders_photo_detail_page() {
        let photo = SearchResult {
            id: 9,
            path: "/tmp/Beach & sun.heic".into(),
            taken_at: Some("2026:05:17 10:11:12".into()),
            camera_model: Some("Phone".into()),
            geo_label: Some("Marseille, France".into()),
            quality_score: Some(0.91),
            width: Some(3000),
            height: Some(2000),
            score: 0.0,
        };
        let html = photo_page_html(&photo);

        assert!(html.contains(r#"src="/photos/9""#));
        assert!(html.contains("Beach &amp; sun.heic"));
        assert!(html.contains("Marseille, France"));
        assert!(html.contains("3000 x 2000"));
    }

    #[test]
    fn renders_people_pages() {
        let face = PersonFace {
            face_id: 11,
            photo_id: 9,
            bbox_x1: 0.25,
            bbox_y1: 0.20,
            bbox_x2: 0.45,
            bbox_y2: 0.55,
            photo_width: Some(3000),
            photo_height: Some(2000),
        };
        let person = PersonSummary {
            id: 3,
            display_name: Some("Alice & Bob".into()),
            face_count: 12,
            representative_face: Some(face.clone()),
        };

        let people_html = people_page_html(&[person]);
        assert!(people_html.contains(r#"href="/people/3""#));
        assert!(people_html.contains("Alice &amp; Bob"));
        assert!(people_html.contains(r#"src="/photos/9""#));

        let detail = PersonDetail {
            id: 3,
            display_name: Some("Alice & Bob".into()),
            face_count: 12,
        };
        let person_html = person_page_html(&detail, &[face]);
        assert!(person_html.contains(r#"action="/people/3/name""#));
        assert!(person_html.contains(r#"value="Alice &amp; Bob""#));
        assert!(person_html.contains(r#"href="/view/9""#));
    }

    #[test]
    fn computes_face_crop_style() {
        let face = PersonFace {
            face_id: 1,
            photo_id: 2,
            bbox_x1: 0.4,
            bbox_y1: 0.3,
            bbox_x2: 0.6,
            bbox_y2: 0.5,
            photo_width: Some(1000),
            photo_height: Some(1000),
        };

        let style = face_crop_style(&face).unwrap();
        assert!(style.contains("width:"));
        assert!(style.contains("left:"));
        assert!(style.contains("top:"));
    }

    #[test]
    fn renders_404_page() {
        let html = render_not_found("/missing");
        assert!(html.contains("Page not found"));
        assert!(html.contains(r#"href="/""#));
        assert!(html.contains("/missing"));
    }
}
