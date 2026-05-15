use std::path::Path;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, RgbaImage};

pub fn open_image(path: &Path) -> Result<DynamicImage> {
    if is_heic(path) {
        return open_heic(path);
    }

    image::open(path).with_context(|| format!("failed to decode image {}", path.display()))
}

fn open_heic(path: &Path) -> Result<DynamicImage> {
    let data = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let output = heic::DecoderConfig::new()
        .decode(&data, heic::PixelLayout::Rgba8)
        .map_err(|error| anyhow::anyhow!("{error:?}"))
        .with_context(|| format!("failed to decode HEIC {}", path.display()))?;
    let image =
        RgbaImage::from_raw(output.width, output.height, output.data).with_context(|| {
            format!(
                "HEIC decoder returned invalid buffer for {}",
                path.display()
            )
        })?;
    Ok(DynamicImage::ImageRgba8(image))
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

    #[test]
    fn detects_supported_extensions() {
        assert!(is_supported_image_path(Path::new("a.JPG")));
        assert!(is_supported_image_path(Path::new("a.heic")));
        assert!(!is_supported_image_path(Path::new("a.mov")));
    }
}
