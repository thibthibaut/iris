mod schema;

use std::{cell::RefCell, collections::HashMap, path::Path};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::db::schema::{CLIP_EMBEDDING_DIMENSIONS, FACE_EMBEDDING_DIMENSIONS};
use crate::models::{
    FaceDetection, GeoCandidate, GeoLocation, ImageQuality, NewPhoto, OcrResult, Photo,
    PhotoMetadata,
};

#[derive(Debug, Clone)]
pub struct PhotoRow {
    pub id: i64,
    pub path: String,
    pub file_size: i64,
    pub modified_at_unix: i64,
    pub missing: bool,
    pub quality_score: Option<f64>,
    pub ocr_status: String,
    pub has_image_embedding: bool,
    pub has_ocr_text_embedding: bool,
    pub geo_label: Option<String>,
    pub face_count: i64,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: i64,
    pub path: String,
    pub taken_at: Option<String>,
    pub camera_model: Option<String>,
    pub geo_label: Option<String>,
    pub quality_score: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct FaceEmbeddingRow {
    pub face_id: i64,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct FaceClusterRunParams {
    pub min_cluster_size: usize,
}

#[derive(Debug, Clone)]
pub struct FaceClusterAssignmentInput {
    pub face_id: i64,
    pub distance_to_centroid: f64,
}

#[derive(Debug, Clone)]
pub struct FaceClusterInput {
    pub cluster_label: i32,
    pub centroid: Vec<f32>,
    pub representative_face_id: i64,
    pub assignments: Vec<FaceClusterAssignmentInput>,
}

#[derive(Debug, Clone, Copy)]
pub struct FaceClusterSaveSummary {
    pub created_persons: usize,
    pub assigned_faces: usize,
}

pub struct Database {
    conn: RefCell<Connection>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::initialize(&conn)?;
        Ok(Self {
            conn: RefCell::new(conn),
        })
    }

    pub fn begin_scan(&self) -> Result<i64> {
        let now = chrono::Utc::now().timestamp();
        self.conn.borrow().execute(
            "INSERT INTO scan_runs(started_at_unix) VALUES (?)",
            params![now],
        )?;
        Ok(self.conn.borrow().last_insert_rowid())
    }

    pub fn finish_scan(&self, scan_id: i64) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.borrow().execute(
            "UPDATE scan_runs SET finished_at_unix = ? WHERE id = ?",
            params![now, scan_id],
        )?;
        Ok(())
    }

