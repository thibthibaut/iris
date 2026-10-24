//! Photos page: every Photo, filters, detail drawer, Pin / Ban / Hero / Chapter moves.

use std::collections::{HashMap, HashSet};

use askama::Template;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use iris_core::{
    decisions::Eligibility,
    model::{Override, PhotoId, Source},
};
use serde::Deserialize;

use super::{AppState, WebResult, blocking, render, toast};
use crate::{
    db::{Database, PhotoRecord},
    state::AlbumState,
    image_io::{GRID_SIZE, PREVIEW_SIZE, proxy_path},
};

const PAGE_SIZE: usize = 120;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Filters {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub chapter: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub person: String,
    #[serde(default)]
    pub shot: String,
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub offset: usize,
}

impl Filters {
    fn query(&self, offset: usize) -> String {
        let mut pairs = vec![];
        for (key, value) in [
            ("source", &self.source),
            ("chapter", &self.chapter),
            ("status", &self.status),
            ("person", &self.person),
            ("shot", &self.shot),
            ("q", &self.q),
        ] {
            if !value.is_empty() {
                pairs.push(format!("{key}={}", urlencode(value)));
            }
        }
        pairs.push(format!("offset={offset}"));
        pairs.join("&")
    }
}

pub fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub struct Tile {
    pub photo: PhotoRecord,
    pub status: &'static str,
    pub status_label: String,
    pub shot_size: usize,
    pub in_album: bool,
}

#[derive(Template)]
#[template(path = "photos.html")]
struct PhotosPage {
    nav: &'static str,
    filters: Filters,
    chapters: Vec<(String, String)>,
    persons: Vec<(i64, String)>,
    grid: PhotoGrid,
    total: usize,
}

#[derive(Template)]
#[template(path = "photo_grid.html")]
pub struct PhotoGrid {
    pub tiles: Vec<Tile>,
    pub next: Option<String>,
}

/// Photo ids currently placed in the album (any Edition).
pub fn album_photos(db: &Database) -> anyhow::Result<HashSet<PhotoId>> {
    let proposal: Option<iris_core::model::Proposal> = db.blob("proposal")?;
    Ok(proposal
        .map(|proposal| proposal.spreads.iter().flat_map(|s| s.photos.iter().map(|p| p.photo)).collect())
        .unwrap_or_default())
}

pub fn tile(photo: PhotoRecord, snapshot: &AlbumState, shot_sizes: &HashMap<i64, usize>, in_album: &HashSet<PhotoId>) -> Tile {
    let eligibility = snapshot.eligibility(photo.id);
    let (status, status_label) = match &eligibility {
        Eligibility::Banned => ("banned", "Banned".to_string()),
        Eligibility::Pinned => ("pinned", "Pinned".to_string()),
        Eligibility::Rejected(reason) => ("rejected", reason.clone()),
        Eligibility::Eligible => ("ok", String::new()),
    };
    Tile {
        shot_size: photo.shot_id.and_then(|shot| shot_sizes.get(&shot).copied()).unwrap_or(1),
        in_album: in_album.contains(&photo.id),
        photo,
        status,
        status_label,
    }
}

fn filtered(state: &AppState, filters: &Filters) -> anyhow::Result<(Vec<Tile>, usize)> {
    let db = state.db()?;
    let snapshot = state.snapshot(&db)?;
    let mut photos = snapshot.photos.clone();
    let shot_sizes = shot_sizes(&photos);
    let in_album = album_photos(&db)?;

    if !filters.q.trim().is_empty() {
        let query = state.embed_query(filters.q.trim())?;
        let ranked = db.search_images(&query, 200)?;
        let rank: HashMap<PhotoId, usize> = ranked.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();
        photos.retain(|photo| rank.contains_key(&photo.id));
        photos.sort_by_key(|photo| rank[&photo.id]);
    }
    if let Ok(person) = filters.person.parse::<i64>()
        && let Some(person) = snapshot.people.by_anchor(person)
    {
        let faces: HashSet<_> = person.faces.iter().collect();
        let with_person: HashSet<PhotoId> =
            snapshot.faces.iter().filter(|face| faces.contains(&face.id)).map(|face| face.photo_id).collect();
        photos.retain(|photo| with_person.contains(&photo.id));
    }
    if let Ok(shot) = filters.shot.parse::<i64>() {
        photos.retain(|photo| photo.shot_id == Some(shot));
    }
    let tiles: Vec<Tile> = photos
        .into_iter()
        .filter(|photo| match filters.source.as_str() {
            "photographer" => photo.source == Some(Source::Photographer),
            "guest" => photo.source == Some(Source::Guest),
            _ => true,
        })
        .filter(|photo| match filters.chapter.as_str() {
            "" => true,
            "none" => snapshot.chapter_of(photo).is_none(),
            chapter => snapshot.chapter_of(photo).as_deref() == Some(chapter),
        })
        .map(|photo| tile(photo, &snapshot, &shot_sizes, &in_album))
        .filter(|tile| match filters.status.as_str() {
            "" => true,
            "in_album" => tile.in_album,
            "not_in_album" => !tile.in_album && tile.status != "rejected" && tile.status != "banned",
            "duplicates" => tile.shot_size > 1,
            status => tile.status == status,
        })
        .collect();
    let total = tiles.len();
    Ok((tiles, total))
}

