use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use iris_core::{
    album::edition_spreads,
    decisions::Decisions,
    layout::{bleed_crop, face_crop},
    model::{Crop, PAGE_H, PAGE_W, PhotoId, Rect},
};
use libvips::{VipsImage, ops};

use crate::{
    album::{current, layout_shots, renderer, unplaced_pins, warnings},
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
    pub fn all(editions: &[&str]) -> Vec<Self> {
        editions
            .iter()
            .map(|edition| Self {
                edition: Box::leak(edition.to_string().into_boxed_str()),
                name: Box::leak(format!("export:{edition}").into_boxed_str()),
            })
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
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &["select"]
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn decisions_key(&self, decisions: &Decisions) -> String {
        format!("{:?}", decisions.locked.keys().collect::<Vec<_>>())
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        let state = AlbumState::load(&ctx.db)?;
        let settings = &state.decisions.settings;
        let bleed = bleed_for(&settings.printer)?;
        let edition = settings.edition(self.edition).context("unknown Edition")?.clone();
        let album = current(&ctx.db, &state)?.context("build the album first")?;
        let spreads = edition_spreads(&album, &edition);
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
            let cover = cover_photo(&state, &album, &spreads);
            let cover_path = match cover {
                Some(photo) => {
                    let record = state.photo(photo).context("cover photo missing")?;
                    let shots = layout_shots(&state);
                    let page = Rect { x: 0.0, y: 0.0, w: PAGE_W + 2.0 * bleed, h: PAGE_H + 2.0 * bleed };
                    let crop = shots.get(&photo).map_or(Crop::FULL, |s| face_crop(s.aspect, page.aspect(), &s.faces).0);
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
                        problems.push(Problem { spread: Some(spread.uid), message });
                    }
                }
            }
            for spread in &spreads {
                problems.extend(warnings(&state, spread).into_iter().map(|message| Problem { spread: Some(spread.uid), message }));
            }
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
                "overrides_revision": ctx.db.overrides_revision()?,
            });
            ctx.db.add_export(self.edition, version, &pdf.to_string_lossy(), &manifest)?;

            // Keep only the latest file per Edition (disk); older versions stay listed.
            for (_, edition, older, path, _, _) in ctx.db.exports()? {
                if edition == self.edition && older < version {
                    let _ = std::fs::remove_file(&path);
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

fn cover_photo(state: &AlbumState, album: &iris_core::model::Proposal, spreads: &[&iris_core::model::Spread]) -> Option<PhotoId> {
    if let Some(chosen) = state.decisions.texts.get("cover.photo").and_then(|v| v.trim().parse().ok()) {
        return Some(chosen);
    }
    let heroes: Vec<PhotoId> = spreads
        .iter()
        .filter(|s| s.templates.iter().any(|t| t == "hero"))
        .flat_map(|s| s.photos.iter().map(|p| p.photo))
        .collect();
    heroes.first().copied().or_else(|| album.spreads.iter().flat_map(|s| s.photos.iter()).map(|p| p.photo).next())
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
