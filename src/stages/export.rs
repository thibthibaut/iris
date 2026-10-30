use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use iris_core::{
    album::edition_spreads,
    decisions::Decisions,
    layout::{bleed_crop, face_crop},
    model::{Crop, Design, EditionDef, EditionKey, PAGE_H, PAGE_W, PhotoId, Rect, Spread},
};
use libvips::{VipsImage, ops};
use serde::Serialize;

use crate::{
    album::{current, layout_shots, override_conflict_notices, renderer, unplaced_pins, warnings},
    image_io::vips_error,
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
    preflight::{self, Problem},
    render::{ImageMap, PrintJob, Renderer, Texts},
    state::AlbumState,
};

const PRINT_PPI: f64 = 300.0;

/// Renders one Edition as a numbered print PDF, with its preflight report.
pub struct Export {
    pub edition: &'static str,
    pub name: &'static str,
}

impl Export {
    pub fn for_edition(edition: &str) -> Self {
        Self {
            edition: Box::leak(edition.to_string().into_boxed_str()),
            name: Box::leak(format!("export:{edition}").into_boxed_str()),
        }
    }

    pub fn all(editions: &[&str]) -> Vec<Self> {
        editions
            .iter()
            .map(|edition| Self::for_edition(edition))
            .collect()
    }
}

/// Printer profiles. Only Flexilivre (single pages, cover = first and last page) is
/// implemented; the others fail clearly.
fn bleed_for(printer: &str) -> Result<f64> {
    match printer {
        "flexilivre_a4_landscape_hard" => Ok(3.0),
        other => anyhow::bail!("printer profile {other} is not implemented yet; choose Flexilivre in Settings"),
    }
}

