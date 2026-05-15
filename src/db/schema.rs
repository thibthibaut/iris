use anyhow::{Context, Result};
use rusqlite::Connection;

pub const FACE_EMBEDDING_DIMENSIONS: usize = 512;

pub fn initialize(conn: &Connection, embedding_dimensions: usize) -> Result<()> {
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
"#,
    )?;

    drop_obsolete_timestamp_columns(conn)?;
    add_missing_geo_columns(conn)?;
    add_missing_face_columns(conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_photos_face_status ON photos(face_status)",
        [],
    )?;

    ensure_embedding_tables(conn, embedding_dimensions)?;

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

fn ensure_embedding_tables(conn: &Connection, embedding_dimensions: usize) -> Result<()> {
    let configured_dimension = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'embedding_dimensions'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|value| value.parse::<usize>().ok());

    if configured_dimension != Some(embedding_dimensions) {
        conn.execute_batch(
            r#"
DROP TABLE IF EXISTS vec_image_embeddings;
DROP TABLE IF EXISTS vec_ocr_text_embeddings;
"#,
        )?;
    }

    let image_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_image_embeddings USING vec0(embedding float[{embedding_dimensions}])"
    );
    conn.execute(&image_sql, [])
        .context("failed to initialize sqlite-vec image table")?;

    let text_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_ocr_text_embeddings USING vec0(embedding float[{embedding_dimensions}])"
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
INSERT INTO app_meta(key, value) VALUES ('embedding_dimensions', ?)
ON CONFLICT(key) DO UPDATE SET value = excluded.value
"#,
        [embedding_dimensions.to_string()],
    )?;

    Ok(())
}
