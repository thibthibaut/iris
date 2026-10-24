use anyhow::Result;
use image::{DynamicImage, imageops::FilterType};
use imageproc::gradients::{horizontal_sobel, vertical_sobel};

use crate::{
    db::QualityResult,
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
    stages::{for_each_pending, load_preview},
};

/// Sharpness, exposure, "looks like a screenshot" and the perceptual hash, from the proxy.
pub struct Quality;

impl Stage for Quality {
    fn name(&self) -> &'static str {
        "quality"
    }

    fn title(&self) -> &'static str {
        "Image quality"
    }

    fn description(&self) -> &'static str {
        "Sharpness, exposure, screenshot detection and perceptual hash, from the 1200 px proxy."
    }

    fn version(&self) -> i64 {
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &["ingest"]
    }

    fn per_photo(&self) -> bool {
        true
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        for_each_pending(ctx, self, sink, cancel, threads, |photo| {
            let image = load_preview(ctx, photo)?;
            let small = image.resize(512, 512, FilterType::Triangle);
            let quality = QualityResult {
                tenengrad: tenengrad(&small),
                exposure_score: exposure_score(&small),
                screenshot_score: edge_density(&small),
                phash: dhash(&image),
            };
            ctx.db.save_quality(photo.id, &quality)
        })
    }
}

/// Mean squared Sobel gradient: higher is sharper.
pub fn tenengrad(img: &DynamicImage) -> f64 {
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    if w == 0 || h == 0 {
        return 0.0;
    }
    let gx = horizontal_sobel(&gray);
    let gy = vertical_sobel(&gray);
    let sum: f64 = gx
        .pixels()
        .zip(gy.pixels())
        .map(|(x, y)| {
            let (sx, sy) = (f64::from(x[0]), f64::from(y[0]));
            sx.mul_add(sx, sy * sy)
        })
        .sum();
    sum / (f64::from(w) * f64::from(h))
}

/// 1 when nothing is crushed to black or blown to white.
fn exposure_score(img: &DynamicImage) -> f64 {
    let gray = img.to_luma8();
    let total = f64::from(gray.width() * gray.height()).max(1.0);
    let clipped = gray.pixels().filter(|p| p[0] < 10 || p[0] > 245).count() as f64;
    (1.0 - clipped / total).clamp(0.0, 1.0)
}

/// Share of strong edges: text and UI make screenshots score high.
fn edge_density(img: &DynamicImage) -> f64 {
    let gray = img.to_luma8();
    let gx = horizontal_sobel(&gray);
    let gy = vertical_sobel(&gray);
    let total = f64::from(gray.width() * gray.height()).max(1.0);
    let strong = gx
        .pixels()
        .zip(gy.pixels())
        .filter(|(x, y)| {
            let (sx, sy) = (f64::from(x[0]), f64::from(y[0]));
            sx.mul_add(sx, sy * sy).sqrt() > 80.0
        })
        .count() as f64;
    strong / total
}

/// 64-bit difference hash of a 9×8 grey thumbnail.
fn dhash(img: &DynamicImage) -> u64 {
    let small = img.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut hash = 0_u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0] {
                hash |= 1;
            }
        }
    }
    hash
}
