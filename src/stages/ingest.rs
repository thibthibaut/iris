use std::io::Cursor;

use anyhow::{Context, Result};
use exif::{In, Reader, Tag};

use crate::{
    db::{IngestResult, PhotoRecord},
    image_io::{GRID_SIZE, PREVIEW_SIZE, proxy_path, save_webp, thumbnail_from_buffer, vips_dimensions},
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
    stages::for_each_pending,
};

/// Reads each original exactly once (the drive is slow): content hash, EXIF, JPEG quality
/// and the two WebP proxies every later stage and the GUI work from.
pub struct Ingest;

impl Stage for Ingest {
    fn name(&self) -> &'static str {
        "ingest"
    }

    fn title(&self) -> &'static str {
        "Read photos"
    }

    fn description(&self) -> &'static str {
        "Reads each original once: hash, capture time, camera, JPEG quality, and the 400 / 1200 px WebP proxies."
    }

    fn version(&self) -> i64 {
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &["scan"]
    }

    fn per_photo(&self) -> bool {
        true
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        for_each_pending(ctx, self, sink, cancel, 3, |photo| ingest(ctx, photo))
    }
}

fn ingest(ctx: &StageCtx, photo: &PhotoRecord) -> Result<()> {
    let bytes = std::fs::read(&photo.path).with_context(|| format!("failed to read {}", photo.path))?;
    let blake3_hash = blake3::hash(&bytes).to_hex().to_string();

    // libvips streams the decode, so each image is read once: the grid proxy is made from
    // the saved preview, not from the same pipeline. Exact copies share their proxies.
    let webp_dir = ctx.config.webp_dir();
    let preview_path = proxy_path(&webp_dir, &blake3_hash, PREVIEW_SIZE);
    let grid_path = proxy_path(&webp_dir, &blake3_hash, GRID_SIZE);
    if !preview_path.exists() {
        let preview = thumbnail_from_buffer(&bytes, PREVIEW_SIZE).context("failed to decode")?;
        save_webp(&preview, &preview_path, 80)?;
    }
    let preview_bytes = std::fs::read(&preview_path)?;
    let preview = libvips::VipsImage::new_from_buffer(&preview_bytes, "")
        .map_err(|error| crate::image_io::vips_error(&error))?;
    if !grid_path.exists() {
        save_webp(&thumbnail_from_buffer(&preview_bytes, GRID_SIZE)?, &grid_path, 75)?;
    }

    // Upright dimensions: the header's, swapped when the upright proxy turned out the other way.
    let header = libvips::VipsImage::new_from_buffer(&bytes, "")
        .map_err(|error| crate::image_io::vips_error(&error))?;
    let (header_w, header_h) = vips_dimensions(&header);
    let (proxy_w, proxy_h) = vips_dimensions(&preview);
    let (width, height) = if (header_w >= header_h) == (proxy_w >= proxy_h) {
        (header_w, header_h)
    } else {
        (header_h, header_w)
    };

    let mut result = IngestResult {
        blake3_hash,
        format: format_of(&bytes).to_string(),
        width,
        height,
        taken_at_source: "none",
        jpeg_q_est: jpeg_quality(&bytes),
        ..IngestResult::default()
    };
    if let Ok(exif) = Reader::new().read_from_container(&mut Cursor::new(&bytes)) {
        result.camera_model = field_string(&exif, Tag::Model);
        if let Some(taken) = capture_time(&exif) {
            result.taken_at_utc = Some(taken);
            result.taken_at_source = "exif_original";
        }
    }
    ctx.db.save_ingest(photo.id, &result)
}

fn format_of(bytes: &[u8]) -> &'static str {
    match bytes {
        [0xFF, 0xD8, ..] => "jpeg",
        [0x89, b'P', b'N', b'G', ..] => "png",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "webp",
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => "heic",
        _ => "other",
    }
}

fn field_string(exif: &exif::Exif, tag: Tag) -> Option<String> {
    exif.get_field(tag, In::PRIMARY)
        .map(|field| field.display_value().to_string())
        .map(|value| value.trim().trim_matches('"').trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `DateTimeOriginal` (+ sub-seconds), with its offset if recorded, else Paris time.
fn capture_time(exif: &exif::Exif) -> Option<i64> {
    let raw = match &exif.get_field(Tag::DateTimeOriginal, In::PRIMARY)?.value {
        exif::Value::Ascii(values) => values.first()?.clone(),
        _ => return None,
    };
    let mut datetime = exif::DateTime::from_ascii(&raw).ok()?;
    if let Some(field) = exif.get_field(Tag::SubSecTimeOriginal, In::PRIMARY)
        && let exif::Value::Ascii(values) = &field.value
        && let Some(sub) = values.first()
    {
        let _ = datetime.parse_subsec(sub);
    }
    if let Some(field) = exif.get_field(Tag::OffsetTimeOriginal, In::PRIMARY)
        && let exif::Value::Ascii(values) = &field.value
        && let Some(offset) = values.first()
    {
        let _ = datetime.parse_offset(offset);
    }
    let date = chrono::NaiveDate::from_ymd_opt(i32::from(datetime.year), u32::from(datetime.month), u32::from(datetime.day))?;
    let local = date.and_hms_nano_opt(
        u32::from(datetime.hour),
        u32::from(datetime.minute),
        u32::from(datetime.second),
        datetime.nanosecond.unwrap_or(0),
    )?;
    match datetime.offset {
        Some(minutes) => Some((local - chrono::Duration::minutes(i64::from(minutes))).and_utc().timestamp_millis()),
        None => iris_core::capture::paris_to_utc_ms(local),
    }
}

/// Estimates the IJG quality a JPEG was saved at, from its luminance quantisation table.
/// WhatsApp re-encodes at about 70–80.
fn jpeg_quality(bytes: &[u8]) -> Option<f64> {
    const STD_LUMA: [u16; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22,
        29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103,
        121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut pos = 2;
    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xFF {
            return None;
        }
        let marker = bytes[pos + 1];
        let len = usize::from(u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]));
        if marker == 0xDA {
            return None;
        }
        if marker == 0xDB {
            let mut table = pos + 4;
            let end = (pos + 2 + len).min(bytes.len());
            while table < end {
                let precision = bytes[table] >> 4;
                let id = bytes[table] & 0x0F;
                let size = if precision == 0 { 64 } else { 128 };
                if id == 0 {
                    let values: Vec<u16> = (0..64)
                        .map(|i| {
                            if precision == 0 {
                                u16::from(*bytes.get(table + 1 + i).unwrap_or(&1))
                            } else {
                                u16::from_be_bytes([
                                    *bytes.get(table + 1 + 2 * i).unwrap_or(&0),
                                    *bytes.get(table + 2 + 2 * i).unwrap_or(&1),
                                ])
                            }
                        })
                        .collect();
                    let sum: f64 = values.iter().map(|&v| f64::from(v)).sum();
                    let std: f64 = STD_LUMA.iter().map(|&v| f64::from(v)).sum();
                    let scale = sum / std * 100.0;
                    let quality = if scale <= 100.0 { (200.0 - scale) / 2.0 } else { 5000.0 / scale };
                    return Some(quality.clamp(1.0, 100.0));
                }
                table += 1 + size;
            }
        }
        pos += 2 + len;
    }
    None
}
