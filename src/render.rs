//! Rendering behind a trait: Spread previews (PNG) and print PDFs. The layout is ours;
//! Typst only places images at absolute positions and typesets the text.
//! For now the Typst CLI is driven with generated `.typ` files (the plan's fallback).

use std::{
    collections::HashMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use iris_core::{
    layout::{MARGIN, TITLE_BAND},
    model::{Design, PAGE_H, PAGE_W, PhotoId, PlacedPhoto, Settings, Spread, SpreadKind},
};

/// Page numbers sit this far above the trim: inside the bottom margin, outside the safe zone.
const FOLIO_FROM_BOTTOM: f64 = 8.0;

/// Text shown on Spreads and on the opening, closing and cover pages.
#[derive(Debug, Clone, Default)]
pub struct Texts {
    pub chapter_titles: HashMap<String, (String, String)>,
    pub group_titles: HashMap<String, (String, String)>,
    pub opening: String,
    pub opening_ar: String,
    pub closing: String,
    pub closing_ar: String,
    pub cover_title: String,
    pub cover_date: String,
    pub cover_date_ar: String,
}

impl Texts {
    pub fn from(settings: &Settings, texts: &std::collections::BTreeMap<String, String>) -> Self {
        let get = |key: &str, default: &str| texts.get(key).cloned().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| default.to_string());
        Self {
            chapter_titles: settings
                .chapters
                .iter()
                .map(|c| {
                    let fr = get(&format!("chapter.{}.fr", c.key), &c.title_fr);
                    let ar = get(&format!("chapter.{}.ar", c.key), &c.title_ar);
                    (c.key.clone(), (fr, ar))
                })
                .collect(),
            group_titles: settings
                .groups
                .iter()
                .map(|g| (g.key.clone(), (get(&format!("group.{}.fr", g.key), &g.name), get(&format!("group.{}.ar", g.key), &g.title_ar))))
                .collect(),
            opening: get("opening.fr", &format!("{}\n{}", settings.design.names, settings.design.cover_date_fr)),
            opening_ar: get("opening.ar", "آمنة و تيبو"),
            closing: get("closing.fr", "Merci d'avoir été là."),
            closing_ar: get("closing.ar", "يعيشكم اللي كنتو معانا"),
            cover_title: get("cover.title", &settings.design.names),
            cover_date: get("cover.date", &settings.design.cover_date_fr),
            cover_date_ar: get("cover.date_ar", &settings.design.cover_date_ar),
        }
    }
}

/// Where each Photo's image comes from: proxies for previews, print crops for PDFs.
pub type ImageMap = HashMap<PhotoId, PathBuf>;

pub trait Renderer: Send + Sync {
    /// A PNG of one Spread (both pages side by side), at screen resolution.
    fn render_preview(&self, spread: &Spread, images: &ImageMap, page_numbers: (usize, usize), out: &Path) -> Result<()>;
    /// The print PDF: cover front, opening page, every Spread as two pages, closing page,
    /// cover back. Images must already be cropped to their Slots.
    /// Returns the renderer's warnings (an unknown font falls back silently otherwise).
    fn render_print(&self, job: &PrintJob, out: &Path) -> Result<Vec<String>>;
}

pub struct PrintJob<'a> {
    pub spreads: Vec<&'a Spread>,
    pub images: ImageMap,
    pub cover_photo: Option<PathBuf>,
    pub bleed_mm: f64,
}

pub struct TypstRenderer {
    pub fonts: PathBuf,
    pub work_dir: PathBuf,
    pub design: Design,
    pub texts: Texts,
}

fn color(hex: &str) -> String {
    format!("rgb(\"{}\")", hex.trim())
}

