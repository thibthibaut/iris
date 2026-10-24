//! Capture-time helpers: when a Photo was taken, or the latest it can have been taken.

/// Parses the time a Guest Photo was sent or saved from its file name, as unix ms (UTC).
/// The name carries Paris local time. This is never the capture time, only an upper bound.
pub fn sent_at_from_file_name(file_name: &str) -> Option<i64> {
    const FORMATS: [(&str, &str); 4] = [
        ("WhatsApp Image ", "%Y-%m-%d at %H.%M.%S"),
        ("WhatsApp Video ", "%Y-%m-%d at %H.%M.%S"),
        ("signal-", "%Y-%m-%d-%H%M%S"),
        ("photo_", "%Y-%m-%d %H.%M.%S"),
    ];

    FORMATS.iter().find_map(|(prefix, format)| {
        let rest = file_name.strip_prefix(prefix)?;
        let (local, _) = chrono::NaiveDateTime::parse_and_remainder(rest, format).ok()?;
        paris_to_utc_ms(local)
    })
}

/// Converts a Paris wall-clock time to unix ms (UTC).
pub fn paris_to_utc_ms(local: chrono::NaiveDateTime) -> Option<i64> {
    use chrono::TimeZone;
    chrono_tz::Europe::Paris
        .from_local_datetime(&local)
        .earliest()
        .map(|time| time.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_ms(rfc3339: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn parses_messenger_file_names_in_paris_time() {
        let cases = [
            (
                "WhatsApp Image 2025-07-19 at 14.36.03.jpeg",
                Some(utc_ms("2025-07-19T12:36:03Z")),
            ),
            (
                "WhatsApp Image 2025-07-20 at 18.05.35 (3).jpeg",
                Some(utc_ms("2025-07-20T16:05:35Z")),
            ),
            (
                "signal-2025-07-20-194953_002.jpeg",
                Some(utc_ms("2025-07-20T17:49:53Z")),
            ),
            (
                "photo_2025-07-21 20.12.34.jpeg",
                Some(utc_ms("2025-07-21T18:12:34Z")),
            ),
            ("DSC_9562.jpg", None),
            ("IMG_4090.jpeg", None),
        ];

        for (name, expected) in cases {
            assert_eq!(sent_at_from_file_name(name), expected, "{name}");
        }
    }
}
