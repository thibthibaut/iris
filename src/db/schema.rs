use anyhow::{Context, Result};
use rusqlite::Connection;

pub const CLIP_EMBEDDING_DIMENSIONS: usize = 768;
pub const FACE_EMBEDDING_DIMENSIONS: usize = 512;

pub fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS scan_runs (
  id INTEGER PRIMARY KEY,
  started_at_unix INTEGER NOT NULL,
  finished_at_unix INTEGER
);

CREATE TABLE IF NOT EXISTS app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS photos (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  blake3_hash TEXT NOT NULL,
  file_size INTEGER NOT NULL,
  modified_at_unix INTEGER NOT NULL,
  missing INTEGER NOT NULL DEFAULT 0,
  last_seen_scan_id INTEGER,
  last_scanned_at_unix INTEGER,
  taken_at TEXT,
  gps_lat REAL,
  gps_lon REAL,
  geo_city TEXT,
  geo_region TEXT,
  geo_country TEXT,
  geo_country_code TEXT,
  geo_label TEXT,
  camera_model TEXT,
  orientation INTEGER,
  width INTEGER,
  height INTEGER,
  blur_score REAL,
  exposure_score REAL,
  screenshot_score REAL,
  quality_score REAL,
  face_status TEXT NOT NULL DEFAULT 'pending',
  ocr_status TEXT NOT NULL DEFAULT 'pending',
  ocr_raw_text TEXT,
  ocr_cleaned_text TEXT,
  ocr_confidence REAL
);

CREATE INDEX IF NOT EXISTS idx_photos_missing ON photos(missing);
CREATE INDEX IF NOT EXISTS idx_photos_hash ON photos(blake3_hash);
CREATE INDEX IF NOT EXISTS idx_photos_ocr_status ON photos(ocr_status);

CREATE TABLE IF NOT EXISTS faces (
  id INTEGER PRIMARY KEY,
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  face_index INTEGER NOT NULL,
  bbox_x1 REAL NOT NULL,
  bbox_y1 REAL NOT NULL,
  bbox_x2 REAL NOT NULL,
  bbox_y2 REAL NOT NULL,
  detection_score REAL NOT NULL,
  landmarks_json TEXT,
  gender TEXT,
  age INTEGER,
  UNIQUE(photo_id, face_index)
);

CREATE INDEX IF NOT EXISTS idx_faces_photo_id ON faces(photo_id);

CREATE TABLE IF NOT EXISTS persons (
  id INTEGER PRIMARY KEY,
  display_name TEXT,
  created_at_unix INTEGER NOT NULL,
  updated_at_unix INTEGER NOT NULL,
  active INTEGER NOT NULL DEFAULT 1,
  representative_face_id INTEGER REFERENCES faces(id) ON DELETE SET NULL,
  centroid BLOB NOT NULL,
  face_count INTEGER NOT NULL DEFAULT 0,
  last_seen_cluster_run_id INTEGER
);

CREATE INDEX IF NOT EXISTS idx_persons_active ON persons(active);

CREATE TABLE IF NOT EXISTS face_cluster_runs (
  id INTEGER PRIMARY KEY,
  started_at_unix INTEGER NOT NULL,
  finished_at_unix INTEGER,
  status TEXT NOT NULL,
  algorithm TEXT NOT NULL,
  min_cluster_size INTEGER NOT NULL,
  min_samples INTEGER NOT NULL,
  match_threshold REAL NOT NULL,
  input_faces INTEGER NOT NULL DEFAULT 0,
  clustered_faces INTEGER NOT NULL DEFAULT 0,
  noise_faces INTEGER NOT NULL DEFAULT 0,
  cluster_count INTEGER NOT NULL DEFAULT 0,
  error TEXT
);