impl Stage for Export {
    fn name(&self) -> &'static str {
        self.name
    }

    fn title(&self) -> &'static str {
        "Render print PDF"
    }

    fn description(&self) -> &'static str {
        "Crops every original at 300 ppi, renders the print PDF with bleed and checks it."
    }

    fn version(&self) -> i64 {
        2
    }

    fn deps(&self) -> &'static [&'static str] {
        &["select"]
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn staleness_key(&self, db: &crate::db::Database, _decisions: &Decisions) -> Result<String> {
        album_state_hash(db, &EditionKey::from(self.edition), &AlbumState::load(db)?)
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        let state = AlbumState::load(&ctx.db)?;
        let settings = &state.decisions.settings;
        let bleed = bleed_for(&settings.printer)?;
        let edition = settings.edition(&EditionKey::from(self.edition)).context("unknown Edition")?.clone();
        let album = current(&ctx.db, &state)?.context("build the album first")?;
        let spreads = edition_spreads(&album, &edition);
        let inputs = export_inputs(&ctx.db, &state, &edition, &album, &spreads, bleed)?;
        let temp = ctx.config.temp_dir().join(format!("print-{}", self.edition));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp)?;
        let result = (|| -> Result<()> {
            let total: usize = spreads.iter().map(|s| s.photos.len()).sum::<usize>() + 1;
            sink.start(total);
            sink.message("cropping the originals at 300 ppi");

            // Print crops: each Slot gets exactly its pixels; bleed-edge Slots grow into the bleed.
            let mut images = ImageMap::new();
            let mut print_spreads = Vec::new();
            for spread in &spreads {
                let mut print = (*spread).clone();
                for placed in &mut print.photos {
                    cancel.check()?;
                    let rect = with_bleed(placed.rect, bleed);
                    let photo = state.photo(placed.photo).context("photo missing")?;
                    let out = temp.join(format!("{}-{}.jpg", spread.uid, placed.photo));
                    let crop = bleed_crop(placed.crop, placed.rect, rect);
                    crop_original(Path::new(&photo.path), crop, rect, &out)
                        .with_context(|| format!("failed to crop {}", photo.file_name))?;
                    images.insert(placed.photo, out);
                    placed.rect = rect;
                    placed.crop = Crop::FULL;
                    sink.inc(1);
                }
                print_spreads.push(print);
            }

            // Cover: the chosen photo, or the first Hero, or the best Couple photo.
            let cover_path = match inputs.cover_photo {
                Some(photo) => {
                    let record = state.photo(photo).context("cover photo missing")?;
                    let page = Rect { x: 0.0, y: 0.0, w: PAGE_W + 2.0 * bleed, h: PAGE_H + 2.0 * bleed };
                    let crop = inputs.cover_crop.unwrap_or(Crop::FULL);
                    let out = temp.join("cover.jpg");
                    crop_original(Path::new(&record.path), crop, page, &out)?;
                    Some(out)
                }
                None => None,
            };
            sink.inc(1);

            sink.message("rendering the PDF");
            let version = ctx.db.next_export_version(self.edition)?;
            let dir = ctx.config.exports_dir.join(self.edition);
            std::fs::create_dir_all(&dir)?;
            let date = chrono::Local::now().format("%Y-%m-%d");
            let pdf = dir.join(format!("v{version}_{date}.pdf"));
            let job = PrintJob { spreads: print_spreads.iter().collect(), images, cover_photo: cover_path, bleed_mm: bleed };
            let typst_warnings = renderer(&ctx.config, &state).render_print(&job, &pdf)?;

            // Preflight: the PDF as written, then each Spread's warnings (ppi, faces, conflicts).
            let page_mm = (PAGE_W + 2.0 * bleed, PAGE_H + 2.0 * bleed);
            let facts = preflight::inspect(&pdf)?;
            let interior = facts.pages.saturating_sub(2);
            let mut problems = preflight::check(&facts, page_mm);
            problems.extend(typst_warnings.into_iter().map(|w| Problem::album(format!("renderer: {w}"))));
            // Titles always come in French and Tunisian Arabic.
            let texts = Texts::from(settings, &state.decisions.texts);
            for spread in spreads.iter().filter(|s| s.title) {
                let (fr, ar) = match &spread.group {
                    Some(group) => texts.group_titles.get(group),
                    None => texts.chapter_titles.get(&spread.chapter),
                }
                .cloned()
                .unwrap_or_default();
                for (text, language) in [(fr, "French"), (ar, "Tunisian Arabic")] {
                    if text.trim().is_empty() {
                        let message = format!("title in {language} missing (Text & design)");
                        problems.push(Problem { spread: Some(spread.uid), href: None, message });
                    }
                }
            }
            for spread in &spreads {
                problems.extend(warnings(&state, spread).into_iter().map(|message| Problem { spread: Some(spread.uid), href: None, message }));
            }
            problems.extend(override_conflict_notices(&state).into_iter().map(|notice| match notice.href {
                Some(href) => Problem::linked(href, notice.message),
                None => Problem::album(notice.message),
            }));
            problems.extend(
                unplaced_pins(&state, &album).into_iter().map(|(_, name)| Problem::album(format!("{name}: Pinned but not in the album"))),
            );
            let manifest = serde_json::json!({
                "edition": self.edition,
                "printer": settings.printer,
                "bleed_mm": bleed,
                "interior_pages": interior,
                "pdf_pages": facts.pages,
                "page_size_mm": [page_mm.0, page_mm.1],
                "spreads": spreads.iter().map(|s| s.uid.to_string()).collect::<Vec<_>>(),
                "photos": spreads.iter().map(|s| s.photos.len()).sum::<usize>(),
                "problems": problems,
                "fonts": facts.fonts,
                "fonts_embedded": facts.fonts_not_embedded.is_empty(),
                "album_state_hash": inputs.album_state_hash,
                "keep_version": false,
                "overrides_revision": ctx.db.overrides_revision()?,
            });
            ctx.db.add_export(self.edition, version, &pdf.to_string_lossy(), &manifest)?;

            // The latest and explicitly kept versions stay on disk and in history.
            for path in ctx.db.prune_exports(self.edition, version)? {
                if let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(path, %error, "failed to remove pruned Export PDF");
                }
            }
            sink.message(&format!("v{version}: {interior} pages, {} warnings", manifest["problems"].as_array().map_or(0, Vec::len)));
            Ok(())
        })();
        // Print crops are big: never keep them.
        let _ = std::fs::remove_dir_all(&temp);
        result
    }
}

