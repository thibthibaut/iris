use std::{fs::File, io::BufReader};

use anyhow::Result;
use exif::{In, Reader, Tag, Value};
use tracing::{info, warn};

use crate::{
    app::AppContext, image_io::open_image, models::PhotoMetadata, processors::progress,
    traits::BatchProcessor,
};

pub struct MetadataProcessor;

impl BatchProcessor for MetadataProcessor {
    fn name(&self) -> &'static str {
        "metadata"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx.db.photos_missing_metadata(ctx.effective_limit())?;
        let pb = progress::bar(photos.len(), "metadata");
        let mut done = 0;
        let mut failed = 0;

        for photo in photos {
            let _ = (&photo.blake3_hash, photo.width, photo.height);
            match extract_metadata(&photo.path) {
                Ok(metadata) => {
                    ctx.db.save_metadata(photo.id, &metadata)?;
                    done += 1;
                }
                Err(error) => {
                    failed += 1;
                    warn!(path = %photo.path, %error, "failed to extract metadata");
                }
            }
            pb.inc(1);
        }

        pb.finish_and_clear();

        info!(processor = self.name(), done, failed, "metadata completed");
        Ok(())
    }
}

fn extract_metadata(path: &str) -> Result<PhotoMetadata> {
    let mut result = PhotoMetadata {
        taken_at: None,
        gps_lat: None,
        gps_lon: None,
        camera_model: None,
        orientation: None,
        width: None,
        height: None,
    };

    if let Ok(img) = open_image(path.as_ref()) {
        result.width = Some(img.width());
        result.height = Some(img.height());
    }

    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        if let Ok(exif) = Reader::new().read_from_container(&mut reader) {
            result.taken_at = field_string(&exif, Tag::DateTimeOriginal)
                .or_else(|| field_string(&exif, Tag::DateTime));
            result.camera_model = field_string(&exif, Tag::Model);
            result.orientation = exif
                .get_field(Tag::Orientation, In::PRIMARY)
                .and_then(|field| field.value.get_uint(0))
                .and_then(|value| i32::try_from(value).ok());
            result.gps_lat = gps_coordinate(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef);
            result.gps_lon = gps_coordinate(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef);
        }
    }

    Ok(result)
}

fn field_string(exif: &exif::Exif, tag: Tag) -> Option<String> {
    exif.get_field(tag, In::PRIMARY)
        .map(|field| field.display_value().with_unit(exif).to_string())
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

fn gps_coordinate(exif: &exif::Exif, coord_tag: Tag, ref_tag: Tag) -> Option<f64> {
    let coord = exif.get_field(coord_tag, In::PRIMARY)?;
    let values = match &coord.value {
        Value::Rational(values) if values.len() >= 3 => values,
        _ => return None,
    };

    let degrees = values[0].to_f64();
    let minutes = values[1].to_f64();
    let seconds = values[2].to_f64();
    let mut decimal = degrees + minutes / 60.0 + seconds / 3600.0;

    if let Some(reference) = field_string(exif, ref_tag)
        && matches!(reference.as_str(), "S" | "W")
    {
        decimal = -decimal;
    }

    Some(decimal)
}