CREATE TABLE IF NOT EXISTS face_person_assignments (
  face_id INTEGER PRIMARY KEY REFERENCES faces(id) ON DELETE CASCADE,
  person_id INTEGER NOT NULL REFERENCES persons(id),
  cluster_run_id INTEGER NOT NULL REFERENCES face_cluster_runs(id),
  cluster_label INTEGER NOT NULL,
  distance_to_centroid REAL,
  assigned_at_unix INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_face_person_assignments_person_id ON face_person_assignments(person_id);
CREATE INDEX IF NOT EXISTS idx_face_person_assignments_cluster_run_id ON face_person_assignments(cluster_run_id);
"#,
    )?;

    drop_obsolete_timestamp_columns(conn)?;
    add_missing_geo_columns(conn)?;
    add_missing_face_columns(conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_photos_face_status ON photos(face_status)",
        [],
    )?;

    ensure_embedding_tables(conn)?;

    Ok(())
}

fn add_missing_face_columns(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "photos", "face_status")? {
        conn.execute(
            "ALTER TABLE photos ADD COLUMN face_status TEXT NOT NULL DEFAULT 'pending'",
            [],
        )?;
    }

    Ok(())
}

fn add_missing_geo_columns(conn: &Connection) -> Result<()> {
    for (column, column_type) in [
        ("geo_city", "TEXT"),
        ("geo_region", "TEXT"),
        ("geo_country", "TEXT"),
        ("geo_country_code", "TEXT"),
        ("geo_label", "TEXT"),
    ] {
        if !column_exists(conn, "photos", column)? {
            let sql = format!("ALTER TABLE photos ADD COLUMN {column} {column_type}");
            conn.execute(&sql, [])?;
        }
    }

    Ok(())
}

fn drop_obsolete_timestamp_columns(conn: &Connection) -> Result<()> {
    for column in [
        "metadata_extracted_at_unix",
        "quality_extracted_at_unix",
        "image_embedding_done_at_unix",
        "ocr_text_embedding_done_at_unix",
    ] {
        if column_exists(conn, "photos", column)? {
            let sql = format!("ALTER TABLE photos DROP COLUMN {column}");
            conn.execute(&sql, [])?;
        }
    }

    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;

    for existing_column in columns {
        if existing_column? == column {
            return Ok(true);
        }
    }

    Ok(false)
}

fn ensure_embedding_tables(conn: &Connection) -> Result<()> {
    let configured_dimension = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'clip_embedding_dimensions'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .or_else(|| {
            conn.query_row(
                "SELECT value FROM app_meta WHERE key = 'embedding_dimensions'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
        })
        .and_then(|value| value.parse::<usize>().ok());

    if configured_dimension != Some(CLIP_EMBEDDING_DIMENSIONS) {
        conn.execute_batch(
            r#"
DROP TABLE IF EXISTS vec_image_embeddings;
DROP TABLE IF EXISTS vec_ocr_text_embeddings;
"#,
        )?;
    }

    let image_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_image_embeddings USING vec0(embedding float[{CLIP_EMBEDDING_DIMENSIONS}])"
    );
    conn.execute(&image_sql, [])
        .context("failed to initialize sqlite-vec image table")?;

    let text_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_ocr_text_embeddings USING vec0(embedding float[{CLIP_EMBEDDING_DIMENSIONS}])"
    );
    conn.execute(&text_sql, [])
        .context("failed to initialize sqlite-vec OCR text table")?;

    let face_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_face_embeddings USING vec0(embedding float[{FACE_EMBEDDING_DIMENSIONS}])"
    );
    conn.execute(&face_sql, [])
        .context("failed to initialize sqlite-vec face table")?;

    conn.execute(
        r#"
INSERT INTO app_meta(key, value) VALUES ('clip_embedding_dimensions', ?)
ON CONFLICT(key) DO UPDATE SET value = excluded.value
"#,
        [CLIP_EMBEDDING_DIMENSIONS.to_string()],
    )?;
    conn.execute(
        "DELETE FROM app_meta WHERE key = 'embedding_dimensions'",
        [],
    )?;

    Ok(())
}
