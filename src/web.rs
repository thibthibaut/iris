use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use open_clip_inference::TextEmbedder;
use serde::Deserialize;
use tracing::info;

use crate::{
    config::Config,
    db::{Database, SearchResult},
};

const MOBILE_CLIP_MODEL_ID: &str = "RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX";
const SEARCH_LIMIT: usize = 48;

#[derive(Clone)]
struct WebState {
    db_path: PathBuf,
    text_embedder: Arc<Mutex<TextEmbedder>>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
}

pub async fn serve(config: Config, host: String, port: u16) -> Result<()> {
    let text_embedder = TextEmbedder::from_hf(MOBILE_CLIP_MODEL_ID)
        .build()
        .await
        .context("failed to initialize MobileCLIP text embedder")?;
    let state = WebState {
        db_path: config.database_path,
        text_embedder: Arc::new(Mutex::new(text_embedder)),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/photos/:id", get(photo))
        .with_state(state);
    let address = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("failed to bind web server on {address}"))?;

    info!(url = %format!("http://{address}"), "web server listening");
    axum::serve(listener, app)
        .await
        .context("web server failed")
}

async fn index(State(state): State<WebState>, Query(params): Query<SearchParams>) -> Response {
    match render_index(&state, params.q.as_deref()) {
        Ok(html) => Html(html).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(render_error(&error.to_string())),
        )
            .into_response(),
    }
}

async fn photo(State(state): State<WebState>, Path(photo_id): Path<i64>) -> Response {
    match read_photo(&state, photo_id) {
        Ok(Some((bytes, content_type))) => Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(render_error(&error.to_string())),
        )
            .into_response(),
    }
}

fn render_index(state: &WebState, query: Option<&str>) -> Result<String> {
    let db = Database::open(&state.db_path)?;
    let indexed_count = db.indexed_photo_count()?;
    let query = query.map(str::trim).filter(|query| !query.is_empty());
    let results = if let Some(query) = query {
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
        db.search_photos(query, &embedding, SEARCH_LIMIT)?
    } else {
        Vec::new()
    };

    Ok(page_html(indexed_count, query, &results))
}

fn read_photo(state: &WebState, photo_id: i64) -> Result<Option<(Vec<u8>, &'static str)>> {
    let db = Database::open(&state.db_path)?;
    let Some(path) = db.photo_path(photo_id)? else {
        return Ok(None);
    };
    let bytes = std::fs::read(&path).with_context(|| format!("failed to read photo {path}"))?;
    Ok(Some((bytes, content_type(&path))))
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
.subtitle {{ margin: 18px 0 30px; color: var(--muted); font-size: 19px; }}
.search {{ display: flex; align-items: center; padding: 8px; border: 1px solid var(--line); border-radius: 24px; background: var(--panel); box-shadow: 0 24px 80px rgba(0,0,0,.08); backdrop-filter: blur(20px); }}
input {{ width: 100%; border: 0; outline: 0; background: transparent; padding: 15px 18px; font: inherit; font-size: 18px; color: var(--text); }}
button {{ border: 0; border-radius: 18px; padding: 13px 20px; font: inherit; font-weight: 600; color: white; background: #0071e3; cursor: pointer; }}
button:hover {{ background: #0077ed; }}
.result-count {{ margin: 0 0 18px; color: var(--muted); font-size: 15px; }}
.grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(190px, 1fr)); gap: 18px; }}
.card {{ overflow: hidden; border: 1px solid var(--line); border-radius: 24px; background: rgba(255,255,255,.72); box-shadow: 0 18px 50px rgba(0,0,0,.06); }}
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
<h1>Iris</h1>
<p class="subtitle">{count} indexed photos in the library.</p>
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
        r#"<article class="card" title="{}"><img class="thumb" src="/photos/{}" loading="lazy" alt=""><div class="meta"><div class="name">{}</div><div class="detail">{}{dimensions}{quality}</div></div></article>"#,
        escape_html(&relevance),
        result.id,
        escape_html(title),
        escape_html(detail),
    )
}

fn render_error(message: &str) -> String {
    format!(
        r#"<!doctype html><title>Iris</title><body style="font-family:-apple-system,BlinkMacSystemFont,sans-serif;padding:48px"><h1>Iris</h1><p>{}</p></body>"#,
        escape_html(message)
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
}
