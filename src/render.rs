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
use iris_core::model::{Design, PAGE_H, PAGE_W, PhotoId, PlacedPhoto, Settings, Spread, SpreadKind};

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
                .map(|g| (g.key.clone(), (get(&format!("group.{}.fr", g.key), &g.name), get(&format!("group.{}.ar", g.key), ""))))
                .collect(),
            opening: get("opening.fr", &format!("{}\n{}", settings.design.names, settings.design.cover_date_fr)),
            opening_ar: get("opening.ar", ""),
            closing: get("closing.fr", "Merci d'avoir été là."),
            closing_ar: get("closing.ar", ""),
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
    fn render_print(&self, job: &PrintJob, out: &Path) -> Result<()>;
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
#let stripes(x, y, w) = {{
  place(dx: x, dy: y, rect(width: w, height: 1.1mm, fill: emerald))
  place(dx: x, dy: y + 1.9mm, rect(width: w, height: 1.1mm, fill: red))
  place(dx: x, dy: y + 3.8mm, rect(width: w, height: 1.1mm, fill: yellow))
}}
#let track(x, y, n) = place(dx: x, dy: y, box(width: 13mm, height: 7mm, {{
  place(ellipse(width: 13mm, height: 7mm, stroke: 0.5pt + emerald))
  place(dx: 1.2mm, dy: 0.9mm, ellipse(width: 10.6mm, height: 5.2mm, stroke: 0.3pt + red))
  align(center + horizon, text(size: 7.5pt, fill: ink, font: "{title}")[#n])
}}))
#let dots(x, y) = {{
  place(dx: x, dy: y, circle(radius: 1.1mm, fill: yellow))
  place(dx: x + 4mm, dy: y, circle(radius: 1.1mm, fill: pink))
  place(dx: x + 8mm, dy: y, circle(radius: 1.1mm, fill: blue))
}}
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

    /// The French and Arabic titles at the same size, over the racing stripes.
    fn title_block(&self, x: f64, fr: &str, ar: &str) -> String {
        let d = &self.design;
        let mut out = String::new();
        let _ = writeln!(
            out,
            r#"#place(dx: {x}mm + 15mm, dy: 19mm, box(width: {w}mm, text(font: "{font}", size: 30pt, fill: ink, "{fr}")))"#,
            w = PAGE_W - 30.0,
            font = d.title_font,
            fr = escape(fr),
        );
        if !ar.trim().is_empty() {
            let _ = writeln!(
                out,
                r#"#place(dx: {x}mm + 15mm, dy: 17mm, box(width: {w}mm, align(right, text(font: "{font}", size: 30pt, dir: rtl, fill: ink, "{ar}"))))"#,
                w = PAGE_W - 30.0,
                font = d.arabic_font,
                ar = escape(ar),
            );
        }
        let _ = writeln!(out, "#stripes({x}mm + 15mm, 44mm, {w}mm)", w = PAGE_W - 30.0);
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
            let _ = writeln!(out, "#dots({}mm, {}mm)", x0 + 2.0 * PAGE_W - 15.0 - 9.0, y0 + 10.0);
        }
        for placed in &spread.photos {
            out.push_str(&photo_call(placed, images, x0, y0));
        }
        let (left, right) = numbers;
        if left > 0 {
            let _ = writeln!(out, "#track({}mm, {}mm, {left})", x0 + 15.0, y0 + PAGE_H - 11.0);
        }
        if right > 0 {
            let _ = writeln!(out, "#track({}mm, {}mm, {right})", x0 + 2.0 * PAGE_W - 28.0, y0 + PAGE_H - 11.0);
        }
        out
    }

    fn compile(&self, source: &str, out: &Path, ppi: Option<u32>) -> Result<()> {
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
        anyhow::ensure!(output.status.success(), "typst failed: {}", String::from_utf8_lossy(&output.stderr));
        Ok(())
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
        self.compile(&source, out, Some(72))
    }

    fn render_print(&self, job: &PrintJob, out: &Path) -> Result<()> {
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
        let _ = writeln!(source, "#stripes({}mm, {}mm, {}mm)", b, b + PAGE_H - 12.0, PAGE_W);
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
        let _ = writeln!(source, "#stripes({}mm, {}mm, {}mm)", b + 100.0, b + 140.0, PAGE_W - 200.0);
        source.push_str("#pagebreak()\n");

        // Spreads, cut into single pages.
        let mut page = 2;
        for spread in &job.spreads {
            for side in 0..2 {
                // Shift the Spread so this page sits inside the trim box.
                let x0 = b - side as f64 * PAGE_W;
                let mut body = String::new();
                let numbers = (page, page + 1);
                body.push_str(&self.spread_body(spread, &job.images, x0, b, numbers));
                let _ = writeln!(source, "#box(width: {w}mm, height: {h}mm, clip: true)[\n{body}]");
                source.push_str("#pagebreak()\n");
                page += 1;
            }
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

        // Cover back: the velodrome, drawn as a track with racing stripes.
        let _ = writeln!(
            source,
            "#place(dx: {}mm, dy: {}mm, ellipse(width: 120mm, height: 64mm, stroke: 2pt + emerald))\n#place(dx: {}mm, dy: {}mm, ellipse(width: 108mm, height: 52mm, stroke: 1.2pt + red))\n#stripes({}mm, {}mm, {}mm)",
            b + (PAGE_W - 120.0) / 2.0,
            b + 60.0,
            b + (PAGE_W - 108.0) / 2.0,
            b + 66.0,
            b,
            b + PAGE_H - 12.0,
            PAGE_W
        );
        self.compile(&source, out, None)
    }
}
