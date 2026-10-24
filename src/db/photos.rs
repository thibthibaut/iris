use anyhow::{Context, Result};
use iris_core::model::{PhotoId, Source};
use rusqlite::{OptionalExtension, Row, params};

use super::{CLIP_EMBEDDING_DIMENSIONS, Database, decode_f32_vec};

#[derive(Debug, Clone)]
pub struct ScannedPhoto {
    pub path: String,
    pub file_name: String,
    pub source: Source,
    pub source_folder: String,
    pub file_size: i64,
    pub modified_at_unix: i64,
    pub sent_at_utc: Option<i64>,
    pub sequence: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct IngestResult {
    pub blake3_hash: String,
    pub format: String,
    pub camera_model: Option<String>,
    pub width: u32,
    pub height: u32,
    pub taken_at_utc: Option<i64>,
    pub taken_at_source: &'static str,
    pub jpeg_q_est: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QualityResult {
    pub tenengrad: f64,
    pub exposure_score: f64,
    pub screenshot_score: f64,
    pub phash: u64,
}

/// Everything the GUI and the album core need about one Photo.
#[derive(Debug, Clone, Default)]
pub struct PhotoRecord {
    pub id: PhotoId,
    pub path: String,
    pub file_name: String,
    pub source: Option<Source>,
    pub source_folder: String,
    pub file_size: i64,
    pub sent_at_utc: Option<i64>,
    pub sequence: Option<i64>,
    pub blake3_hash: Option<String>,
    pub format: Option<String>,
    pub camera_model: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at_utc: Option<i64>,
    pub taken_at_source: String,
    pub jpeg_q_est: Option<f64>,
    pub tenengrad: Option<f64>,
    pub exposure_score: Option<f64>,
    pub screenshot_score: Option<f64>,
    pub phash: Option<u64>,
    pub clip_aesthetic: Option<f64>,
    pub clip_screenshot: Option<f64>,
    pub shot_id: Option<i64>,
    pub chapter: Option<String>,
    pub chapter_prob: Option<f64>,
    pub chapter_margin: Option<f64>,
    pub chapter_source: Option<String>,
    pub chapter_order: Option<f64>,
}

impl PhotoRecord {
    pub fn megapixels(&self) -> Option<f64> {
        Some((self.width? * self.height?) as f64 / 1e6)
    }

    /// Longest side printable at 250 ppi, in mm.
    pub fn max_print_mm(&self) -> Option<f64> {
        Some(self.width?.max(self.height?) as f64 / 250.0 * 25.4)
    }

    pub fn aspect(&self) -> Option<f64> {
        Some(self.width? as f64 / self.height? as f64)
    }
}

const PHOTO_COLUMNS: &str = "id, path, file_name, source, source_folder, file_size, sent_at_utc, sequence,
  blake3_hash, format, camera_model, width, height, taken_at_utc, taken_at_source, jpeg_q_est,
  tenengrad, exposure_score, screenshot_score, phash, clip_aesthetic, clip_screenshot,
  shot_id, chapter, chapter_prob, chapter_margin, chapter_source, chapter_order";

fn photo_from_row(row: &Row<'_>) -> rusqlite::Result<PhotoRecord> {
    Ok(PhotoRecord {
        id: row.get(0)?,
        path: row.get(1)?,
        file_name: row.get(2)?,
        source: row.get::<_, Option<String>>(3)?.map(|s| Source::parse(&s)),
        source_folder: row.get(4)?,
        file_size: row.get(5)?,
        sent_at_utc: row.get(6)?,
        sequence: row.get(7)?,
        blake3_hash: row.get(8)?,
        format: row.get(9)?,
        camera_model: row.get(10)?,
        width: row.get(11)?,
        height: row.get(12)?,
        taken_at_utc: row.get(13)?,
        taken_at_source: row.get(14)?,
        jpeg_q_est: row.get(15)?,
        tenengrad: row.get(16)?,
        exposure_score: row.get(17)?,
        screenshot_score: row.get(18)?,
        phash: row.get::<_, Option<i64>>(19)?.map(|v| v as u64),
        clip_aesthetic: row.get(20)?,
        clip_screenshot: row.get(21)?,
        shot_id: row.get(22)?,
        chapter: row.get(23)?,
        chapter_prob: row.get(24)?,
        chapter_margin: row.get(25)?,
        chapter_source: row.get(26)?,
        chapter_order: row.get(27)?,
    })
}

/// Filters of the Photos page.
#[derive(Debug, Clone, Default)]
pub struct PhotoFilter {
    pub source: Option<Source>,
    pub chapter: Option<String>,
    pub shot: Option<i64>,
    pub ids: Option<Vec<PhotoId>>,
    pub offset: usize,
    pub limit: usize,
}

impl Database {
    pub fn begin_scan(&self) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO scan_runs(started_at_unix) VALUES (?)",
            params![chrono::Utc::now().timestamp()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_scan(&self, scan_id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE scan_runs SET finished_at_unix = ? WHERE id = ?",
            params![chrono::Utc::now().timestamp(), scan_id],
        )?;
        Ok(())
    }

    /// (id, size, mtime) of a known path.
    pub fn photo_file_state(&self, path: &str) -> Result<Option<(i64, i64, i64)>> {
        self.conn()
            .query_row(
                "SELECT id, file_size, modified_at_unix FROM photos WHERE path = ?",
                params![path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("failed to read photo file state")
    }

    /// Inserts a new Photo, or resets a changed one so every stage processes it again.
    pub fn upsert_scanned_photo(&self, photo: &ScannedPhoto, scan_id: i64) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            r#"
INSERT INTO photos(path, file_name, source, source_folder, file_size, modified_at_unix,
  missing, last_seen_scan_id, sent_at_utc, sequence)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9)
ON CONFLICT(path) DO UPDATE SET
  file_name = excluded.file_name, source = excluded.source,
  source_folder = excluded.source_folder, file_size = excluded.file_size,
  modified_at_unix = excluded.modified_at_unix, missing = 0,
  last_seen_scan_id = excluded.last_seen_scan_id, sent_at_utc = excluded.sent_at_utc,
  sequence = excluded.sequence
"#,
            params![
                photo.path,
                photo.file_name,
                photo.source.as_str(),
                photo.source_folder,
                photo.file_size,
                photo.modified_at_unix,
                scan_id,
                photo.sent_at_utc,
                photo.sequence,
            ],
        )?;
        let id: i64 = tx.query_row("SELECT id FROM photos WHERE path = ?", params![photo.path], |row| row.get(0))?;
        tx.execute("DELETE FROM photo_stages WHERE photo_id = ?", params![id])?;
        tx.commit()?;
        Ok(id)
    }

    pub fn mark_seen_unchanged(&self, photo_id: i64, scan_id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE photos SET missing = 0, last_seen_scan_id = ? WHERE id = ?",
            params![scan_id, photo_id],
        )?;
        Ok(())
    }

