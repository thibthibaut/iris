use anyhow::Result;
use image::{DynamicImage, imageops::FilterType};
use imageproc::gradients::{horizontal_sobel, vertical_sobel};
use tracing::{info, warn};

use crate::{
    app::AppContext, image_io::open_image, models::ImageQuality, processors::progress,
    traits::BatchProcessor,
};

pub struct QualityProcessor;

impl BatchProcessor for QualityProcessor {
    fn name(&self) -> &'static str {
        "quality"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx.db.photos_missing_quality(ctx.effective_limit())?;
        let pb = progress::bar(photos.len(), "quality");
        let mut done = 0;
        let mut failed = 0;

        for photo in photos {
            let _ = (&photo.blake3_hash, photo.width, photo.height);
            match open_image(photo.path.as_ref()).map(|img| compute_quality(&img)) {
                Ok(quality) => {
                    ctx.db.save_quality(photo.id, &quality)?;
                    done += 1;
                }
                Err(error) => {
                    failed += 1;
                    warn!(path = %photo.path, %error, "failed to compute quality");
                }
            }
            pb.inc(1);
        }

        pb.finish_and_clear();

        info!(processor = self.name(), done, failed, "quality completed");
        Ok(())
    }
}

pub fn compute_quality(img: &DynamicImage) -> ImageQuality {
    let tenengrad = tenengrad_sharpness(img);
    let exposure = exposure_score(img);
    let screenshot = screenshot_score(img);
    let normalized_tenengrad = (tenengrad / 3000.0).clamp(0.0, 1.0);
    let quality = normalized_tenengrad * 0.8 + exposure * 0.2;

    ImageQuality {
        tenengrad_sharpness: tenengrad,
        exposure_score: exposure,
        screenshot_score: screenshot,
        quality_score: quality,
    }
}

pub fn tenengrad_sharpness(img: &DynamicImage) -> f64 {
    let img = resize_max_dim(img, 512);
    let gray = img.to_luma8();
    let gx = horizontal_sobel(&gray);
    let gy = vertical_sobel(&gray);
    let (w, h) = gray.dimensions();

    if w == 0 || h == 0 {
        return 0.0;
    }

    let mut sum = 0.0;
    for y in 0..h {
        for x in 0..w {
            let sx = f64::from(gx.get_pixel(x, y)[0]);
            let sy = f64::from(gy.get_pixel(x, y)[0]);
            sum += sx.mul_add(sx, sy * sy);
        }
    }

    sum / (f64::from(w) * f64::from(h))
}

pub fn exposure_score(img: &DynamicImage) -> f64 {
    let img = resize_max_dim(img, 512);
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    if w == 0 || h == 0 {
        return 0.0;
    }

    let total = f64::from(w) * f64::from(h);
    let mut dark = 0_u64;
    let mut bright = 0_u64;

    for pixel in gray.pixels() {
        let luma = pixel[0];
        if luma < 10 {
            dark += 1;
        }
        if luma > 245 {
            bright += 1;
        }
    }

    let bad_exposure = dark as f64 / total + bright as f64 / total;
    (1.0 - bad_exposure).clamp(0.0, 1.0)
}

pub fn screenshot_score(img: &DynamicImage) -> f64 {
    sobel_edge_density(img)
}

pub fn sobel_edge_density(img: &DynamicImage) -> f64 {
    let img = resize_max_dim(img, 512);
    let gray = img.to_luma8();
    let gx = horizontal_sobel(&gray);
    let gy = vertical_sobel(&gray);
    let (w, h) = gray.dimensions();

    if w == 0 || h == 0 {
        return 0.0;
    }

    let mut edge_pixels = 0_u64;
    let total_pixels = u64::from(w) * u64::from(h);

    for y in 0..h {
        for x in 0..w {
            let sx = f64::from(gx.get_pixel(x, y)[0]);
            let sy = f64::from(gy.get_pixel(x, y)[0]);
            let magnitude = sx.mul_add(sx, sy * sy).sqrt();
            if magnitude > 80.0 {
                edge_pixels += 1;
            }
        }
    }

    edge_pixels as f64 / total_pixels as f64
}

fn resize_max_dim(img: &DynamicImage, max_dim: u32) -> DynamicImage {
    let w = img.width();
    let h = img.height();

    if w <= max_dim && h <= max_dim {
        return img.clone();
    }

    let scale = max_dim as f32 / w.max(h) as f32;
    let new_w = (w as f32 * scale).round() as u32;
    let new_h = (h as f32 * scale).round() as u32;
    img.resize(new_w.max(1), new_h.max(1), FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, GrayImage, Luma};

    use super::*;

    #[test]
    fn exposure_scores_black_badly() {
        let img = DynamicImage::ImageLuma8(GrayImage::from_pixel(10, 10, Luma([0])));
        assert_eq!(exposure_score(&img), 0.0);
    }

    #[test]
    fn exposure_scores_midtones_well() {
        let img = DynamicImage::ImageLuma8(GrayImage::from_pixel(10, 10, Luma([128])));
        assert_eq!(exposure_score(&img), 1.0);
    }

    #[test]
    fn sharpness_runs() {
        let img = DynamicImage::ImageLuma8(GrayImage::from_pixel(10, 10, Luma([128])));
        assert!(tenengrad_sharpness(&img) >= 0.0);
    }
}
