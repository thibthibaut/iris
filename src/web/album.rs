//! Album page (every Spread of an Edition) and the Spread editor.

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use iris_core::{
    album::edition_spreads,
    edit::{Edit, apply, pages},
    layout::page_options,
    model::{Crop, EditionDef, Override, PAGE_W, PhotoId, Spread, SpreadKind},
};
use serde::Deserialize;

use super::{AppState, WebResult, blocking, render, toast};
use crate::{
    album::{current, layout_shots, page_numbers, preview, unplaced_pins, warnings},
    db::Database,
    jobs::stage_status,
    state::AlbumState,
};

#[derive(Deserialize, Default)]
pub struct EditionQuery {
    #[serde(default)]
    edition: String,
}

pub struct SpreadCard {
    pub uid: u64,
    pub kind: SpreadKind,
    pub group: String,
    pub locked: bool,
    pub pages: (usize, usize),
    pub photos: usize,
    pub warnings: usize,
}

pub struct ChapterSection {
    pub key: String,
    pub title: String,
    pub spreads: Vec<SpreadCard>,
    pub core_count: usize,
}

#[derive(Template)]
#[template(path = "album.html")]
struct AlbumPage {
    nav: &'static str,
    editions: Vec<EditionDef>,
    edition: EditionDef,
    sections: Vec<ChapterSection>,
    has_album: bool,
    pages: usize,
    target: usize,
    couple_share: Option<f64>,
    couple_target: f64,
    unmet: Vec<(String, u32, u32)>,
    /// Pinned Photos with no room in the album.
    pins: Vec<(PhotoId, String)>,
    stale: Option<String>,
    building: bool,
}

pub fn chosen_edition(state: &AlbumState, key: &str) -> EditionDef {
    state.decisions.settings.edition(key).or_else(|| state.decisions.settings.editions.first()).cloned().unwrap_or(EditionDef {
        key: "ours".into(),
        name: "Nous".into(),
        group: None,
    })
}

fn album_page(state: &AppState, edition_key: &str) -> anyhow::Result<AlbumPage> {
    let db = state.db()?;
    let snapshot = state.snapshot(&db)?;
    let settings = &snapshot.decisions.settings;
    let edition = chosen_edition(&snapshot, edition_key);
    let album = current(&db, &snapshot)?;
    let select = state.jobs().stage("select").expect("select stage");
    let status = stage_status(&db, state.jobs().stages(), select.as_ref(), &snapshot.decisions)?;
    let mut sections = Vec::new();
    let mut couple_share = None;
    let mut unmet = Vec::new();
    let mut pins = Vec::new();
    let mut pages = 0;
    if let Some(album) = &album {
        let spreads = edition_spreads(album, &edition);
        pages = 2 + 2 * spreads.len();
        let numbers = page_numbers(spreads.iter().copied());
        for chapter in &settings.chapters {
            let in_chapter: Vec<&&Spread> = spreads.iter().filter(|s| s.chapter == chapter.key).collect();
            sections.push(ChapterSection {
                key: chapter.key.clone(),
                title: chapter.title_fr.clone(),
                core_count: in_chapter.iter().filter(|s| s.kind == SpreadKind::Core).count(),
                spreads: in_chapter
                    .iter()
                    .map(|spread| SpreadCard {
                        uid: spread.uid,
                        kind: spread.kind,
                        group: spread
                            .group
                            .as_ref()
                            .and_then(|g| settings.groups.iter().find(|x| &x.key == g))
                            .map(|g| g.name.clone())
                            .unwrap_or_default(),
                        locked: spread.locked,
                        pages: numbers.get(&spread.uid).copied().unwrap_or_default(),
                        photos: spread.photos.len(),
                        warnings: warnings(&snapshot, spread).len(),
                    })
                    .collect(),
            });
        }
        couple_share = Some(album.couple_share);
        for &(anchor, quota, reached) in &album.unmet_quotas {
            let name = snapshot.people.by_anchor(anchor).and_then(|p| p.name.clone()).unwrap_or_else(|| format!("Person {anchor}"));
            unmet.push((name, quota, reached));
        }
        pins = unplaced_pins(&snapshot, album);
    }
    let target = if edition.group.is_some() {
        let groups = album.as_ref().map_or(0, |a| a.spreads.iter().filter(|s| s.kind == SpreadKind::Group).count());
        settings.page_target as usize - 2 * groups.saturating_sub(1)
    } else {
        settings.page_target as usize
    };
    Ok(AlbumPage {
        nav: "album",
        editions: settings.editions.clone(),
        has_album: album.is_some(),
        pages,
        target,
        couple_share,
        couple_target: settings.couple_share,
        unmet,
        pins,
        stale: status.stale_reason,
        building: state.jobs().is_busy("select"),
        edition,
        sections,
    })
}