    pub fn mark_missing_not_seen(&self, scan_id: i64) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE photos SET missing = 1 WHERE last_seen_scan_id IS NOT ? AND missing = 0",
            params![scan_id],
        )?)
    }

    /// Photos the per-Photo stage has not processed at this version (failures included
    /// only when `retry_failed`).
    pub fn photos_pending_stage(&self, stage: &str, version: i64, retry_failed: bool) -> Result<Vec<PhotoRecord>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {PHOTO_COLUMNS} FROM photos WHERE missing = 0 AND NOT EXISTS (
               SELECT 1 FROM photo_stages s WHERE s.photo_id = photos.id AND s.stage = ?1
               AND s.version = ?2 AND (s.ok = 1 OR ?3 = 0)) ORDER BY source, source_folder, file_name"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![stage, version, retry_failed], photo_from_row)?;
        rows.collect::<rusqlite::Result<_>>().context("failed to read pending photos")
    }

    pub fn mark_photo_stage(&self, photo_id: i64, stage: &str, version: i64, error: Option<&str>) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO photo_stages(photo_id, stage, version, ok, error) VALUES (?, ?, ?, ?, ?)",
            params![photo_id, stage, version, error.is_none(), error],
        )?;
        Ok(())
    }

    /// (done, failed, pending) for a per-Photo stage.
    pub fn photo_stage_counts(&self, stage: &str, version: i64) -> Result<(i64, i64, i64)> {
        self.conn()
            .query_row(
                r#"
SELECT
  COALESCE(SUM(CASE WHEN s.ok = 1 THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN s.ok = 0 THEN 1 ELSE 0 END), 0),
  COALESCE(SUM(CASE WHEN s.photo_id IS NULL THEN 1 ELSE 0 END), 0)
FROM photos p
LEFT JOIN photo_stages s ON s.photo_id = p.id AND s.stage = ?1 AND s.version = ?2
WHERE p.missing = 0
"#,
                params![stage, version],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .context("failed to count stage progress")
    }

    /// Photos that failed a per-Photo stage, with their reasons.
    pub fn stage_failures(&self, stage: &str) -> Result<Vec<(PhotoRecord, String)>> {
        let conn = self.conn();
        let columns = PHOTO_COLUMNS.replace("id, path", "p.id, p.path");
        let sql = format!(
            "SELECT {columns}, s.error FROM photos p JOIN photo_stages s ON s.photo_id = p.id
             WHERE s.stage = ? AND s.ok = 0 AND p.missing = 0 ORDER BY p.file_name"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![stage], |row| {
            Ok((photo_from_row(row)?, row.get::<_, Option<String>>(28)?.unwrap_or_default()))
        })?;
        rows.collect::<rusqlite::Result<_>>().context("failed to read stage failures")
    }

    pub fn save_ingest(&self, photo_id: i64, ingest: &IngestResult) -> Result<()> {
        self.conn().execute(
            r#"
UPDATE photos SET blake3_hash = ?, format = ?, camera_model = ?, width = ?, height = ?,
  taken_at_utc = ?, taken_at_source = ?, jpeg_q_est = ?
WHERE id = ?
"#,
            params![
                ingest.blake3_hash,
                ingest.format,
                ingest.camera_model,
                ingest.width,
                ingest.height,
                ingest.taken_at_utc,
                ingest.taken_at_source,
                ingest.jpeg_q_est,
                photo_id,
            ],
        )?;
        Ok(())
    }

    pub fn save_quality(&self, photo_id: i64, quality: &QualityResult) -> Result<()> {
        self.conn().execute(
            "UPDATE photos SET tenengrad = ?, exposure_score = ?, screenshot_score = ?, phash = ? WHERE id = ?",
            params![
                quality.tenengrad,
                quality.exposure_score,
                quality.screenshot_score,
                quality.phash as i64,
                photo_id
            ],
        )?;
        Ok(())
    }

    pub fn save_clip_scores(&self, photo_id: i64, aesthetic: f64, screenshot: f64) -> Result<()> {
        self.conn().execute(
            "UPDATE photos SET clip_aesthetic = ?, clip_screenshot = ? WHERE id = ?",
            params![aesthetic, screenshot, photo_id],
        )?;
        Ok(())
    }

    pub fn save_shots(&self, shots: &[(PhotoId, i64)]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("UPDATE photos SET shot_id = NULL", [])?;
        {
            let mut stmt = tx.prepare("UPDATE photos SET shot_id = ? WHERE id = ?")?;
            for (photo, shot) in shots {
                stmt.execute(params![shot, photo])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// (photo, chapter, probability, margin, source, order)
    pub fn save_chapters(&self, rows: &[(PhotoId, String, f64, f64, &str, f64)]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE photos SET chapter = ?, chapter_prob = ?, chapter_margin = ?, chapter_source = ?, chapter_order = ? WHERE id = ?",
            )?;
            for (photo, chapter, prob, margin, source, order) in rows {
                stmt.execute(params![chapter, prob, margin, source, order, photo])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn photo(&self, photo_id: PhotoId) -> Result<Option<PhotoRecord>> {
        let sql = format!("SELECT {PHOTO_COLUMNS} FROM photos WHERE id = ?");
        self.conn()
            .query_row(&sql, params![photo_id], photo_from_row)
            .optional()
            .context("failed to read photo")
    }

    pub fn all_photos(&self) -> Result<Vec<PhotoRecord>> {
        self.photos(&PhotoFilter { limit: usize::MAX, ..PhotoFilter::default() })
    }

    pub fn photos(&self, filter: &PhotoFilter) -> Result<Vec<PhotoRecord>> {
        let mut clauses = vec!["missing = 0".to_string()];
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(source) = filter.source {
            clauses.push("source = ?".into());
            values.push(source.as_str().to_string().into());
        }
        if let Some(chapter) = &filter.chapter {
            if chapter == "none" {
                clauses.push("chapter IS NULL".into());
            } else {
                clauses.push("chapter = ?".into());
                values.push(chapter.clone().into());
            }
        }
        if let Some(shot) = filter.shot {
            clauses.push("shot_id = ?".into());
            values.push(shot.into());
        }
        if let Some(ids) = &filter.ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            clauses.push(format!("id IN ({list})"));
        }
        let limit = i64::try_from(filter.limit).unwrap_or(i64::MAX);
        let sql = format!(
            "SELECT {PHOTO_COLUMNS} FROM photos WHERE {} ORDER BY chapter IS NULL, chapter_order, source, source_folder, file_name LIMIT {limit} OFFSET {}",
            clauses.join(" AND "),
            filter.offset
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values), photo_from_row)?;
        rows.collect::<rusqlite::Result<_>>().context("failed to read photos")
    }

    pub fn save_image_embedding(&self, photo_id: i64, embedding: &[f32]) -> Result<()> {
        anyhow::ensure!(embedding.len() == CLIP_EMBEDDING_DIMENSIONS, "CLIP embedding dimension mismatch");
        self.conn().execute(
            "INSERT OR REPLACE INTO vec_image_embeddings(rowid, embedding) VALUES (?, ?)",
            params![photo_id, bytemuck::cast_slice::<f32, u8>(embedding)],
        )?;
        Ok(())
    }

    pub fn image_embeddings(&self) -> Result<Vec<(PhotoId, Vec<f32>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.rowid, e.embedding FROM vec_image_embeddings e JOIN photos p ON p.id = e.rowid WHERE p.missing = 0",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        rows.map(|row| {
            let (id, bytes) = row?;
            Ok((id, decode_f32_vec(&bytes, CLIP_EMBEDDING_DIMENSIONS)?))
        })
        .collect()
    }

    /// Nearest Photos to a CLIP query vector (text search).
    pub fn search_images(&self, query: &[f32], limit: usize) -> Result<Vec<(PhotoId, f64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT rowid, distance FROM vec_image_embeddings WHERE embedding MATCH ? AND k = ?",
        )?;
        let rows = stmt.query_map(
            params![bytemuck::cast_slice::<f32, u8>(query), i64::try_from(limit)?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        rows.collect::<rusqlite::Result<_>>().context("failed to search images")
    }

    /// Counts for the inventory page.
    pub fn count_by(&self, column: &str) -> Result<Vec<(String, i64)>> {
        anyhow::ensure!(
            ["source", "source_folder", "format", "camera_model", "taken_at_source", "chapter"]
                .contains(&column),
            "not a countable column"
        );
        let conn = self.conn();
        let sql = format!(
            "SELECT COALESCE({column}, '—'), COUNT(*) FROM photos WHERE missing = 0 GROUP BY 1 ORDER BY 2 DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>().context("failed to count photos")
    }

    /// Exact duplicates: groups of Photos with the same content hash.
    pub fn exact_duplicates(&self) -> Result<Vec<Vec<PhotoId>>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT GROUP_CONCAT(id) FROM photos WHERE missing = 0 AND blake3_hash IS NOT NULL GROUP BY blake3_hash HAVING COUNT(*) > 1",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(row?.split(',').filter_map(|id| id.parse().ok()).collect()))
            .collect()
    }
}