pub fn shot_sizes(photos: &[PhotoRecord]) -> HashMap<i64, usize> {
    let mut sizes = HashMap::new();
    for shot in photos.iter().filter_map(|photo| photo.shot_id) {
        *sizes.entry(shot).or_insert(0) += 1;
    }
    sizes
}

fn page_of(mut tiles: Vec<Tile>, filters: &Filters, total: usize) -> PhotoGrid {
    let end = (filters.offset + PAGE_SIZE).min(tiles.len());
    let tiles: Vec<Tile> = tiles.drain(filters.offset.min(end)..end).collect();
    PhotoGrid { next: (end < total).then(|| filters.query(end)), tiles }
}

pub async fn page(State(state): State<AppState>, Query(filters): Query<Filters>) -> WebResult<Html<String>> {
    let page = blocking(move || {
        let (tiles, total) = filtered(&state, &filters)?;
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        Ok(PhotosPage {
            nav: "photos",
            chapters: snapshot
                .decisions
                .settings
                .chapters
                .iter()
                .map(|c| (c.key.clone(), c.title_fr.clone()))
                .collect(),
            persons: snapshot
                .people
                .persons
                .iter()
                .filter_map(|p| p.name.clone().map(|name| (p.anchor, name)))
                .collect(),
            grid: page_of(tiles, &filters, total),
            filters,
            total,
        })
    })
    .await?;
    render(&page)
}

pub async fn grid(State(state): State<AppState>, Query(filters): Query<Filters>) -> WebResult<Html<String>> {
    let grid = blocking(move || {
        let (tiles, total) = filtered(&state, &filters)?;
        Ok(page_of(tiles, &filters, total))
    })
    .await?;
    render(&grid)
}

#[derive(Template)]
#[template(path = "photo_detail.html")]
struct Detail {
    tile: Tile,
    taken: String,
    sent: String,
    faces: Vec<DetailFace>,
    shot: Vec<PhotoRecord>,
    chapters: Vec<(String, String)>,
    chapter_title: String,
    is_hero: bool,
    evaluation: Option<iris_core::quality::Evaluation>,
}

struct DetailFace {
    id: i64,
    name: String,
    anchor: Option<i64>,
    soft: bool,
    small: bool,
    sharpness: String,
}

pub fn paris_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&chrono_tz::Europe::Paris).format("%a %d %b %Y, %H:%M:%S").to_string())
        .unwrap_or_default()
}

fn load_detail(state: &AppState, id: PhotoId) -> anyhow::Result<Option<Detail>> {
    let db = state.db()?;
    let Some(photo) = db.photo(id)? else { return Ok(None) };
    let snapshot = state.snapshot(&db)?;
    let all = &snapshot.photos;
    let shot: Vec<PhotoRecord> = match photo.shot_id {
        Some(shot) => all.iter().filter(|p| p.shot_id == Some(shot) && p.id != id).cloned().collect(),
        None => Vec::new(),
    };
    let settings = &snapshot.decisions.settings;
    let faces = snapshot
        .faces
        .iter()
        .filter(|face| face.photo_id == id)
        .map(|face| {
            let person = snapshot.people.person_of_face(face.id);
            DetailFace {
                id: face.id,
                name: person.and_then(|p| p.name.clone()).unwrap_or_else(|| {
                    if person.is_some() { "Unnamed".into() } else { "Unknown".into() }
                }),
                anchor: person.map(|p| p.anchor),
                soft: person.is_some_and(|p| p.soft_faces.contains(&face.id)),
                small: face.height_frac() < settings.face_min_height_frac,
                sharpness: face.sharpness.map(|s| format!("{s:.2}")).unwrap_or_default(),
            }
        })
        .collect();
    let shot_sizes = shot_sizes(all);
    let in_album = album_photos(&db)?;
    Ok(Some(Detail {
        taken: photo.taken_at_utc.map(paris_time).unwrap_or_default(),
        sent: photo.sent_at_utc.map(paris_time).unwrap_or_default(),
        evaluation: snapshot.evaluations.get(&id).cloned(),
        chapter_title: snapshot
            .chapter_of(&photo)
            .as_ref()
            .and_then(|key| settings.chapters.iter().find(|c| &c.key == key))
            .map(|c| c.title_fr.clone())
            .unwrap_or_else(|| "—".into()),
        chapters: settings.chapters.iter().map(|c| (c.key.clone(), c.title_fr.clone())).collect(),
        is_hero: snapshot.decisions.heroes.contains(&id),
        tile: tile(photo, &snapshot, &shot_sizes, &in_album),
        faces,
        shot,
    }))
}

