use anyhow::Result;
use isocountry::CountryCode;
use rayon::prelude::*;
use reverse_geocoder::{Record, ReverseGeocoder};
use tracing::info;

use crate::{app::AppContext, models::GeoLocation, processors::progress, traits::BatchProcessor};

pub struct ReverseGeoProcessor;

impl BatchProcessor for ReverseGeoProcessor {
    fn name(&self) -> &'static str {
        "geo"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let candidates = ctx.db.photos_missing_geo(ctx.limit)?;
        let pb = progress::bar(candidates.len(), "geo");
        let geocoder = ReverseGeocoder::new();
        let mut done = 0;

        let results = candidates
            .par_iter()
            .map(|candidate| {
                let result = geocoder.search((candidate.gps_lat, candidate.gps_lon));
                let location = location_from_record(result.record);
                pb.inc(1);
                (candidate.id, location)
            })
            .collect::<Vec<_>>();

        for (photo_id, location) in results {
            ctx.db.save_geo_location(photo_id, &location)?;
            done += 1;
        }

        pb.finish_and_clear();

        info!(processor = self.name(), done, "reverse geocoding completed");
        Ok(())
    }
}

fn location_from_record(record: &Record) -> GeoLocation {
    let city = non_empty(&record.name);
    let region = non_empty(&record.admin1).or_else(|| non_empty(&record.admin2));
    let country_code = non_empty(&record.cc).map(|value| value.to_ascii_uppercase());
    let country = country_code.as_deref().and_then(country_name);
    let label = geo_label(city.as_deref(), region.as_deref(), country.as_deref());

    GeoLocation {
        city,
        region,
        country,
        country_code,
        label,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn country_name(country_code: &str) -> Option<String> {
    CountryCode::for_alpha2_caseless(country_code)
        .ok()
        .map(|country| country.name().to_string())
}

fn geo_label(city: Option<&str>, region: Option<&str>, country: Option<&str>) -> Option<String> {
    let parts = [city, region, country]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    (!parts.is_empty()).then(|| parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_country_code_to_name() {
        assert_eq!(country_name("FR"), Some("France".to_string()));
        assert_eq!(country_name("zz"), None);
    }

    #[test]
    fn builds_geo_label() {
        assert_eq!(
            geo_label(Some("Paris"), Some("Ile-de-France"), Some("France")),
            Some("Paris, Ile-de-France, France".to_string())
        );
        assert_eq!(geo_label(None, None, None), None);
    }
}