    pub fn photo_file_state(&self, path: &str) -> Result<Option<(i64, i64, i64)>> {
        self.conn
            .borrow()
            .query_row(
                "SELECT id, file_size, modified_at_unix FROM photos WHERE path = ?",
                params![path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("failed to read photo file state")
    }

    pub fn upsert_photo(&self, input: &NewPhoto, scan_id: i64) -> Result<i64> {
        self.conn.borrow().execute(
            r#"
INSERT INTO photos(path, blake3_hash, file_size, modified_at_unix, missing, last_seen_scan_id, last_scanned_at_unix)
VALUES (?, ?, ?, ?, 0, ?, strftime('%s','now'))
ON CONFLICT(path) DO UPDATE SET
  blake3_hash = excluded.blake3_hash,
  file_size = excluded.file_size,
  modified_at_unix = excluded.modified_at_unix,
  missing = 0,
  last_seen_scan_id = excluded.last_seen_scan_id,
  last_scanned_at_unix = excluded.last_scanned_at_unix,
  taken_at = NULL,
  gps_lat = NULL,
  gps_lon = NULL,
  geo_city = NULL,
  geo_region = NULL,
  geo_country = NULL,
  geo_country_code = NULL,
  geo_label = NULL,
  camera_model = NULL,
  orientation = NULL,
  width = NULL,
  height = NULL,
  blur_score = NULL,
  exposure_score = NULL,
  screenshot_score = NULL,
  quality_score = NULL,
  face_status = 'pending',
  ocr_status = 'pending',
  ocr_raw_text = NULL,
  ocr_cleaned_text = NULL,
  ocr_confidence = NULL
"#,
            params![
                input.path,
                input.blake3_hash,
                input.file_size,
                input.modified_at_unix,
                scan_id
            ],
        )?;
        let photo_id = self.photo_id_by_path(&input.path)?;
        self.delete_embeddings(photo_id)?;
        self.delete_faces_for_photo(photo_id)?;
        Ok(photo_id)
    }

    pub fn mark_seen_unchanged(&self, photo_id: i64, scan_id: i64) -> Result<()> {
        self.conn.borrow().execute(
            r#"
UPDATE photos
SET missing = 0, last_seen_scan_id = ?, last_scanned_at_unix = strftime('%s','now')
WHERE id = ?
"#,
            params![scan_id, photo_id],
        )?;
        Ok(())
    }

    pub fn mark_missing_not_seen(&self, scan_id: i64) -> Result<usize> {
        let changed = self.conn.borrow().execute(
            "UPDATE photos SET missing = 1 WHERE last_seen_scan_id IS NULL OR last_seen_scan_id <> ?",
            params![scan_id],
        )?;
        Ok(changed)
    }

    pub fn photos_missing_metadata(&self, limit: Option<usize>) -> Result<Vec<Photo>> {
        self.query_photos(
            "SELECT id, blake3_hash, path, width, height FROM photos WHERE missing = 0 AND (width IS NULL OR height IS NULL) ORDER BY id",
            limit,
        )
    }

    pub fn photos_missing_quality(&self, limit: Option<usize>) -> Result<Vec<Photo>> {
        self.query_photos(
            "SELECT id, blake3_hash, path, width, height FROM photos WHERE missing = 0 AND quality_score IS NULL ORDER BY id",
            limit,
        )
    }

    pub fn photos_missing_image_embedding(&self, limit: Option<usize>) -> Result<Vec<Photo>> {
        self.query_photos(
            "SELECT id, blake3_hash, path, width, height FROM photos WHERE missing = 0 AND NOT EXISTS (SELECT 1 FROM vec_image_embeddings WHERE rowid = photos.id) ORDER BY id",
            limit,
        )
    }

    pub fn photos_pending_ocr(&self, limit: Option<usize>) -> Result<Vec<Photo>> {
        self.query_photos(
            "SELECT id, blake3_hash, path, width, height FROM photos WHERE missing = 0 AND ocr_status = 'pending' ORDER BY id",
            limit,
        )
    }

    pub fn photos_pending_faces(&self, limit: Option<usize>) -> Result<Vec<Photo>> {
        self.query_photos(
            "SELECT id, blake3_hash, path, width, height FROM photos WHERE missing = 0 AND face_status = 'pending' ORDER BY id",
            limit,
        )
    }

    pub fn photos_missing_geo(&self, limit: Option<usize>) -> Result<Vec<GeoCandidate>> {
        let conn = self.conn.borrow();
        let sql = with_optional_limit(
            r#"
SELECT id, gps_lat, gps_lon
FROM photos
WHERE missing = 0
  AND gps_lat IS NOT NULL
  AND gps_lon IS NOT NULL
  AND geo_label IS NULL
ORDER BY id
"#,
            limit,
        )?;
        let mut stmt = conn.prepare(&sql.sql)?;
        let rows = stmt.query_map(sql.params(), |row| {
            Ok(GeoCandidate {
                id: row.get(0)?,
                gps_lat: row.get(1)?,
                gps_lon: row.get(2)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read geo candidates")
    }

    pub fn photos_missing_ocr_text_embedding(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<(Photo, String)>> {
        let conn = self.conn.borrow();
        let sql = with_optional_limit(
            r#"
SELECT id, blake3_hash, path, width, height, ocr_cleaned_text
FROM photos
WHERE missing = 0
  AND ocr_status = 'done'
  AND ocr_cleaned_text IS NOT NULL
  AND ocr_cleaned_text <> ''
  AND NOT EXISTS (SELECT 1 FROM vec_ocr_text_embeddings WHERE rowid = photos.id)
ORDER BY id
"#,
            limit,
        )?;
        let mut stmt = conn.prepare(&sql.sql)?;
        let rows = stmt.query_map(sql.params(), |row| {
            Ok((
                Photo {
                    id: row.get(0)?,
                    blake3_hash: row.get(1)?,
                    path: row.get(2)?,
                    width: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    height: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                },
                row.get(5)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read OCR text embedding candidates")
    }

    pub fn save_metadata(&self, photo_id: i64, metadata: &PhotoMetadata) -> Result<()> {
        self.conn.borrow().execute(
            r#"
UPDATE photos SET
  taken_at = ?, gps_lat = ?, gps_lon = ?, camera_model = ?, orientation = ?, width = ?, height = ?
WHERE id = ?
"#,
            params![
                metadata.taken_at,
                metadata.gps_lat,
                metadata.gps_lon,
                metadata.camera_model,
                metadata.orientation,
                metadata.width.map(i64::from),
                metadata.height.map(i64::from),
                photo_id
            ],
        )?;
        Ok(())
    }

    pub fn save_geo_location(&self, photo_id: i64, location: &GeoLocation) -> Result<()> {
        self.conn.borrow().execute(
            r#"
UPDATE photos SET
  geo_city = ?, geo_region = ?, geo_country = ?, geo_country_code = ?, geo_label = ?
WHERE id = ?
"#,
            params![
                location.city,
                location.region,
                location.country,
                location.country_code,
                location.label,
                photo_id
            ],
        )?;
        Ok(())
    }

    pub fn save_quality(&self, photo_id: i64, quality: &ImageQuality) -> Result<()> {
        self.conn.borrow().execute(
            r#"
UPDATE photos SET
  blur_score = ?, exposure_score = ?, screenshot_score = ?, quality_score = ?
WHERE id = ?
"#,
            params![
                quality.tenengrad_sharpness,
                quality.exposure_score,
                quality.screenshot_score,
                quality.quality_score,
                photo_id
            ],
        )?;
        Ok(())
    }

    pub fn save_ocr_result(&self, photo_id: i64, result: &OcrResult) -> Result<()> {
        self.conn.borrow().execute(
            r#"
UPDATE photos SET ocr_status = 'done', ocr_raw_text = ?, ocr_cleaned_text = ?, ocr_confidence = ?
WHERE id = ?
"#,
            params![
                result.raw_text,
                result.cleaned_text,
                result.confidence,
                photo_id
            ],
        )?;
        Ok(())
    }

    pub fn mark_ocr_skipped(&self, photo_id: i64) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE photos SET ocr_status = 'skipped' WHERE id = ?",
            params![photo_id],
        )?;
        Ok(())
    }

    pub fn mark_ocr_failed(&self, photo_id: i64) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE photos SET ocr_status = 'failed' WHERE id = ?",
            params![photo_id],
        )?;
        Ok(())
    }

    pub fn save_image_embedding(&self, photo_id: i64, embedding: &[f32]) -> Result<()> {
        self.save_vec_embedding(
            "vec_image_embeddings",
            photo_id,
            embedding,
            CLIP_EMBEDDING_DIMENSIONS,
        )
    }

    pub fn save_ocr_text_embedding(&self, photo_id: i64, embedding: &[f32]) -> Result<()> {
        self.save_vec_embedding(
            "vec_ocr_text_embeddings",
            photo_id,
            embedding,
            CLIP_EMBEDDING_DIMENSIONS,
        )
    }

    pub fn replace_faces(&self, photo_id: i64, faces: &[FaceDetection]) -> Result<()> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;

        delete_faces_for_photo_tx(&tx, photo_id)?;

        for face in faces {
            anyhow::ensure!(
                face.embedding.len() == FACE_EMBEDDING_DIMENSIONS,
                "face embedding dimension mismatch: expected {}, got {}",
                FACE_EMBEDDING_DIMENSIONS,
                face.embedding.len()
            );

            tx.execute(
                r#"
INSERT INTO faces(
  photo_id, face_index, bbox_x1, bbox_y1, bbox_x2, bbox_y2,
  detection_score, landmarks_json, gender, age
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
"#,
                params![
                    photo_id,
                    face.face_index,
                    face.bbox_x1,
                    face.bbox_y1,
                    face.bbox_x2,
                    face.bbox_y2,
                    face.detection_score,
                    face.landmarks_json,
                    face.gender,
                    face.age.map(i64::from),
                ],
            )?;
            let face_id = tx.last_insert_rowid();
            let bytes = bytemuck::cast_slice(face.embedding.as_slice());
            tx.execute(
                "INSERT OR REPLACE INTO vec_face_embeddings(rowid, embedding) VALUES (?, ?)",
                params![face_id, bytes],
            )?;
        }

        tx.execute(
            "UPDATE photos SET face_status = 'done' WHERE id = ?",
            params![photo_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn mark_faces_failed(&self, photo_id: i64) -> Result<()> {
        self.conn.borrow().execute(
            "UPDATE photos SET face_status = 'failed' WHERE id = ?",
            params![photo_id],
        )?;
        Ok(())
    }

    pub fn face_embeddings_for_clustering(&self) -> Result<Vec<FaceEmbeddingRow>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            r#"
SELECT faces.id, vec_face_embeddings.embedding
FROM faces
JOIN photos ON photos.id = faces.photo_id
JOIN vec_face_embeddings ON vec_face_embeddings.rowid = faces.id
WHERE photos.missing = 0
ORDER BY faces.id
"#,
        )?;
        let rows = stmt.query_map([], |row| {
            let face_id = row.get(0)?;
            let bytes = row.get::<_, Vec<u8>>(1)?;
            Ok((face_id, bytes))
        })?;

        rows.map(|row| {
            let (face_id, bytes) = row?;
            Ok(FaceEmbeddingRow {
                face_id,
                embedding: decode_f32_vec(&bytes, FACE_EMBEDDING_DIMENSIONS)?,
            })
        })
        .collect()
    }

    pub fn start_face_cluster_run(&self, params: &FaceClusterRunParams) -> Result<i64> {
        let now = chrono::Utc::now().timestamp();
        self.conn.borrow().execute(
            r#"
INSERT INTO face_cluster_runs(
  started_at_unix, status, algorithm, min_cluster_size, min_samples, match_threshold
)
VALUES (?, 'running', 'hdbscan', ?, ?, ?)
"#,
            params![
                now,
                i64::try_from(params.min_cluster_size).context("min_cluster_size is too large")?,
                0_i64,
                0.0_f64,
            ],
        )?;
        Ok(self.conn.borrow().last_insert_rowid())
    }

    pub fn clear_face_clusters(&self) -> Result<()> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM face_person_assignments", [])?;
        tx.execute("DELETE FROM persons", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_empty_face_cluster_run(
        &self,
        run_id: i64,
        status: &str,
        input_faces: usize,
        noise_faces: usize,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.borrow().execute(
            r#"
UPDATE face_cluster_runs
SET finished_at_unix = ?, status = ?, input_faces = ?, clustered_faces = 0,
    noise_faces = ?, cluster_count = 0
WHERE id = ?
"#,
            params![
                now,
                status,
                i64::try_from(input_faces).context("input face count is too large")?,
                i64::try_from(noise_faces).context("noise face count is too large")?,
                run_id,
            ],
        )?;
        Ok(())
    }

    pub fn fail_face_cluster_run(&self, run_id: i64, error: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.borrow().execute(
            r#"
UPDATE face_cluster_runs
SET finished_at_unix = ?, status = 'failed', error = ?
WHERE id = ?
"#,
            params![now, error, run_id],
        )?;
        Ok(())
    }

    pub fn save_face_clusters(
        &self,
        run_id: i64,
        input_faces: usize,
        noise_faces: usize,
        clusters: &[FaceClusterInput],
    ) -> Result<FaceClusterSaveSummary> {
        anyhow::ensure!(!clusters.is_empty(), "clusters must not be empty");
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().timestamp();
        let mut created_persons = 0;
        let mut assigned_faces = 0;

        tx.execute("DELETE FROM face_person_assignments", [])?;
        tx.execute("DELETE FROM persons", [])?;

        for cluster in clusters {
            anyhow::ensure!(
                cluster.centroid.len() == FACE_EMBEDDING_DIMENSIONS,
                "face cluster centroid dimension mismatch: expected {}, got {}",
                FACE_EMBEDDING_DIMENSIONS,
                cluster.centroid.len()
            );

            let centroid_bytes = encode_f32_vec(&cluster.centroid);
            tx.execute(
                r#"
INSERT INTO persons(
  created_at_unix, updated_at_unix, active, representative_face_id, centroid,
  face_count, last_seen_cluster_run_id
)
VALUES (?, ?, 1, ?, ?, ?, ?)
"#,
                params![
                    now,
                    now,
                    cluster.representative_face_id,
                    centroid_bytes,
                    i64::try_from(cluster.assignments.len()).context("face count is too large")?,
                    run_id,
                ],
            )?;
            created_persons += 1;
            let person_id = tx.last_insert_rowid();

            for assignment in &cluster.assignments {
                tx.execute(
                    r#"
INSERT INTO face_person_assignments(
  face_id, person_id, cluster_run_id, cluster_label, distance_to_centroid, assigned_at_unix
)
VALUES (?, ?, ?, ?, ?, ?)
"#,
                    params![
                        assignment.face_id,
                        person_id,
                        run_id,
                        cluster.cluster_label,
                        assignment.distance_to_centroid,
                        now,
                    ],
                )?;
                assigned_faces += 1;
            }
        }

        tx.execute(
            r#"
UPDATE face_cluster_runs
SET finished_at_unix = ?, status = 'done', input_faces = ?, clustered_faces = ?,
    noise_faces = ?, cluster_count = ?
WHERE id = ?
"#,
            params![
                now,
                i64::try_from(input_faces).context("input face count is too large")?,
                i64::try_from(assigned_faces).context("assigned face count is too large")?,
                i64::try_from(noise_faces).context("noise face count is too large")?,
                i64::try_from(clusters.len()).context("cluster count is too large")?,
                run_id,
            ],
        )?;

        tx.commit()?;
        Ok(FaceClusterSaveSummary {
            created_persons,
            assigned_faces,
        })
    }

    pub fn stats(&self) -> Result<(i64, i64)> {
        self.conn
            .borrow()
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN missing THEN 1 ELSE 0 END) FROM photos",
                [],
                |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
            )
            .context("failed to read stats")
    }

    pub fn indexed_photo_count(&self) -> Result<i64> {
        self.conn
            .borrow()
            .query_row("SELECT COUNT(*) FROM photos WHERE missing = 0", [], |row| {
                row.get(0)
            })
            .context("failed to read indexed photo count")
    }

    pub fn photo_path(&self, photo_id: i64) -> Result<Option<String>> {
        self.conn
            .borrow()
            .query_row(
                "SELECT path FROM photos WHERE id = ? AND missing = 0",
                params![photo_id],
                |row| row.get(0),
            )
            .optional()
            .context("failed to read photo path")
    }

    pub fn photo_detail(&self, photo_id: i64) -> Result<Option<SearchResult>> {
        self.conn
            .borrow()
            .query_row(
                r#"
SELECT id, path, taken_at, camera_model, geo_label, quality_score, width, height
FROM photos
WHERE id = ? AND missing = 0
"#,
                params![photo_id],
                |row| {
                    Ok(SearchResult {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        taken_at: row.get(2)?,
                        camera_model: row.get(3)?,
                        geo_label: row.get(4)?,
                        quality_score: row.get(5)?,
                        width: row.get(6)?,
                        height: row.get(7)?,
                        score: 0.0,
                    })
                },
            )
            .optional()
            .context("failed to read photo detail")
    }

    pub fn heic_photos(&self, limit: Option<usize>) -> Result<Vec<(i64, String)>> {
        let conn = self.conn.borrow();
        let sql = with_optional_limit(
            r#"
SELECT id, path
FROM photos
WHERE missing = 0
  AND lower(path) LIKE '%.heic'
ORDER BY id
"#,
            limit,
        )?;
        let mut stmt = conn.prepare(&sql.sql)?;
        let rows = stmt.query_map(sql.params(), |row| Ok((row.get(0)?, row.get(1)?)))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read HEIC photo candidates")
    }

    pub fn search_photos(
        &self,
        query: &str,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        anyhow::ensure!(limit > 0, "search limit must be > 0");
        anyhow::ensure!(
            query_embedding.len() == CLIP_EMBEDDING_DIMENSIONS,
            "query embedding dimension mismatch: expected {}, got {}",
            CLIP_EMBEDDING_DIMENSIONS,
            query_embedding.len()
        );

        let mut scores = HashMap::new();
        self.add_vector_scores(
            "vec_image_embeddings",
            query_embedding,
            limit,
            0.65,
            &mut scores,
        )?;
        self.add_vector_scores(
            "vec_ocr_text_embeddings",
            query_embedding,
            limit,
            0.55,
            &mut scores,
        )?;
        self.add_text_scores(query, limit, &mut scores)?;

        let mut ranked = scores.into_iter().collect::<Vec<_>>();
        ranked.sort_by(|(_, a), (_, b)| b.total_cmp(a));

        ranked
            .into_iter()
            .take(limit)
            .map(|(photo_id, score)| self.search_result(photo_id, score))
            .collect()
    }

    pub fn photo_rows(&self, limit: usize) -> Result<Vec<PhotoRow>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            r#"
SELECT
  photos.id,
  photos.path,
  photos.file_size,
  photos.modified_at_unix,
  photos.missing,
  photos.quality_score,
  photos.ocr_status,
  photos.geo_label,
  EXISTS(SELECT 1 FROM vec_image_embeddings WHERE rowid = photos.id),
  EXISTS(SELECT 1 FROM vec_ocr_text_embeddings WHERE rowid = photos.id),
  (SELECT COUNT(*) FROM faces WHERE photo_id = photos.id)
FROM photos
ORDER BY photos.id
LIMIT ?
"#,
        )?;

        let rows = stmt.query_map(
            params![i64::try_from(limit).context("limit is too large")?],
            |row| {
                Ok(PhotoRow {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    file_size: row.get(2)?,
                    modified_at_unix: row.get(3)?,
                    missing: row.get::<_, i64>(4)? != 0,
                    quality_score: row.get(5)?,
                    ocr_status: row.get(6)?,
                    geo_label: row.get(7)?,
                    has_image_embedding: row.get::<_, i64>(8)? != 0,
                    has_ocr_text_embedding: row.get::<_, i64>(9)? != 0,
                    face_count: row.get(10)?,
                })
            },
        )?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to inspect photo rows")
    }

    fn photo_id_by_path(&self, path: &str) -> Result<i64> {
        self.conn
            .borrow()
            .query_row(
                "SELECT id FROM photos WHERE path = ?",
                params![path],
                |row| row.get(0),
            )
            .context("failed to read photo id")
    }

    fn query_photos(&self, sql: &str, limit: Option<usize>) -> Result<Vec<Photo>> {
        let sql = with_optional_limit(sql, limit)?;
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql.sql)?;
        let rows = stmt.query_map(sql.params(), |row| {
            Ok(Photo {
                id: row.get(0)?,
                blake3_hash: row.get(1)?,
                path: row.get(2)?,
                width: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                height: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to query photos")
    }

    fn save_vec_embedding(
        &self,
        table: &str,
        rowid: i64,
        embedding: &[f32],
        dimensions: usize,
    ) -> Result<()> {
        anyhow::ensure!(
            embedding.len() == dimensions,
            "embedding dimension mismatch: expected {}, got {}",
            dimensions,
            embedding.len()
        );
        let bytes = bytemuck::cast_slice(embedding);
        let sql = format!("INSERT OR REPLACE INTO {table}(rowid, embedding) VALUES (?, ?)");
        self.conn.borrow().execute(&sql, params![rowid, bytes])?;
        Ok(())
    }

    fn add_vector_scores(
        &self,
        table: &str,
        query_embedding: &[f32],
        limit: usize,
        weight: f64,
        scores: &mut HashMap<i64, f64>,
    ) -> Result<()> {
        let bytes = bytemuck::cast_slice(query_embedding);
        let sql = format!("SELECT rowid, distance FROM {table} WHERE embedding MATCH ? AND k = ?");
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![bytes, i64::try_from(limit).context("limit is too large")?],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)),
        )?;

        for row in rows {
            let (photo_id, distance) = row?;
            let score = weight / (1.0 + distance.max(0.0));
            *scores.entry(photo_id).or_insert(0.0) += score;
        }

        Ok(())
    }

    fn add_text_scores(
        &self,
        query: &str,
        limit: usize,
        scores: &mut HashMap<i64, f64>,
    ) -> Result<()> {
        let pattern = format!("%{}%", escape_like(query));
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            r#"
SELECT id
FROM photos
WHERE missing = 0
  AND (
    path LIKE ? ESCAPE '\'
    OR camera_model LIKE ? ESCAPE '\'
    OR taken_at LIKE ? ESCAPE '\'
    OR geo_city LIKE ? ESCAPE '\'
    OR geo_region LIKE ? ESCAPE '\'
    OR geo_country LIKE ? ESCAPE '\'
    OR geo_country_code LIKE ? ESCAPE '\'
    OR geo_label LIKE ? ESCAPE '\'
    OR ocr_cleaned_text LIKE ? ESCAPE '\'
  )
ORDER BY id
LIMIT ?
"#,
        )?;
        let rows = stmt.query_map(
            params![
                pattern,
                pattern,
                pattern,
                pattern,
                pattern,
                pattern,
                pattern,
                pattern,
                pattern,
                i64::try_from(limit).context("limit is too large")?,
            ],
            |row| row.get::<_, i64>(0),
        )?;

        for row in rows {
            *scores.entry(row?).or_insert(0.0) += 0.35;
        }

        Ok(())
    }

    fn search_result(&self, photo_id: i64, score: f64) -> Result<SearchResult> {
        self.conn
            .borrow()
            .query_row(
                r#"
SELECT id, path, taken_at, camera_model, geo_label, quality_score, width, height
FROM photos
WHERE id = ? AND missing = 0
"#,
                params![photo_id],
                |row| {
                    Ok(SearchResult {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        taken_at: row.get(2)?,
                        camera_model: row.get(3)?,
                        geo_label: row.get(4)?,
                        quality_score: row.get(5)?,
                        width: row.get(6)?,
                        height: row.get(7)?,
                        score,
                    })
                },
            )
            .context("failed to read search result")
    }

    fn delete_embeddings(&self, photo_id: i64) -> Result<()> {
        self.conn.borrow().execute(
            "DELETE FROM vec_image_embeddings WHERE rowid = ?",
            params![photo_id],
        )?;
        self.conn.borrow().execute(
            "DELETE FROM vec_ocr_text_embeddings WHERE rowid = ?",
            params![photo_id],
        )?;
        Ok(())
    }

    fn delete_faces_for_photo(&self, photo_id: i64) -> Result<()> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        delete_faces_for_photo_tx(&tx, photo_id)?;
        tx.commit()?;
        Ok(())
    }
}

fn escape_like(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' | '_' | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn delete_faces_for_photo_tx(tx: &rusqlite::Transaction<'_>, photo_id: i64) -> Result<()> {
    let face_ids = {
        let mut stmt = tx.prepare("SELECT id FROM faces WHERE photo_id = ?")?;
        let rows = stmt.query_map(params![photo_id], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    for face_id in face_ids {
        tx.execute(
            "DELETE FROM vec_face_embeddings WHERE rowid = ?",
            params![face_id],
        )?;
    }

    tx.execute("DELETE FROM faces WHERE photo_id = ?", params![photo_id])?;
    Ok(())
}

fn encode_f32_vec(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(values));
    for value in values {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    bytes
}

fn decode_f32_vec(bytes: &[u8], dimensions: usize) -> Result<Vec<f32>> {
    let expected_len = dimensions * std::mem::size_of::<f32>();
    anyhow::ensure!(
        bytes.len() == expected_len,
        "embedding byte length mismatch: expected {}, got {}",
        expected_len,
        bytes.len()
    );

    Ok(bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| f32::from_ne_bytes(chunk.try_into().expect("chunk length is checked")))
        .collect())
}

fn register_sqlite_vec() {
    type SqliteExtensionEntry = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3,
        *mut *const std::os::raw::c_char,
        *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;

    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            SqliteExtensionEntry,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    }
}

struct OptionalLimitSql {
    sql: String,
    limit: Option<i64>,
}

impl OptionalLimitSql {
    fn params(&self) -> rusqlite::ParamsFromIter<Option<i64>> {
        params_from_iter(self.limit)
    }
}

fn with_optional_limit(sql: &str, limit: Option<usize>) -> Result<OptionalLimitSql> {
    let limit = limit
        .map(|limit| i64::try_from(limit).context("limit is too large"))
        .transpose()?;

    let sql = if limit.is_some() {
        format!("{sql} LIMIT ?")
    } else {
        sql.to_string()
    };

    Ok(OptionalLimitSql { sql, limit })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_missing_without_deleting() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        db.upsert_photo(
            &NewPhoto {
                path: "/tmp/a.jpg".into(),
                blake3_hash: "abc".into(),
                file_size: 1,
                modified_at_unix: 2,
            },
            scan_id,
        )
        .unwrap();
        db.finish_scan(scan_id).unwrap();

        let scan_id = db.begin_scan().unwrap();
        assert_eq!(db.mark_missing_not_seen(scan_id).unwrap(), 1);
        let (total, missing) = db.stats().unwrap();
        assert_eq!((total, missing), (1, 1));
    }

    #[test]
    fn rejects_wrong_embedding_dimension() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let photo_id = db
            .upsert_photo(
                &NewPhoto {
                    path: "/tmp/a.jpg".into(),
                    blake3_hash: "abc".into(),
                    file_size: 1,
                    modified_at_unix: 2,
                },
                scan_id,
            )
            .unwrap();

        assert!(
            db.save_image_embedding(photo_id, &[1.0; CLIP_EMBEDDING_DIMENSIONS - 1])
                .is_err()
        );
        assert!(
            db.save_image_embedding(photo_id, &[1.0; CLIP_EMBEDDING_DIMENSIONS])
                .is_ok()
        );
    }

    #[test]
    fn image_embedding_presence_controls_candidates() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let photo_id = db
            .upsert_photo(
                &NewPhoto {
                    path: "/tmp/a.jpg".into(),
                    blake3_hash: "abc".into(),
                    file_size: 1,
                    modified_at_unix: 2,
                },
                scan_id,
            )
            .unwrap();

        assert_eq!(
            db.photos_missing_image_embedding(Some(10)).unwrap().len(),
            1
        );
        db.save_image_embedding(photo_id, &[1.0; CLIP_EMBEDDING_DIMENSIONS])
            .unwrap();
        assert_eq!(
            db.photos_missing_image_embedding(Some(10)).unwrap().len(),
            0
        );
    }