pub(crate) fn album_state_hash(db: &crate::db::Database, edition_key: &EditionKey, state: &AlbumState) -> Result<String> {
    let Some(album) = current(db, state)? else {
        return Ok(blake3::hash(b"iris-export-no-album-v2").to_hex().to_string());
    };
    let settings = &state.decisions.settings;
    let edition = settings.edition(edition_key).context("unknown Edition")?;
    // Unsupported printer profiles still get a stable key so the GUI can show the
    // stage and let the user fix the setting; run() reports the actionable error.
    let bleed = bleed_for(&settings.printer).unwrap_or(0.0);
    let spreads = edition_spreads(&album, edition);
    Ok(export_inputs(db, state, edition, &album, &spreads, bleed)?.album_state_hash)
}

/// Slots touching the page edge extend into the bleed.
fn with_bleed(rect: Rect, bleed: f64) -> Rect {
    let page_x = if rect.x >= PAGE_W - 0.01 { PAGE_W } else { 0.0 };
    let mut out = rect;
    if (rect.x - page_x).abs() < 0.5 {
        out.x -= bleed;
        out.w += bleed;
    }
    if (rect.x + rect.w - (page_x + PAGE_W)).abs() < 0.5 {
        out.w += bleed;
    }
    if rect.y.abs() < 0.5 {
        out.y -= bleed;
        out.h += bleed;
    }
    if (rect.y + rect.h - PAGE_H).abs() < 0.5 {
        out.h += bleed;
    }
    out
}

fn cover_photo(state: &AlbumState, album: &iris_core::model::Proposal, spreads: &[&Spread]) -> Option<PhotoId> {
    if let Some(chosen) = state.decisions.texts.get("cover.photo").and_then(|v| v.trim().parse().ok()) {
        return Some(chosen);
    }
    let heroes: Vec<PhotoId> = spreads
        .iter()
        .filter(|s| s.templates.iter().any(|t| t.as_str() == "hero"))
        .flat_map(|s| s.photos.iter().map(|p| p.photo))
        .collect();
    heroes.first().copied().or_else(|| album.spreads.iter().flat_map(|s| s.photos.iter()).map(|p| p.photo).next())
}

#[derive(Debug, Clone, Serialize)]
struct ExportTexts {
    chapter_titles: BTreeMap<String, (String, String)>,
    group_titles: BTreeMap<String, (String, String)>,
    opening: String,
    opening_ar: String,
    closing: String,
    closing_ar: String,
    cover_title: String,
    cover_date: String,
    cover_date_ar: String,
}

#[derive(Debug, Clone, Serialize)]
struct ExportPhotoInput {
    id: PhotoId,
    path: String,
    file_name: String,
    file_size: i64,
    blake3_hash: Option<String>,
}

/// Stable, serializable inputs that affect the PDF or its manifest diagnostics.
#[derive(Debug, Clone, Serialize)]
struct ExportFingerprint {
    schema: u32,
    edition: EditionDef,
    printer: String,
    bleed_mm: f64,
    design: Design,
    spreads: Vec<Spread>,
    texts: ExportTexts,
    cover_photo: Option<PhotoId>,
    cover_crop: Option<Crop>,
    photos: Vec<ExportPhotoInput>,
    bans: Vec<PhotoId>,
    pins: Vec<PhotoId>,
    spread_warnings: Vec<(u64, Vec<String>)>,
    unplaced_pins: Vec<(PhotoId, String)>,
    overrides_revision: String,
}

struct ExportInputs {
    album_state_hash: String,
    cover_photo: Option<PhotoId>,
    cover_crop: Option<Crop>,
}