pub async fn page(State(state): State<AppState>, Query(query): Query<EditionQuery>) -> WebResult<Html<String>> {
    let page = blocking(move || album_page(&state, &query.edition)).await?;
    render(&page)
}

/// The preview PNG of a Spread, rendered on demand.
pub async fn preview_image(State(state): State<AppState>, Path(uid): Path<u64>, Query(query): Query<EditionQuery>) -> WebResult<Response> {
    let png = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let Some(album) = current(&db, &snapshot)? else { return Ok(None) };
        let edition = chosen_edition(&snapshot, &query.edition);
        let spreads = edition_spreads(&album, &edition);
        let numbers = page_numbers(spreads.iter().copied());
        let Some(spread) = album.spreads.iter().find(|s| s.uid == uid) else { return Ok(None) };
        let path = preview(state.config(), &snapshot, spread, numbers.get(&uid).copied().unwrap_or_default())?;
        Ok(Some(std::fs::read(path)?))
    })
    .await?;
    Ok(match png {
        Some(bytes) => ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-cache")], Body::from(bytes)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}

// ---- Spread editor ----------------------------------------------------------------------

pub struct Slot {
    pub photo: PhotoId,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub crop: Crop,
    pub name: String,
    pub pinned: bool,
    pub hero: bool,
    /// Effective print resolution in this Slot.
    pub ppi: u32,
}

#[derive(Template)]
#[template(path = "spread.html")]
struct SpreadPage {
    nav: &'static str,
    editor: Editor,
}

#[derive(Template)]
#[template(path = "spread_editor.html")]
pub struct Editor {
    pub uid: u64,
    pub edition: String,
    pub chapter: String,
    pub kind: SpreadKind,
    pub group: String,
    pub locked: bool,
    pub title: bool,
    pub version: String,
    pub slots: Vec<Slot>,
    pub templates: [String; 2],
    pub options: [Vec<(String, String, bool)>; 2],
    pub warnings: Vec<String>,
    pub prev: Option<u64>,
    pub next: Option<u64>,
    pub position: String,
}

fn describe(template: &str) -> String {
    match template.split_once(':') {
        Some(("rows", sizes)) => format!("Rows {}", sizes.replace('-', " + ")),
        Some(("cols", sizes)) => format!("Columns {}", sizes.replace('-', " + ")),
        _ if template == "hero" => "Hero (full page)".into(),
        _ => template.into(),
    }
}

fn editor(state: &AppState, db: &Database, snapshot: &AlbumState, uid: u64, edition_key: &str) -> anyhow::Result<Option<Editor>> {
    let Some(album) = current(db, snapshot)? else { return Ok(None) };
    let edition = chosen_edition(snapshot, edition_key);
    let spreads = edition_spreads(&album, &edition);
    let Some(index) = spreads.iter().position(|s| s.uid == uid) else { return Ok(None) };
    let spread = spreads[index];
    let settings = &snapshot.decisions.settings;
    let shots = layout_shots(snapshot);
    let (left, right) = pages(spread);
    let page_shots = |ids: &[PhotoId]| -> Vec<_> { ids.iter().filter_map(|id| shots.get(id).cloned()).collect() };
    let current_of = |page: usize| spread.templates[page].clone();
    let mut options = [
        if left.is_empty() { Vec::new() } else { page_options(&page_shots(&left), false, spread.title) },
        if right.is_empty() { Vec::new() } else { page_options(&page_shots(&right), true, false) },
    ]
    .map(|list| list.into_iter().take(8).map(|(t, _)| (t.clone(), describe(&t), false)).collect::<Vec<_>>());
    for (page, list) in options.iter_mut().enumerate() {
        for option in list.iter_mut() {
            option.2 = option.0 == current_of(page);
        }
    }
    let numbers = page_numbers(spreads.iter().copied());
    let preview_path = preview(state.config(), snapshot, spread, numbers.get(&uid).copied().unwrap_or_default())?;
    Ok(Some(Editor {
        uid,
        edition: edition.key.clone(),
        chapter: settings.chapters.iter().find(|c| c.key == spread.chapter).map(|c| c.title_fr.clone()).unwrap_or_default(),
        kind: spread.kind,
        group: spread.group.clone().unwrap_or_default(),
        locked: spread.locked,
        title: spread.title,
        version: preview_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        slots: spread
            .photos
            .iter()
            .map(|placed| Slot {
                photo: placed.photo,
                left: placed.rect.x / (2.0 * PAGE_W) * 100.0,
                top: placed.rect.y / 210.0 * 100.0,
                width: placed.rect.w / (2.0 * PAGE_W) * 100.0,
                height: placed.rect.h / 210.0 * 100.0,
                crop: placed.crop,
                name: snapshot.photo(placed.photo).map(|p| p.file_name.clone()).unwrap_or_default(),
                ppi: snapshot
                    .photo(placed.photo)
                    .and_then(|p| p.width)
                    .map_or(0, |w| ((w as f64 * placed.crop.w) / (placed.rect.w / 25.4)) as u32),
                pinned: snapshot.decisions.pins.contains(&placed.photo),
                hero: snapshot.decisions.heroes.contains(&placed.photo),
            })
            .collect(),
        templates: spread.templates.clone().map(|t| describe(&t)),
        options,
        warnings: warnings(snapshot, spread),
        prev: index.checked_sub(1).map(|i| spreads[i].uid),
        next: spreads.get(index + 1).map(|s| s.uid),
        position: format!("Spread {} of {} · pages {}–{}", index + 1, spreads.len(), 2 + 2 * index, 3 + 2 * index),
    }))
}

pub async fn spread_page(State(state): State<AppState>, Path(uid): Path<u64>, Query(query): Query<EditionQuery>) -> WebResult<Response> {
    let page = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        editor(&state, &db, &snapshot, uid, &query.edition)
    })
    .await?;
    Ok(match page {
        Some(editor) => render(&SpreadPage { nav: "album", editor })?.into_response(),
        None => axum::response::Redirect::to("/album").into_response(),
    })
}