    #[test]
    fn changed_file_resets_derived_data_and_embeddings() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let photo_id = db
            .upsert_photo(
                &NewPhoto {
                    path: "/tmp/a.jpg".into(),
                    blake3_hash: "abc".into(),
                    file_size: 1,
                    modified_at_unix: 2,
                },
                scan_id,
            )
            .unwrap();

        db.save_metadata(
            photo_id,
            &PhotoMetadata {
                taken_at: None,
                gps_lat: None,
                gps_lon: None,
                camera_model: None,
                orientation: None,
                width: Some(10),
                height: Some(10),
            },
        )
        .unwrap();
        db.save_quality(
            photo_id,
            &ImageQuality {
                tenengrad_sharpness: 1.0,
                exposure_score: 1.0,
                screenshot_score: 0.0,
                quality_score: 1.0,
            },
        )
        .unwrap();
        db.save_image_embedding(photo_id, &[1.0; CLIP_EMBEDDING_DIMENSIONS])
            .unwrap();
        db.replace_faces(photo_id, &[face_detection(0)]).unwrap();

        db.upsert_photo(
            &NewPhoto {
                path: "/tmp/a.jpg".into(),
                blake3_hash: "def".into(),
                file_size: 2,
                modified_at_unix: 3,
            },
            scan_id,
        )
        .unwrap();