fn escape(text: &str) -> String {
    // Typst markup inside a string literal.
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

impl TypstRenderer {
    fn preamble(&self, width: f64, height: f64) -> String {
        let d = &self.design;
        format!(
            r#"#set page(width: {width}mm, height: {height}mm, margin: 0mm, fill: {paper})
#set text(font: ("{body}", "{arabic}"), fill: {ink}, size: 11pt, lang: "fr")
#let emerald = {emerald}
#let red = {red}
#let yellow = {yellow}
#let pink = {pink}
#let blue = {blue}
#let ink = {ink}
#let rule(x, y, w) = {{
  place(dx: x, dy: y, line(length: w, stroke: 0.35pt + ink.transparentize(75%)))
  place(dx: x, dy: y - 0.35mm, rect(width: 12mm, height: 0.7mm, fill: emerald))
}}
#let folio(x, y, n, side, on-photo) = place(dx: if side == left {{ x }} else {{ x - 40mm }}, dy: y, box(width: 40mm, align(side, text(font: "{title}", size: 8.5pt, tracking: 0.12em, fill: if on-photo {{ white }} else {{ ink.transparentize(25%) }})[#n])))
#let photo(path, x, y, w, h, cx, cy, cw, ch) = place(dx: x, dy: y,
  box(width: w, height: h, clip: true, place(dx: -w * cx / cw, dy: -h * cy / ch,
    image(path, width: w / cw, height: h / ch, fit: "stretch"))))
"#,
            paper = color(&d.paper),
            ink = color(&d.ink),
            emerald = color(&d.emerald),
            red = color(&d.red_orange),
            yellow = color(&d.yellow),
            pink = color(&d.pink),
            blue = color(&d.blue),
            body = d.body_font,
            arabic = d.arabic_font,
            title = d.title_font,
        )
    }

    /// The French and Arabic titles at the same size, side by side over a hairline.
    fn title_block(&self, x: f64, fr: &str, ar: &str) -> String {
        let d = &self.design;
        let w = PAGE_W - 2.0 * MARGIN;
        let mut out = String::new();
        let _ = writeln!(
            out,
            r#"#place(dx: {x}mm + {MARGIN}mm, dy: {MARGIN}mm, box(width: {w}mm, text(font: "{font}", size: 22pt, fill: ink, "{fr}")))"#,
            font = d.title_font,
            fr = escape(fr),
        );
        if !ar.trim().is_empty() {
            let _ = writeln!(
                out,
                r#"#place(dx: {x}mm + {MARGIN}mm, dy: {MARGIN}mm - 1.5mm, box(width: {w}mm, align(right, text(font: "{font}", size: 22pt, dir: rtl, fill: ink, "{ar}"))))"#,
                font = d.arabic_font,
                ar = escape(ar),
            );
        }
        let _ = writeln!(out, "#rule({x}mm + {MARGIN}mm, {}mm, {w}mm)", MARGIN + TITLE_BAND - 5.0);
        out
    }

    fn spread_body(&self, spread: &Spread, images: &ImageMap, x0: f64, y0: f64, numbers: (usize, usize)) -> String {
        let mut out = String::new();
        if spread.title {
            let (fr, ar) = match (spread.kind, &spread.group) {
                (SpreadKind::Group, Some(group)) => self.texts.group_titles.get(group).cloned().unwrap_or_default(),
                _ => self.texts.chapter_titles.get(&spread.chapter).cloned().unwrap_or_default(),
            };
            out.push_str(&self.title_block(x0, &fr, &ar));
        }
        for placed in &spread.photos {
            out.push_str(&photo_call(placed, images, x0, y0));
        }
        // Page numbers in the outer corners of the bottom margin, white over a full-bleed photo.
        let (left, right) = numbers;
        let y = PAGE_H - FOLIO_FROM_BOTTOM;
        for (n, x, side) in [(left, MARGIN, "left"), (right, 2.0 * PAGE_W - MARGIN, "right")] {
            if n > 0 {
                let on_photo = spread.photos.iter().any(|p| p.rect.x <= x && x <= p.rect.x + p.rect.w && p.rect.y <= y && y <= p.rect.y + p.rect.h);
                let _ = writeln!(out, "#folio({}mm, {}mm, {n}, {side}, {on_photo})", x0 + x, y0 + y);
            }
        }
        out
    }

    fn compile(&self, source: &str, out: &Path, ppi: Option<u32>) -> Result<Vec<String>> {
        std::fs::create_dir_all(&self.work_dir)?;
        let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("render");
        let typ = self.work_dir.join(format!("{stem}.typ"));
        std::fs::write(&typ, source)?;
        let mut command = Command::new(typst_binary());
        command.arg("compile").arg("--root").arg("/").arg("--font-path").arg(&self.fonts);
        if let Some(ppi) = ppi {
            command.arg("--ppi").arg(ppi.to_string());
        }
        let output = command.arg(&typ).arg(out).output().context("failed to run typst (is it installed?)")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(output.status.success(), "typst failed: {stderr}");
        Ok(stderr.lines().filter_map(|line| line.strip_prefix("warning: ")).map(str::to_string).collect())
    }
}

/// `typst` from PATH, or Homebrew's.
fn typst_binary() -> PathBuf {
    for candidate in ["/opt/homebrew/bin/typst", "/usr/local/bin/typst"] {
        if Path::new(candidate).exists() && which("typst").is_none() {
            return PathBuf::from(candidate);
        }
    }
    PathBuf::from("typst")
}

fn which(binary: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').map(|dir| Path::new(dir).join(binary)).find(|p| p.exists())
}

fn photo_call(placed: &PlacedPhoto, images: &ImageMap, x0: f64, y0: f64) -> String {
    let Some(path) = images.get(&placed.photo) else {
        return format!(
            "#place(dx: {}mm, dy: {}mm, rect(width: {}mm, height: {}mm, fill: luma(225)))\n",
            x0 + placed.rect.x,
            y0 + placed.rect.y,
            placed.rect.w,
            placed.rect.h
        );
    };
    let c = placed.crop;
    format!(
        "#photo(\"{}\", {}mm, {}mm, {}mm, {}mm, {}, {}, {}, {})\n",
        escape(&path.to_string_lossy()),
        x0 + placed.rect.x,
        y0 + placed.rect.y,
        placed.rect.w,
        placed.rect.h,
        c.x,
        c.y,
        c.w.max(1e-3),
        c.h.max(1e-3)
    )
}

