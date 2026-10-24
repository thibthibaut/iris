//! Preflight: checks the print PDF as written, not the album it was made from.

use std::path::Path;

use anyhow::{Context, Result};
use lopdf::{Document, Object};
use serde::{Deserialize, Serialize};

const MM_PER_PT: f64 = 25.4 / 72.0;

/// A preflight problem, linked to its Spread when it has one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    pub spread: Option<u64>,
    pub message: String,
}

impl Problem {
    pub fn album(message: impl Into<String>) -> Self {
        Self { spread: None, message: message.into() }
    }
}

/// What the PDF itself says.
#[derive(Debug, Clone)]
pub struct PdfFacts {
    pub pages: usize,
    /// Page sizes in mm (width, height), from each page's MediaBox.
    pub sizes_mm: Vec<(f64, f64)>,
    /// Every font the PDF uses (PostScript names).
    pub fonts: Vec<String>,
    /// Fonts used without their font file.
    pub fonts_not_embedded: Vec<String>,
}

pub fn inspect(pdf: &Path) -> Result<PdfFacts> {
    let doc = Document::load(pdf).with_context(|| format!("failed to read {}", pdf.display()))?;
    let pages = doc.get_pages();
    let mut sizes_mm = Vec::new();
    for id in pages.values() {
        let page = doc.get_dictionary(*id)?;
        let mut node = page;
        // MediaBox may be inherited from the page tree.
        let media = loop {
            if let Ok(media) = node.get_deref(b"MediaBox", &doc) {
                break media.as_array()?.clone();
            }
            node = doc.get_dict_in_dict(node, b"Parent").context("page without MediaBox")?;
        };
        let n: Vec<f64> = media.iter().map(|v| v.as_float().map(f64::from)).collect::<Result<_, _>>()?;
        anyhow::ensure!(n.len() == 4, "bad MediaBox");
        sizes_mm.push(((n[2] - n[0]) * MM_PER_PT, (n[3] - n[1]) * MM_PER_PT));
    }
    let mut fonts = Vec::new();
    let mut fonts_not_embedded = Vec::new();
    for object in doc.objects.values() {
        let dict = match object {
            Object::Dictionary(dict) => dict,
            Object::Stream(stream) => &stream.dict,
            _ => continue,
        };
        let is_descriptor = dict.get(b"Type").and_then(Object::as_name).is_ok_and(|t| t == b"FontDescriptor");
        if !is_descriptor {
            continue;
        }
        let name = dict.get(b"FontName").and_then(Object::as_name).map_or_else(|_| "?".into(), |n| String::from_utf8_lossy(n).into_owned());
        if ![&b"FontFile"[..], b"FontFile2", b"FontFile3"].iter().any(|k| dict.has(k)) {
            fonts_not_embedded.push(name.clone());
        }
        fonts.push(name);
    }
    fonts.sort();
    fonts.dedup();
    Ok(PdfFacts { pages: pages.len(), sizes_mm, fonts, fonts_not_embedded })
}

/// Flexilivre: single pages with bleed, the cover as the first and last page, an even
/// interior of 24 to 400 pages.
pub fn check(facts: &PdfFacts, page_mm: (f64, f64)) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (i, &(w, h)) in facts.sizes_mm.iter().enumerate() {
        if (w - page_mm.0).abs() > 0.5 || (h - page_mm.1).abs() > 0.5 {
            problems.push(Problem::album(format!("page {}: {w:.1} × {h:.1} mm, expected {:.1} × {:.1} mm with bleed", i + 1, page_mm.0, page_mm.1)));
        }
    }
    let interior = facts.pages.saturating_sub(2);
    if interior % 2 != 0 {
        problems.push(Problem::album(format!("{interior} interior pages: Flexilivre needs an even count")));
    }
    if !(24..=400).contains(&interior) {
        problems.push(Problem::album(format!("{interior} interior pages: Flexilivre accepts 24 to 400")));
    }
    for font in &facts.fonts_not_embedded {
        problems.push(Problem::album(format!("font {font} is not embedded")));
    }
    problems
}

#[cfg(test)]
mod tests {
    use iris_core::model::{Crop, PAGE_H, PAGE_W, PlacedPhoto, Rect, Settings, Spread, SpreadKind};

    use super::*;
    use crate::{
        album::fonts_dir,
        render::{ImageMap, PrintJob, Renderer, Texts, TypstRenderer},
    };

    #[test]
    fn a_print_pdf_has_bleed_sized_pages_and_embeds_both_title_fonts() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let renderer = TypstRenderer {
            fonts: fonts_dir(),
            work_dir: dir.path().join("typst"),
            design: settings.design.clone(),
            texts: Texts::from(
                &settings,
                &[
                    (format!("chapter.{}.fr", settings.chapters[0].key), "Les préparatifs".to_string()),
                    (format!("chapter.{}.ar", settings.chapters[0].key), "التحضيرات".to_string()),
                ]
                .into(),
            ),
        };
        let spread = Spread {
            uid: 1,
            chapter: settings.chapters[0].key.clone(),
            kind: SpreadKind::Core,
            group: None,
            templates: ["hero".into(), String::new()],
            photos: vec![PlacedPhoto { photo: 1, rect: Rect { x: 0.0, y: 0.0, w: PAGE_W, h: PAGE_H }, crop: Crop::FULL }],
            title: true,
            locked: false,
            position: 0,
        };
        let job = PrintJob { spreads: vec![&spread], images: ImageMap::new(), cover_photo: None, bleed_mm: 3.0 };
        let pdf = dir.path().join("album.pdf");

        let renderer_warnings = renderer.render_print(&job, &pdf).unwrap();
        let facts = inspect(&pdf).unwrap();

        assert!(renderer_warnings.is_empty(), "{renderer_warnings:?}");
        // Cover, opening page, the Spread's two pages, closing page, back cover.
        assert_eq!(facts.pages, 6);
        assert!(facts.fonts_not_embedded.is_empty(), "{:?}", facts.fonts_not_embedded);
        for family in [&settings.design.title_font, &settings.design.arabic_font] {
            assert!(facts.fonts.iter().any(|f| f.contains(family.as_str())), "{family} missing from {:?}", facts.fonts);
        }
        let problems = check(&facts, (PAGE_W + 6.0, PAGE_H + 6.0));
        assert_eq!(problems, vec![Problem::album("4 interior pages: Flexilivre accepts 24 to 400")]);
    }
}