        assert_eq!(db.photos_missing_metadata(Some(10)).unwrap().len(), 1);
        assert_eq!(db.photos_missing_quality(Some(10)).unwrap().len(), 1);
        assert_eq!(
            db.photos_missing_image_embedding(Some(10)).unwrap().len(),
            1
        );
        assert_eq!(db.photos_pending_faces(Some(10)).unwrap().len(), 1);
        assert_eq!(db.photo_rows(10).unwrap()[0].face_count, 0);
    }

    #[test]
    fn geo_presence_controls_candidates() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let photo_id = db
            .upsert_photo(
                &NewPhoto {
                    path: "/tmp/a.jpg".into(),
                    blake3_hash: "abc".into(),
                    file_size: 1,
                    modified_at_unix: 2,
                },
                scan_id,
            )
            .unwrap();
        db.save_metadata(
            photo_id,
            &PhotoMetadata {
                taken_at: None,
                gps_lat: Some(48.8566),
                gps_lon: Some(2.3522),
                camera_model: None,
                orientation: None,
                width: Some(10),
                height: Some(10),
            },
        )
        .unwrap();

        assert_eq!(db.photos_missing_geo(Some(10)).unwrap().len(), 1);
        db.save_geo_location(
            photo_id,
            &GeoLocation {
                city: Some("Paris".into()),
                region: Some("Ile-de-France".into()),
                country: Some("France".into()),
                country_code: Some("FR".into()),
                label: Some("Paris, Ile-de-France, France".into()),
            },
        )
        .unwrap();
        assert_eq!(db.photos_missing_geo(Some(10)).unwrap().len(), 0);
    }

    #[test]
    fn face_status_allows_zero_face_photos() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let photo_id = insert_test_photo(&db, scan_id);

        assert_eq!(db.photos_pending_faces(Some(10)).unwrap().len(), 1);
        db.replace_faces(photo_id, &[]).unwrap();
        assert_eq!(db.photos_pending_faces(Some(10)).unwrap().len(), 0);
        assert_eq!(db.photo_rows(10).unwrap()[0].face_count, 0);
    }

    #[test]
    fn search_combines_embeddings_and_geo_text() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let vector_photo_id = insert_test_photo(&db, scan_id);
        let geo_photo_id = db
            .upsert_photo(
                &NewPhoto {
                    path: "/tmp/marseille.jpg".into(),
                    blake3_hash: "def".into(),
                    file_size: 1,
                    modified_at_unix: 2,
                },
                scan_id,
            )
            .unwrap();

        db.save_image_embedding(vector_photo_id, &[1.0; CLIP_EMBEDDING_DIMENSIONS])
            .unwrap();
        db.save_geo_location(
            geo_photo_id,
            &GeoLocation {
                city: Some("Marseille".into()),
                region: Some("Provence-Alpes-Cote d'Azur".into()),
                country: Some("France".into()),
                country_code: Some("FR".into()),
                label: Some("Marseille, Provence-Alpes-Cote d'Azur, France".into()),
            },
        )
        .unwrap();

        let results = db
            .search_photos("Marseille", &[1.0; CLIP_EMBEDDING_DIMENSIONS], 10)
            .unwrap();
        let ids = results.iter().map(|result| result.id).collect::<Vec<_>>();

        assert!(ids.contains(&vector_photo_id));
        assert!(ids.contains(&geo_photo_id));
    }

    #[test]
    fn stores_multiple_faces_per_photo() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let photo_id = insert_test_photo(&db, scan_id);

        db.replace_faces(photo_id, &[face_detection(0), face_detection(1)])
            .unwrap();
        assert_eq!(db.photos_pending_faces(Some(10)).unwrap().len(), 0);
        assert_eq!(db.photo_rows(10).unwrap()[0].face_count, 2);
    }

    #[test]
    fn rejects_wrong_face_embedding_dimension() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        let scan_id = db.begin_scan().unwrap();
        let photo_id = insert_test_photo(&db, scan_id);
        let mut face = face_detection(0);
        face.embedding = vec![0.0; FACE_EMBEDDING_DIMENSIONS - 1];

        assert!(db.replace_faces(photo_id, &[face]).is_err());
        assert_eq!(db.photos_pending_faces(Some(10)).unwrap().len(), 1);
    }

    fn insert_test_photo(db: &Database, scan_id: i64) -> i64 {
        db.upsert_photo(
            &NewPhoto {
                path: format!("/tmp/{scan_id}.jpg"),
                blake3_hash: "abc".into(),
                file_size: 1,
                modified_at_unix: 2,
            },
            scan_id,
        )
        .unwrap()
    }

    fn face_detection(face_index: i64) -> FaceDetection {
        FaceDetection {
            face_index,
            bbox_x1: 0.1,
            bbox_y1: 0.2,
            bbox_x2: 0.3,
            bbox_y2: 0.4,
            detection_score: 0.9,
            landmarks_json: Some("[]".into()),
            gender: Some("Male".into()),
            age: Some(30),
            embedding: vec![0.0; FACE_EMBEDDING_DIMENSIONS],
        }
    }
}
