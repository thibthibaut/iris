//! Text & design (titles in French and Arabic, cover, colours, fonts) and Export.

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use iris_core::{
    album::edition_spreads,
    model::{Design, EditionDef, Override},
};
use serde::Deserialize;

use super::{AppState, WebResult, blocking, pipeline::ago, render, toast};
use crate::{album::current, render::Texts};

pub struct TextRow {
    pub key: String,
    pub label: String,
    pub fr: String,
    pub ar: String,
}

#[derive(Template)]
#[template(path = "design.html")]
struct DesignPage {
    nav: &'static str,
    rows: Vec<TextRow>,
    design: Design,
    fonts: Vec<&'static str>,
    arabic_fonts: Vec<&'static str>,
    cover_photo: String,
    colors: Vec<(&'static str, &'static str, String)>,
}

pub async fn page(State(state): State<AppState>) -> WebResult<Html<String>> {
    let page = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let settings = &snapshot.decisions.settings;
        let texts = Texts::from(settings, &snapshot.decisions.texts);
        let mut rows: Vec<TextRow> = settings
            .chapters
            .iter()
            .map(|c| {
                let (fr, ar) = texts.chapter_titles.get(&c.key).cloned().unwrap_or_default();
                TextRow { key: format!("chapter.{}", c.key), label: format!("Chapter: {}", c.title_fr), fr, ar }
            })
            .collect();
        rows.extend(settings.groups.iter().map(|g| {
            let (fr, ar) = texts.group_titles.get(&g.key).cloned().unwrap_or_default();
            TextRow { key: format!("group.{}", g.key), label: format!("Group Spread: {}", g.name), fr, ar }
        }));
        rows.push(TextRow { key: "opening".into(), label: "Opening page".into(), fr: texts.opening.clone(), ar: texts.opening_ar.clone() });
        rows.push(TextRow { key: "closing".into(), label: "Closing page".into(), fr: texts.closing.clone(), ar: texts.closing_ar.clone() });
        Ok(DesignPage {
            nav: "design",
            rows,
            colors: {
                let d = &settings.design;
                vec![
                    ("paper", "Paper", d.paper.clone()),
                    ("ink", "Ink", d.ink.clone()),
                    ("emerald", "Emerald", d.emerald.clone()),
                    ("red_orange", "Red-orange", d.red_orange.clone()),
                    ("pink", "Sunset pink", d.pink.clone()),
                    ("yellow", "Yellow", d.yellow.clone()),
                    ("blue", "Electric blue", d.blue.clone()),
                ]
            },
            design: settings.design.clone(),
            fonts: vec!["Righteous", "Bungee", "Monoton", "Shrikhand", "Playfair Display"],
            arabic_fonts: vec!["Lalezar", "Reem Kufi", "Aref Ruqaa", "Noto Naskh Arabic"],
            cover_photo: snapshot.decisions.texts.get("cover.photo").cloned().unwrap_or_default(),
        })
    })
    .await?;
    render(&page)
}

#[derive(Deserialize)]
pub struct TextForm {
    key: String,
    value: String,
}

pub async fn save_text(State(state): State<AppState>, axum::Form(form): axum::Form<TextForm>) -> WebResult<Response> {
    let label = blocking(move || {
        let db = state.db()?;
        let label = format!("Text “{}”", form.key);
        state.decide(&db, Override::SetText { key: form.key, value: form.value }, &label)?;
        Ok(label)
    })
    .await?;
    Ok(toast(Html(String::new()), &format!("Saved {label}")))
}

// ---- Export -------------------------------------------------------------------------------

pub struct ExportCard {
    pub edition: EditionDef,
    pub pages: usize,
    pub busy: bool,
    pub versions: Vec<ExportVersion>,
}

pub struct ExportVersion {
    pub id: i64,
    pub version: i64,
    pub when: String,
    pub exists: bool,
    pub size: String,
    pub pages: i64,
    pub problems: Vec<String>,
    pub changed: Option<usize>,
}

#[derive(Template)]
#[template(path = "export.html")]
struct ExportPage {
    nav: &'static str,
    cards: Vec<ExportCard>,
    printer: String,
    has_album: bool,
}

pub async fn export_page(State(state): State<AppState>) -> WebResult<Html<String>> {
    let page = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let settings = &snapshot.decisions.settings;
        let album = current(&db, &snapshot)?;
        let exports = db.exports()?;
        let current_uids: Vec<String> = Vec::new();
        let _ = current_uids;
        let cards = settings
            .editions
            .iter()
            .map(|edition| {
                let spreads: Vec<String> = album
                    .as_ref()
                    .map(|a| edition_spreads(a, edition).iter().map(|s| s.uid.to_string()).collect())
                    .unwrap_or_default();
                let mut versions: Vec<ExportVersion> = exports
                    .iter()
                    .filter(|(_, e, ..)| e == &edition.key)
                    .map(|(id, _, version, path, created, manifest)| {
                        let exported: Vec<String> = manifest["spreads"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
                        let changed = spreads.iter().filter(|uid| !exported.contains(uid)).count() + exported.iter().filter(|uid| !spreads.contains(uid)).count();
                        let metadata = std::fs::metadata(path).ok();
                        ExportVersion {
                            id: *id,
                            version: *version,
                            when: ago(*created),
                            exists: metadata.is_some(),
                            size: metadata.map(|m| super::pipeline::human_bytes(m.len())).unwrap_or_default(),
                            pages: manifest["interior_pages"].as_i64().unwrap_or(0),
                            problems: manifest["problems"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                            changed: Some(changed),
                        }
                    })
                    .collect();
                versions.sort_by(|a, b| b.version.cmp(&a.version));
                ExportCard {
                    pages: 2 + 2 * spreads.len(),
                    busy: state.jobs().is_busy(&format!("export:{}", edition.key)),
                    edition: edition.clone(),
                    versions,
                }
            })
            .collect();
        Ok(ExportPage { nav: "export", cards, printer: settings.printer.clone(), has_album: album.is_some() })
    })
    .await?;
    render(&page)
}

pub async fn download(State(state): State<AppState>, Path(id): Path<i64>) -> WebResult<Response> {
    let file = blocking(move || {
        let exports = state.db()?.exports()?;
        let Some((_, edition, version, path, ..)) = exports.into_iter().find(|e| e.0 == id) else { return Ok(None) };
        Ok(std::fs::read(&path).ok().map(|bytes| (bytes, format!("album-{edition}-v{version}.pdf"))))
    })
    .await?;
    Ok(match file {
        Some((bytes, name)) => (
            [
                (header::CONTENT_TYPE, "application/pdf".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            ],
            Body::from(bytes),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}

pub async fn delete_export(State(state): State<AppState>, Path(id): Path<i64>) -> WebResult<Response> {
    blocking(move || {
        if let Some(path) = state.db()?.delete_export(id)? {
            let _ = std::fs::remove_file(path);
        }
        state.jobs().notify("export");
        Ok(())
    })
    .await?;
    Ok(([("HX-Refresh", "true")], Html(String::new())).into_response())
}
