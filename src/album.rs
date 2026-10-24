//! Glue between the stored analysis and the pure album core: candidates, layout inputs,
//! the current album (Proposal + Locks), and Spread previews.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use anyhow::Result;
use iris_core::{
    album::{AlbumInput, conflicts, spread_conflicts},
    decisions::Eligibility,
    edit::apply_locks,
    layout::LayoutShot,
    model::{Conflict, EditionDef, PAGE_W, PhotoId, Proposal, Spread},
    select::{Candidate, GroupPick},
};

use crate::{
    config::Config,
    db::Database,
    image_io::{PREVIEW_SIZE, proxy_path},
    render::{ImageMap, Renderer, Texts, TypstRenderer},
    state::AlbumState,
};

/// What layout needs about every Photo (aspect, faces to keep, print size).
pub fn layout_shots(state: &AlbumState) -> HashMap<PhotoId, LayoutShot> {
    let couple = state.couple_anchors();
    state
        .photos
        .iter()
        .filter_map(|photo| {
            let aspect = photo.aspect()?;
            let faces = state
                .counted_faces(photo.id)
                .filter(|face| {
                    couple.is_empty() || state.people.person_of_face(face.id).is_some_and(|p| couple.contains(&p.anchor)) || face.height_frac() > 0.08
                })
                .map(|face| face.normalized())
                .collect();
            Some((
                photo.id,
                LayoutShot {
                    photo: photo.id,
                    aspect,
                    quality: state.evaluations.get(&photo.id).map_or(0.5, |e| e.quality),
                    max_print_mm: photo.max_print_mm().unwrap_or(100.0),
                    hero: false,
                    faces,
                },
            ))
        })
        .collect()
}

/// One Candidate per Shot. A Pin or Ban on any Photo of a Shot applies to the Shot; a
/// pinned copy becomes the one used.
pub fn album_input(state: &AlbumState, embeddings: &HashMap<PhotoId, Vec<f32>>) -> AlbumInput {
    let settings = &state.decisions.settings;
    let couple = state.couple_anchors();
    let mut shots: HashMap<i64, Vec<&crate::db::PhotoRecord>> = HashMap::new();
    for photo in &state.photos {
        shots.entry(photo.shot_id.unwrap_or(photo.id)).or_default().push(photo);
    }
    let layout = layout_shots(state);
    let mut candidates = Vec::new();
    for (representative, members) in shots {
        let banned = members.iter().any(|p| state.decisions.bans.contains(&p.id));
        let pinned = members.iter().find(|p| state.decisions.pins.contains(&p.id)).copied();
        let Some(photo) = pinned.or_else(|| members.iter().find(|p| p.id == representative).copied()).or(members.first().copied()) else {
            continue;
        };
        let Some(chapter) = state.chapter_of(photo).and_then(|key| settings.chapter_index(&key)) else { continue };
        if !layout.contains_key(&photo.id) {
            continue;
        }
        let eligible = !banned && (pinned.is_some() || matches!(state.eligibility(photo.id), Eligibility::Eligible | Eligibility::Pinned));
        let couple_face = state
            .counted_faces(photo.id)
            .filter(|face| state.people.person_of_face(face.id).is_some_and(|p| couple.contains(&p.anchor)))
            .map(|face| face.height_frac())
            .fold(0.0, f64::max);
        candidates.push(Candidate {
            photo: photo.id,
            chapter,
            order: photo.chapter_order.unwrap_or(f64::MAX / 2.0),
            quality: state.evaluations.get(&photo.id).map_or(0.0, |e| e.quality),
            persons: state.persons_in(photo.id),
            couple_face,
            photographer: state.is_photographer(photo),
            group_portrait: photo.source_folder == "groupe" || state.counted_faces(photo.id).count() >= 5,
            embedding: embeddings.get(&photo.id).cloned().unwrap_or_default(),
            time_ms: photo.taken_at_utc,
            eligible,
            pinned: pinned.is_some(),
        });
    }
    candidates.sort_by_key(|c| c.photo);
    let size = settings.shots_per_spread.round().max(3.0) as usize;
    let groups = settings
        .groups
        .iter()
        .map(|group| GroupPick {
            key: group.key.clone(),
            members: state.people.persons.iter().filter(|p| p.groups.contains(&group.key)).map(|p| p.anchor).collect(),
            size,
        })
        .collect();
    let quotas = state.people.persons.iter().filter(|p| p.quota > 0).map(|p| (p.anchor, p.quota)).collect();
    AlbumInput { candidates, layout, groups, quotas, couple: couple.into_iter().collect::<HashSet<_>>() }
}

/// The album as the user sees it now.
pub fn current(db: &Database, state: &AlbumState) -> Result<Option<Proposal>> {
    let proposal: Option<Proposal> = db.blob("proposal")?;
    let order: Vec<String> = state.decisions.settings.chapters.iter().map(|c| c.key.clone()).collect();
    Ok(proposal.map(|proposal| apply_locks(&proposal, &state.decisions.locked, &order)))
}

/// Page numbers of each Spread in an Edition (page 1 is the opening page).
pub fn page_numbers<'a>(spreads: impl IntoIterator<Item = &'a Spread>) -> HashMap<u64, (usize, usize)> {
    spreads.into_iter().enumerate().map(|(i, spread)| (spread.uid, (2 + 2 * i, 3 + 2 * i))).collect()
}