pub async fn detail(State(state): State<AppState>, Path(id): Path<PhotoId>) -> WebResult<Response> {
    match blocking(move || load_detail(&state, id)).await? {
        Some(detail) => Ok(render(&detail)?.into_response()),
        None => Ok((StatusCode::NOT_FOUND, Html("Photo not found".to_string())).into_response()),
    }
}

#[derive(Deserialize)]
pub struct ActForm {
    #[serde(default)]
    chapter: String,
}

pub async fn act(
    State(state): State<AppState>,
    Path((id, action)): Path<(PhotoId, String)>,
    axum::Form(form): axum::Form<ActForm>,
) -> WebResult<Response> {
    let (detail, message) = blocking(move || {
        let db = state.db()?;
        let photo = db.photo(id)?.ok_or_else(|| anyhow::anyhow!("no photo {id}"))?;
        let snapshot = state.snapshot(&db)?;
        let decisions = &snapshot.decisions;
        let name = &photo.file_name;
        let (item, label) = match action.as_str() {
            "pin" => {
                let on = !decisions.pins.contains(&id);
                (Override::Pin { photo: id, on }, format!("{} {name}", if on { "Pinned" } else { "Unpinned" }))
            }
            "ban" => {
                let on = !decisions.bans.contains(&id);
                (Override::Ban { photo: id, on }, format!("{} {name}", if on { "Banned" } else { "Unbanned" }))
            }
            "hero" => {
                let on = !decisions.heroes.contains(&id);
                (Override::Hero { photo: id, on }, format!("{} {name} as Hero", if on { "Marked" } else { "Unmarked" }))
            }
            "chapter" => {
                let title = decisions
                    .settings
                    .chapters
                    .iter()
                    .find(|c| c.key == form.chapter)
                    .map(|c| c.title_fr.clone())
                    .ok_or_else(|| anyhow::anyhow!("unknown chapter"))?;
                (Override::MoveChapter { photo: id, chapter: form.chapter.clone() }, format!("Moved {name} to {title}"))
            }
            "split-shot" => (Override::SplitShot { photo: id }, format!("Took {name} out of its Shot")),
            _ => anyhow::bail!("unknown action {action}"),
        };
        state.decide(&db, item, &label)?;
        Ok((load_detail(&state, id)?, label))
    })
    .await?;
    let detail = detail.ok_or_else(|| anyhow::anyhow!("photo vanished"))?;
    Ok(toast(render(&detail)?, &message))
}

/// The WebP proxy of a Photo: `s` = 400 px, `m` = 1200 px.
pub async fn image(State(state): State<AppState>, Path((id, size)): Path<(PhotoId, String)>) -> WebResult<Response> {
    let size = if size == "m" { PREVIEW_SIZE } else { GRID_SIZE };
    let webp_dir = state.config().webp_dir();
    let bytes = blocking(move || {
        let hash = state.db()?.photo(id)?.and_then(|photo| photo.blake3_hash);
        Ok(match hash {
            Some(hash) => std::fs::read(proxy_path(&webp_dir, &hash, size)).ok(),
            None => None,
        })
    })
    .await?;
    Ok(match bytes {
        Some(bytes) => (
            [(header::CONTENT_TYPE, "image/webp"), (header::CACHE_CONTROL, "public, max-age=86400")],
            Body::from(bytes),
        )
            .into_response(),
        None => (
            [(header::CONTENT_TYPE, "image/svg+xml")],
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 3"><rect width="4" height="3" fill="#e8e4dc"/></svg>"##,
        )
            .into_response(),
    })
}

/// A square crop around one face, from the 1200 px proxy (People page, detail drawer).
pub async fn face(State(state): State<AppState>, Path(face_id): Path<i64>) -> WebResult<Response> {
    let webp_dir = state.config().webp_dir();
    let jpeg = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let Some(face) = snapshot.faces.iter().find(|face| face.id == face_id) else { return Ok(None) };
        let Some(hash) = db.photo(face.photo_id)?.and_then(|photo| photo.blake3_hash) else { return Ok(None) };
        let image = image::open(proxy_path(&webp_dir, &hash, PREVIEW_SIZE))?;
        let scale = f64::from(image.width()) / face.photo_width.max(1) as f64;
        let [x1, y1, x2, y2] = face.bbox.map(|v| v * scale);
        let side = ((x2 - x1).max(y2 - y1) * 1.5).max(8.0);
        let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
        let left = (cx - side / 2.0).clamp(0.0, (f64::from(image.width()) - side).max(0.0));
        let top = (cy - side / 2.0).clamp(0.0, (f64::from(image.height()) - side).max(0.0));
        let crop = image
            .crop_imm(left as u32, top as u32, side as u32, side as u32)
            .resize_to_fill(160, 160, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&crop)?;
        Ok(Some(out))
    })
    .await?;
    Ok(match jpeg {
        Some(bytes) => {
            ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "public, max-age=600")], Body::from(bytes)).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    })
}