impl Renderer for TypstRenderer {
    fn render_preview(&self, spread: &Spread, images: &ImageMap, page_numbers: (usize, usize), out: &Path) -> Result<()> {
        let mut source = self.preamble(2.0 * PAGE_W, PAGE_H);
        // The fold, so the two pages read as a book.
        let _ = writeln!(source, "#place(dx: {}mm, dy: 0mm, line(length: {}mm, angle: 90deg, stroke: 0.4pt + luma(215)))", PAGE_W, PAGE_H);
        source.push_str(&self.spread_body(spread, images, 0.0, 0.0, page_numbers));
        self.compile(&source, out, Some(72)).map(drop)
    }

    fn render_print(&self, job: &PrintJob, out: &Path) -> Result<Vec<String>> {
        let b = job.bleed_mm;
        let (w, h) = (PAGE_W + 2.0 * b, PAGE_H + 2.0 * b);
        let mut source = self.preamble(w, h);
        let d = &self.design;
        let texts = &self.texts;

        // Cover front.
        if let Some(cover) = &job.cover_photo {
            let _ = writeln!(source, "#place(dx: 0mm, dy: 0mm, image(\"{}\", width: {w}mm, height: {h}mm, fit: \"cover\"))", escape(&cover.to_string_lossy()));
        }
        let _ = writeln!(
            source,
            r#"#place(dx: {x}mm, dy: {y}mm, box(width: {bw}mm, fill: {paper}.transparentize(10%), inset: 6mm, align(center, stack(spacing: 3mm, text(font: "{title}", size: 34pt, "{t}"), text(size: 13pt, "{date}"), text(font: "{arabic}", size: 15pt, dir: rtl, "{date_ar}")))))"#,
            x = b + 60.0,
            y = b + PAGE_H - 70.0,
            bw = PAGE_W - 120.0,
            paper = color(&d.paper),
            title = d.title_font,
            arabic = d.arabic_font,
            t = escape(&texts.cover_title),
            date = escape(&texts.cover_date),
            date_ar = escape(&texts.cover_date_ar),
        );
        source.push_str("#pagebreak()\n");

        // Opening page.
        let _ = writeln!(
            source,
            r#"#place(dx: {x}mm, dy: {y}mm, box(width: {bw}mm, align(center, stack(spacing: 6mm, text(font: "{title}", size: 26pt, "{t}"), text(font: "{arabic}", size: 22pt, dir: rtl, "{ar}")))))"#,
            x = b + 40.0,
            y = b + 70.0,
            bw = PAGE_W - 80.0,
            title = d.title_font,
            arabic = d.arabic_font,
            t = escape(&texts.opening.replace('\n', " · ")),
            ar = escape(&texts.opening_ar),
        );
        let _ = writeln!(source, "#place(dx: {}mm, dy: {}mm, rect(width: 12mm, height: 0.7mm, fill: emerald))", b + (PAGE_W - 12.0) / 2.0, b + 130.0);
        source.push_str("#pagebreak()\n");

        // Spreads, cut into single pages.
        let mut page = 2;
        for spread in &job.spreads {
            // Both halves carry the Spread's own (left, right) numbers; each shows its side's.
            let numbers = (page, page + 1);
            for side in 0..2 {
                // Shift the Spread so this page sits inside the trim box.
                let x0 = b - side as f64 * PAGE_W;
                let mut body = String::new();
                body.push_str(&self.spread_body(spread, &job.images, x0, b, numbers));
                let _ = writeln!(source, "#box(width: {w}mm, height: {h}mm, clip: true)[\n{body}]");
                source.push_str("#pagebreak()\n");
            }
            page += 2;
        }

        // Closing page.
        let _ = writeln!(
            source,
            r#"#place(dx: {x}mm, dy: {y}mm, box(width: {bw}mm, align(center, stack(spacing: 6mm, text(font: "{title}", size: 24pt, "{t}"), text(font: "{arabic}", size: 20pt, dir: rtl, "{ar}")))))"#,
            x = b + 40.0,
            y = b + 85.0,
            bw = PAGE_W - 80.0,
            title = d.title_font,
            arabic = d.arabic_font,
            t = escape(&texts.closing),
            ar = escape(&texts.closing_ar),
        );
        source.push_str("#pagebreak()\n");

        // Cover back: the velodrome, as two fine concentric tracks.
        let _ = writeln!(
            source,
            "#place(dx: {}mm, dy: {}mm, ellipse(width: 96mm, height: 50mm, stroke: 0.8pt + emerald))\n#place(dx: {}mm, dy: {}mm, ellipse(width: 88mm, height: 42mm, stroke: 0.4pt + emerald.transparentize(40%)))",
            b + (PAGE_W - 96.0) / 2.0,
            b + 80.0,
            b + (PAGE_W - 88.0) / 2.0,
            b + 84.0,
        );
        self.compile(&source, out, None)
    }
}