fn export_inputs(
    db: &crate::db::Database,
    state: &AlbumState,
    edition: &EditionDef,
    album: &iris_core::model::Proposal,
    spreads: &[&Spread],
    bleed: f64,
) -> Result<ExportInputs> {
    let settings = &state.decisions.settings;
    let cover_photo = cover_photo(state, album, spreads);
    let cover_crop = cover_photo.map(|photo| {
        let shots = layout_shots(state);
        let page = Rect { x: 0.0, y: 0.0, w: PAGE_W + 2.0 * bleed, h: PAGE_H + 2.0 * bleed };
        shots.get(&photo).map_or(Crop::FULL, |shot| face_crop(shot.aspect, page.aspect(), &shot.faces).0)
    });
    let unplaced_pins = {
        let mut pins = unplaced_pins(state, album);
        pins.sort_by_key(|(photo, _)| *photo);
        pins
    };
    let mut photo_ids: BTreeSet<PhotoId> = spreads.iter().flat_map(|spread| spread.photos.iter().map(|photo| photo.photo)).collect();
    if let Some(photo) = cover_photo {
        photo_ids.insert(photo);
    }
    photo_ids.extend(unplaced_pins.iter().map(|(photo, _)| *photo));
    let photos = photo_ids
        .into_iter()
        .filter_map(|id| {
            state.photo(id).map(|photo| ExportPhotoInput {
                id,
                path: photo.path.clone(),
                file_name: photo.file_name.clone(),
                file_size: photo.file_size,
                blake3_hash: photo.blake3_hash.clone(),
            })
        })
        .collect();
    let texts = Texts::from(settings, &state.decisions.texts);
    let fingerprint = ExportFingerprint {
        schema: 1,
        edition: edition.clone(),
        printer: settings.printer.clone(),
        bleed_mm: bleed,
        design: settings.design.clone(),
        spreads: spreads.iter().map(|spread| (*spread).clone()).collect(),
        texts: ExportTexts {
            chapter_titles: texts.chapter_titles.into_iter().map(|(key, value)| (key.to_string(), value)).collect(),
            group_titles: texts.group_titles.into_iter().map(|(key, value)| (key.to_string(), value)).collect(),
            opening: texts.opening,
            opening_ar: texts.opening_ar,
            closing: texts.closing,
            closing_ar: texts.closing_ar,
            cover_title: texts.cover_title,
            cover_date: texts.cover_date,
            cover_date_ar: texts.cover_date_ar,
        },
        cover_photo,
        cover_crop,
        photos,
        bans: sorted_photo_ids(state.decisions.bans.iter().copied()),
        pins: sorted_photo_ids(state.decisions.pins.iter().copied()),
        spread_warnings: spreads.iter().map(|spread| (spread.uid, warnings(state, spread))).collect(),
        unplaced_pins,
        overrides_revision: db.overrides_revision()?,
    };
    let album_state_hash = fingerprint_hash(&fingerprint)?;
    Ok(ExportInputs {
        album_state_hash,
        cover_photo,
        cover_crop,
    })
}

