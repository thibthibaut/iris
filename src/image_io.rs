use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, GrayImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use libvips::{VipsImage, ops};

/// Decodes the full-size image, upright (EXIF orientation applied).
/// Only for final rendering: analysis uses [`open_for_analysis`].
pub fn open_image(path: &Path) -> Result<DynamicImage> {
    if is_heic(path) {
        return open_for_analysis(path, u32::MAX >> 2);
    }

    let decode = || -> Result<DynamicImage> {
        let mut decoder = ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut image = DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        Ok(image)
    };
    decode().with_context(|| format!("failed to decode image {}", path.display()))
}

/// Decodes an upright, downscaled copy (longest side ≤ `max_side`) with libvips, which
/// shrinks JPEGs on load and handles HEIC: fast and light on memory.
pub fn open_for_analysis(path: &Path, max_side: u32) -> Result<DynamicImage> {
    let _ = crate::webp_cache::vips();
    let side = i32::try_from(max_side).unwrap_or(i32::MAX);
    let options = ops::ThumbnailOptions {
        height: side,
        size: ops::Size::Down,
        // The binding passes empty profile names through, which libvips tries to load.
        input_profile: "srgb".into(),
        output_profile: "srgb".into(),
        ..Default::default()
    };
    let path_str = path.to_string_lossy();
    let image = ops::thumbnail_with_opts(&path_str, side, &options)
        .and_then(|image| ops::colourspace(&image, ops::Interpretation::Srgb))
        .and_then(|image| ops::cast(&image, ops::BandFormat::Uchar))
        .map_err(|error| vips_error(&error))
        .with_context(|| format!("libvips failed to decode {}", path.display()))?;
    vips_to_dynamic(&image).with_context(|| format!("unexpected pixel layout in {}", path.display()))
}

fn vips_error(error: &libvips::error::Error) -> anyhow::Error {
    let detail = crate::webp_cache::vips().error_buffer().unwrap_or_default().trim().to_string();
    crate::webp_cache::vips().error_clear();
    anyhow!("{error:?}: {detail}")
}

fn vips_to_dynamic(image: &VipsImage) -> Result<DynamicImage> {
    let width = u32::try_from(image.get_width())?;
    let height = u32::try_from(image.get_height())?;
    let pixels = image.image_write_to_memory();
    let invalid = || anyhow!("pixel buffer does not match {width}×{height}");
    Ok(match image.get_bands() {
        1 => DynamicImage::ImageLuma8(GrayImage::from_raw(width, height, pixels).ok_or_else(invalid)?),
        2 => DynamicImage::ImageLumaA8(
            image::GrayAlphaImage::from_raw(width, height, pixels).ok_or_else(invalid)?,
        ),
        3 => DynamicImage::ImageRgb8(RgbImage::from_raw(width, height, pixels).ok_or_else(invalid)?),
        4 => DynamicImage::ImageRgba8(RgbaImage::from_raw(width, height, pixels).ok_or_else(invalid)?),
        bands => bail!("unsupported band count {bands}"),
    })
}

pub fn is_supported_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "heic" | "webp")
    )
}

pub fn ensure_supported(path: &Path) -> Result<()> {
    if is_supported_image_path(path) {
        Ok(())
    } else {
        bail!("unsupported image extension: {}", path.display())
    }
}

fn is_heic(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("heic"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A 40×20 JPEG whose EXIF says "rotate 90° clockwise to display" (orientation 6).
    fn write_orientation_6_jpeg(path: &Path) {
        let mut jpeg = Vec::new();
        image::DynamicImage::new_rgb8(40, 20)
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        // Big-endian TIFF with one IFD entry: Orientation (0x0112), SHORT, count 1, value 6.
        let tiff: [u8; 26] = [
            b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0,
            0,
        ];
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let len = u16::try_from(app1.len() + 2).unwrap().to_be_bytes();
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1, len[0], len[1]]);
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn applies_exif_orientation_when_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("portrait.jpg");
        write_orientation_6_jpeg(&path);

        let full = open_image(&path).unwrap();
        assert_eq!((full.width(), full.height()), (20, 40));

        let analysis = open_for_analysis(&path, 1024).unwrap();
        assert_eq!((analysis.width(), analysis.height()), (20, 40));
    }

    #[test]
    fn detects_supported_extensions() {
        assert!(is_supported_image_path(Path::new("a.JPG")));
        assert!(is_supported_image_path(Path::new("a.heic")));
        assert!(!is_supported_image_path(Path::new("a.mov")));
    }
}
