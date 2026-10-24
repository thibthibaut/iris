//! Quality v2 and the quality gate: computed on the fly from stored measurements, so
//! threshold changes and Couple labels apply at once.

use crate::model::{Gate, Source};

/// Measurements of one Photo (from the quality, embed and faces stages).
#[derive(Debug, Clone, Default)]
pub struct Measurements {
    pub source: Option<Source>,
    pub tenengrad: Option<f64>,
    pub exposure: Option<f64>,
    pub clip_aesthetic: Option<f64>,
    pub clip_screenshot: Option<f64>,
    pub jpeg_quality: Option<f64>,
    pub megapixels: Option<f64>,
    /// Sharpness of the important faces (the Couple and the biggest faces).
    pub important_face_sharpness: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    pub quality: f64,
    pub face_sharpness: Option<f64>,
    pub rejected: Option<String>,
}

pub fn evaluate(m: &Measurements, gate: &Gate) -> Evaluation {
    let sharpness = (m.tenengrad.unwrap_or(0.0) / 2500.0).clamp(0.0, 1.0);
    let exposure = m.exposure.unwrap_or(0.5);
    let aesthetic = m.clip_aesthetic.unwrap_or(0.5);
    let face_sharpness = m.important_face_sharpness.iter().copied().reduce(f64::min);
    let quality = match face_sharpness {
        Some(face) => 0.25 * sharpness + 0.15 * exposure + 0.25 * aesthetic + 0.35 * face.min(1.0),
        None => 0.45 * sharpness + 0.2 * exposure + 0.35 * aesthetic,
    };

    let mut reasons = Vec::new();
    let guest = m.source == Some(Source::Guest);
    let min_quality = if guest { gate.guest_min_quality } else { gate.photographer_min_quality };
    if quality < min_quality {
        reasons.push("low quality");
    }
    if m.clip_screenshot.is_some_and(|s| s > gate.screenshot_max) {
        reasons.push("screenshot");
    }
    if face_sharpness.is_some_and(|s| s < gate.min_face_sharpness) {
        reasons.push("blurry faces");
    }
    if guest {
        if m.jpeg_quality.is_some_and(|q| q < gate.guest_min_jpeg_q) {
            reasons.push("heavily compressed");
        }
        if m.megapixels.is_some_and(|mp| mp < gate.guest_min_megapixels) {
            reasons.push("too small to print");
        }
    }
    Evaluation {
        quality,
        face_sharpness,
        rejected: (!reasons.is_empty()).then(|| reasons.join(", ")),
    }
}