fn fingerprint_hash(fingerprint: &ExportFingerprint) -> Result<String> {
    let bytes = serde_json::to_vec(fingerprint)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn sorted_photo_ids(ids: impl Iterator<Item = PhotoId>) -> Vec<PhotoId> {
    let mut ids: Vec<PhotoId> = ids.collect();
    ids.sort_unstable();
    ids
}

/// Crops an original (upright) to `crop` and resizes it to the Slot at 300 ppi, sRGB JPEG.
fn crop_original(path: &Path, crop: Crop, rect: Rect, out: &PathBuf) -> Result<()> {
    let _ = crate::image_io::vips();
    let image = VipsImage::new_from_file(&path.to_string_lossy()).map_err(|e| vips_error(&e))?;
    let image = ops::autorot(&image).map_err(|e| vips_error(&e))?;
    let image = ops::colourspace(&image, ops::Interpretation::Srgb).map_err(|e| vips_error(&e))?;
    let (w, h) = (f64::from(image.get_width()), f64::from(image.get_height()));
    let left = (crop.x * w).round().clamp(0.0, w - 1.0);
    let top = (crop.y * h).round().clamp(0.0, h - 1.0);
    let width = (crop.w * w).round().clamp(1.0, w - left);
    let height = (crop.h * h).round().clamp(1.0, h - top);
    let cropped = ops::extract_area(&image, left as i32, top as i32, width as i32, height as i32).map_err(|e| vips_error(&e))?;
    let target_w = (rect.w / 25.4 * PRINT_PPI).round();
    let target_h = (rect.h / 25.4 * PRINT_PPI).round();
    let resized = ops::resize_with_opts(
        &cropped,
        target_w / width,
        &ops::ResizeOptions { vscale: target_h / height, kernel: ops::Kernel::Lanczos3, ..Default::default() },
    )
    .map_err(|e| vips_error(&e))?;
    let sharpened = ops::sharpen_with_opts(&resized, &ops::SharpenOptions { sigma: 0.6, ..Default::default() }).map_err(|e| vips_error(&e))?;
    let options = ops::JpegsaveOptions { q: 95, keep: ops::ForeignKeep::Icc, ..Default::default() };
    ops::jpegsave_with_opts(&sharpened, &out.to_string_lossy(), &options).map_err(|e| vips_error(&e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::model::{PlacedPhoto, Settings, SpreadKind};

    fn fingerprint() -> ExportFingerprint {
        let settings = Settings::default();
        let edition = settings.edition(&EditionKey::from("ours")).unwrap().clone();
        ExportFingerprint {
            schema: 1,
            edition,
            printer: settings.printer,
            bleed_mm: 3.0,
            design: settings.design,
            spreads: vec![Spread {
                uid: 7,
                chapter: "prep_bride".into(),
                kind: SpreadKind::Core,
                group: None,
                templates: ["hero".into(), String::new()],
                photos: vec![PlacedPhoto {
                    photo: 1,
                    rect: Rect { x: 0.0, y: 0.0, w: PAGE_W, h: PAGE_H },
                    crop: Crop::FULL,
                }],
                title: true,
                locked: false,
                position: 0,
            }],
            texts: ExportTexts {
                chapter_titles: BTreeMap::from([("prep_bride".into(), ("Préparatifs".into(), "تحضيرات".into()))]),
                group_titles: BTreeMap::new(),
                opening: "Opening".into(),
                opening_ar: "افتتاح".into(),
                closing: "Closing".into(),
                closing_ar: "ختام".into(),
                cover_title: "Emna & Thibaut".into(),
                cover_date: "19 juillet 2025".into(),
                cover_date_ar: "19 جويلية 2025".into(),
            },
            cover_photo: Some(1),
            cover_crop: Some(Crop::FULL),
            photos: vec![ExportPhotoInput {
                id: 1,
                path: "/photos/one.jpg".into(),
                file_name: "one.jpg".into(),
                file_size: 1024,
                blake3_hash: Some("photo-content-hash".into()),
            }],
            bans: vec![2],
            pins: vec![],
            spread_warnings: vec![],
            unplaced_pins: vec![],
            overrides_revision: "4:5:0".into(),
        }
    }

    #[test]
    fn export_fingerprint_is_stable_and_tracks_print_inputs() {
        let baseline = fingerprint();
        let hash = fingerprint_hash(&baseline).unwrap();
        assert_eq!(hash, fingerprint_hash(&baseline.clone()).unwrap());

        let mut changed = baseline.clone();
        changed.spreads[0].locked = true;
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "Locks affect export freshness");

        let mut changed = baseline.clone();
        changed.texts.chapter_titles.get_mut("prep_bride").unwrap().0 = "New title".into();
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "text affects export freshness");

        let mut changed = baseline.clone();
        changed.design.emerald = "#123456".into();
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "design affects export freshness");

        let mut changed = baseline.clone();
        changed.printer = "another-printer".into();
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "printer affects export freshness");

        let mut changed = baseline.clone();
        changed.edition.group = Some("bride_family".into());
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "Edition contents affect export freshness");

        let mut changed = baseline.clone();
        changed.bans.push(9);
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "Bans affect export freshness");

        let mut changed = baseline.clone();
        changed.cover_photo = Some(3);
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "cover selection affects export freshness");

        let mut changed = baseline;
        changed.photos[0].blake3_hash = Some("changed-photo-content-hash".into());
        assert_ne!(hash, fingerprint_hash(&changed).unwrap(), "source image changes affect export freshness");
    }

    #[test]
    fn decision_photo_ids_are_sorted_before_hashing() {
        assert_eq!(sorted_photo_ids([9, 1, 4].into_iter()), sorted_photo_ids([4, 9, 1].into_iter()));
    }
}