#[derive(Deserialize, Default)]
pub struct EditForm {
    op: String,
    #[serde(default)]
    edition: String,
    #[serde(default)]
    a: Option<PhotoId>,
    #[serde(default)]
    b: Option<PhotoId>,
    #[serde(default)]
    page: Option<usize>,
    #[serde(default)]
    template: String,
    #[serde(default)]
    zoom: Option<f64>,
    #[serde(default)]
    dx: Option<f64>,
    #[serde(default)]
    dy: Option<f64>,
    #[serde(default)]
    position: Option<u32>,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    chapter: String,
}

/// One editing action on a Spread. Layout changes Lock the Spread with its new content.
pub async fn edit(State(state): State<AppState>, Path(uid): Path<u64>, axum::Form(form): axum::Form<EditForm>) -> WebResult<Response> {
    let edition = form.edition.clone();
    let (editor, label) = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let album = current(&db, &snapshot)?.ok_or_else(|| anyhow::anyhow!("build the album first"))?;
        let spread = album.spreads.iter().find(|s| s.uid == uid).ok_or_else(|| anyhow::anyhow!("this Spread no longer exists"))?.clone();
        let shots = layout_shots(&snapshot);
        let name = |id: Option<PhotoId>| id.and_then(|id| snapshot.photo(id)).map(|p| p.file_name.clone()).unwrap_or_default();
        let lock = |spread: Spread, label: String| -> anyhow::Result<String> {
            let mut spread = spread;
            spread.locked = true;
            state.decide(&db, Override::LockSpread { spread }, &label)?;
            Ok(label)
        };
        let edited = |edit: Edit| apply(&spread, &edit, &shots).ok_or_else(|| anyhow::anyhow!("that change does not fit this Spread"));
        let label = match form.op.as_str() {
            "swap" => lock(edited(Edit::Swap { a: form.a.unwrap_or(0), b: form.b.unwrap_or(0) })?, format!("Swapped {} and {}", name(form.a), name(form.b)))?,
            "place" => {
                let new = form.b.unwrap_or(0);
                anyhow::ensure!(!album.spreads.iter().any(|s| s.photos.iter().any(|p| p.photo == new)), "that photo is already in the album");
                lock(edited(Edit::Place { old: form.a.unwrap_or(0), new })?, format!("Put {} in place of {}", name(Some(new)), name(form.a)))?
            }
            "add" => {
                let new = form.b.unwrap_or(0);
                anyhow::ensure!(!album.spreads.iter().any(|s| s.photos.iter().any(|p| p.photo == new)), "that photo is already in the album");
                lock(edited(Edit::Add { photo: new, page: form.page.unwrap_or(1) })?, format!("Added {}", name(Some(new))))?
            }
            "remove" => lock(edited(Edit::Remove { photo: form.a.unwrap_or(0) })?, format!("Took {} out", name(form.a)))?,
            "ban" => {
                let photo = form.a.unwrap_or(0);
                db.add_override(&Override::Ban { photo, on: true }, &format!("Banned {}", name(form.a)))?;
                lock(edited(Edit::Remove { photo })?, format!("Took {} out", name(form.a)))?
            }
            "pin" => {
                let photo = form.a.unwrap_or(0);
                let on = !snapshot.decisions.pins.contains(&photo);
                let label = format!("{} {}", if on { "Pinned" } else { "Unpinned" }, name(form.a));
                state.decide(&db, Override::Pin { photo, on }, &label)?;
                label
            }
            "hero" => {
                let photo = form.a.unwrap_or(0);
                let on = !snapshot.decisions.heroes.contains(&photo);
                let label = format!("{} {} as Hero", if on { "Marked" } else { "Unmarked" }, name(form.a));
                state.decide(&db, Override::Hero { photo, on }, &label)?;
                label
            }
            "template" => {
                let page = form.page.unwrap_or(0);
                lock(edited(Edit::SetTemplate { page, template: form.template.clone() })?, format!("Changed the {} page to {}", if page == 0 { "left" } else { "right" }, describe(&form.template)))?
            }
            "relayout" => lock(edited(Edit::Relayout)?, "Re-laid out a Spread".into())?,
            "title" => lock(edited(Edit::SetTitle { title: !spread.title })?, if spread.title { "Removed a title".into() } else { "Added the Chapter title".into() })?,
            "crop" => {
                let photo = form.a.unwrap_or(0);
                let placed = spread.photos.iter().find(|p| p.photo == photo).ok_or_else(|| anyhow::anyhow!("not on this Spread"))?;
                let mut crop = placed.crop;
                if let Some(zoom) = form.zoom {
                    // Zoom around the center, keeping the Slot's shape.
                    let (cx, cy) = (crop.x + crop.w / 2.0, crop.y + crop.h / 2.0);
                    let factor = zoom.clamp(0.5, 2.0);
                    crop.w = (crop.w / factor).min(1.0);
                    crop.h = (crop.h / factor).min(1.0);
                    let scale = (crop.w / placed.crop.w).min(crop.h / placed.crop.h);
                    crop.w = placed.crop.w * scale;
                    crop.h = placed.crop.h * scale;
                    crop.x = cx - crop.w / 2.0;
                    crop.y = cy - crop.h / 2.0;
                }
                crop.x += form.dx.unwrap_or(0.0) * crop.w;
                crop.y += form.dy.unwrap_or(0.0) * crop.h;
                crop.x = crop.x.clamp(0.0, (1.0 - crop.w).max(0.0));
                crop.y = crop.y.clamp(0.0, (1.0 - crop.h).max(0.0));
                if form.op == "crop" && form.zoom.is_none() && form.dx.is_none() && form.dy.is_none() {
                    // Reset: the face-aware crop for this Slot.
                    let shot = shots.get(&photo).ok_or_else(|| anyhow::anyhow!("unknown photo"))?;
                    crop = iris_core::layout::face_crop(shot.aspect, placed.rect.aspect(), &shot.faces).0;
                }
                lock(edited(Edit::Crop { photo, crop })?, format!("Cropped {}", name(form.a)))?
            }
            "lock" => lock(spread.clone(), "Locked a Spread".into())?,
            "unlock" => {
                state.decide(&db, Override::UnlockSpread { uid }, "Unlocked a Spread")?;
                "Unlocked: the next build may change it".into()
            }
            "move" => {
                let mut moved = spread.clone();
                moved.position = form.position.unwrap_or(0);
                lock(moved, "Moved a Spread".into())?
            }
            "chapter_spreads" => {
                let label = format!("{} now has {} Spreads (rebuild to apply)", form.chapter, form.count.unwrap_or(1));
                state.decide(&db, Override::SetChapterSpreads { chapter: form.chapter.clone(), count: form.count.unwrap_or(1) }, &label)?;
                label
            }
            _ => anyhow::bail!("unknown action {}", form.op),
        };
        let snapshot = state.snapshot(&db)?;
        let editor = editor(&state, &db, &snapshot, uid, &form.edition)?;
        // Say so when a photo is too small for its Slot.
        let low: Vec<String> = editor
            .as_ref()
            .map(|e| e.slots.iter().filter(|s| s.ppi > 0 && s.ppi < 250).map(|s| format!("{} at {} ppi", s.name, s.ppi)).collect())
            .unwrap_or_default();
        let label = if low.is_empty() { label } else { format!("{label} — too small to print sharply: {}", low.join(", ")) };
        Ok((editor, label))
    })
    .await?;
    let _ = edition;
    match editor {
        Some(editor) => Ok(toast(render(&editor)?, &label)),
        None => Ok(toast(Html(String::new()), &label)),
    }
}