pub fn renderer(config: &Config, state: &AlbumState) -> TypstRenderer {
    TypstRenderer {
        fonts: fonts_dir(),
        work_dir: config.temp_dir().join("typst"),
        design: state.decisions.settings.design.clone(),
        texts: Texts::from(&state.decisions.settings, &state.decisions.texts),
    }
}

pub fn fonts_dir() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    if here.exists() { here } else { PathBuf::from("assets/fonts") }
}

/// The 1200 px proxies of a Spread's Photos.
pub fn proxy_images(config: &Config, state: &AlbumState, spread: &Spread) -> ImageMap {
    spread
        .photos
        .iter()
        .filter_map(|placed| {
            let hash = state.photo(placed.photo)?.blake3_hash.as_ref()?;
            Some((placed.photo, proxy_path(&config.webp_dir(), hash, PREVIEW_SIZE)))
        })
        .collect()
}

/// Renders (or reuses) the preview PNG of a Spread; the file name is a hash of everything
/// it shows, so any change gets a new preview.
pub fn preview(config: &Config, state: &AlbumState, spread: &Spread, numbers: (usize, usize)) -> Result<PathBuf> {
    let renderer = renderer(config, state);
    let key = format!(
        "{}|{:?}|{:?}|{:?}|{numbers:?}",
        serde_json::to_string(spread)?,
        renderer.design,
        renderer.texts.chapter_titles.get(&spread.chapter),
        spread.group.as_ref().and_then(|g| renderer.texts.group_titles.get(g)),
    );
    let path = config.preview_dir().join(format!("{}.png", &blake3::hash(key.as_bytes()).to_hex()[..24]));
    if !path.exists() {
        std::fs::create_dir_all(config.preview_dir())?;
        renderer.render_preview(spread, &proxy_images(config, state, spread), numbers, &path)?;
    }
    Ok(path)
}

pub fn edition<'a>(state: &'a AlbumState, key: &str) -> Option<&'a EditionDef> {
    state.decisions.settings.edition(key)
}

/// What could go wrong in print: low resolution, cut faces, faces in the safe zone.
/// Pinned Photos the album has no room for: (Photo, file name).
pub fn unplaced_pins(state: &AlbumState, album: &Proposal) -> Vec<(PhotoId, String)> {
    conflicts(album, &state.decisions)
        .into_iter()
        .filter_map(|conflict| match conflict {
            Conflict::PinNotPlaced { photo } => Some((photo, state.photo(photo).map_or_else(|| format!("Photo {photo}"), |p| p.file_name.clone()))),
            Conflict::BannedOnLockedSpread { .. } => None,
        })
        .collect()
}

pub fn warnings(state: &AlbumState, spread: &Spread) -> Vec<String> {
    let mut out = Vec::new();
    for conflict in spread_conflicts(spread, &state.decisions) {
        if let Conflict::BannedOnLockedSpread { photo, .. } = conflict {
            let name = state.photo(photo).map_or_else(|| format!("Photo {photo}"), |p| p.file_name.clone());
            out.push(format!("{name}: Banned, but this Spread is Locked — remove it here or unlock the Spread"));
        }
    }
    for placed in &spread.photos {
        let Some(photo) = state.photo(placed.photo) else { continue };
        if let Some(review) = state.reviews.get(&photo.id) {
            let flags: Vec<&str> = ["eyes_closed", "mid_blink_or_mid_speech", "unflattering", "heads_cut", "motion_blur"]
                .into_iter()
                .filter(|flag| review[*flag].as_bool() == Some(true))
                .collect();
            if !flags.is_empty() {
                out.push(format!("{}: Mistral flags {} — {}", photo.file_name, flags.join(", ").replace('_', " "), review["note"].as_str().unwrap_or("")));
            }
        }
        let (Some(width), Some(height)) = (photo.width, photo.height) else { continue };
        let ppi = (width as f64 * placed.crop.w) / (placed.rect.w / 25.4);
        // Slots are sized to exactly 250 ppi; allow for rounding.
        if ppi < 249.5 {
            out.push(format!("{}: {:.0} ppi (needs 250)", photo.file_name, ppi));
        }
        let _ = height;
        for face in state.counted_faces(photo.id) {
            let [x1, y1, x2, y2] = face.normalized();
            let c = placed.crop;
            let inside = x1 >= c.x - 1e-6 && x2 <= c.x + c.w + 1e-6 && y1 >= c.y - 1e-6 && y2 <= c.y + c.h + 1e-6;
            if !inside && face.height_frac() > 0.06 {
                out.push(format!("{}: a face is cut", photo.file_name));
                break;
            }
            // Face position on the page, in mm, against the 5 mm safe zone.
            let fx1 = placed.rect.x + (x1 - c.x) / c.w * placed.rect.w;
            let fx2 = placed.rect.x + (x2 - c.x) / c.w * placed.rect.w;
            let fy1 = placed.rect.y + (y1 - c.y) / c.h * placed.rect.h;
            let fy2 = placed.rect.y + (y2 - c.y) / c.h * placed.rect.h;
            let page_x = if placed.rect.x >= PAGE_W { PAGE_W } else { 0.0 };
            let near = fx1 < page_x + 5.0 || fx2 > page_x + PAGE_W - 5.0 || fy1 < 5.0 || fy2 > 210.0 - 5.0;
            if inside && near && face.height_frac() > 0.06 {
                out.push(format!("{}: a face is in the 5 mm safe zone", photo.file_name));
                break;
            }
        }
    }
    out
}