#[derive(Template)]
#[template(path = "tray.html")]
struct Tray {
    uid: u64,
    edition: String,
    photo: PhotoId,
    name: String,
    query: String,
    candidates: Vec<(PhotoId, String, bool)>,
}

#[derive(Deserialize, Default)]
pub struct TrayQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    edition: String,
}

/// Alternatives for one Slot: the selector's ranked list, or a text search.
pub async fn tray(State(state): State<AppState>, Path((uid, photo)): Path<(u64, PhotoId)>, Query(query): Query<TrayQuery>) -> WebResult<Html<String>> {
    let tray = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let album = current(&db, &snapshot)?.unwrap_or_default();
        let used: std::collections::HashSet<PhotoId> = album.spreads.iter().flat_map(|s| s.photos.iter().map(|p| p.photo)).collect();
        let ids: Vec<PhotoId> = if query.q.trim().is_empty() {
            album.alternatives.get(&photo).cloned().unwrap_or_default()
        } else {
            let vector = state.embed_query(query.q.trim())?;
            db.search_images(&vector, 60)?.into_iter().map(|(id, _)| id).filter(|id| snapshot.photo(*id).is_some_and(|p| p.shot_id.is_none_or(|s| s == *id))).collect()
        };
        let candidates = ids
            .into_iter()
            .filter(|id| !used.contains(id))
            .filter_map(|id| {
                let p = snapshot.photo(id)?;
                let usable = matches!(snapshot.eligibility(id), iris_core::decisions::Eligibility::Eligible | iris_core::decisions::Eligibility::Pinned);
                Some((id, p.file_name.clone(), usable))
            })
            .take(24)
            .collect();
        Ok(Tray {
            uid,
            edition: query.edition,
            photo,
            name: snapshot.photo(photo).map(|p| p.file_name.clone()).unwrap_or_default(),
            query: query.q,
            candidates,
        })
    })
    .await?;
    render(&tray)
}
