use anyhow::{Context, Result};
use rusqlite::Connection;

pub const CLIP_EMBEDDING_DIMENSIONS: usize = 768;
pub const FACE_EMBEDDING_DIMENSIONS: usize = 512;

/// Fresh schema for the album. No migrations from older iris databases.
pub fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS scan_runs (
  id INTEGER PRIMARY KEY,
  started_at_unix INTEGER NOT NULL,
  finished_at_unix INTEGER
);

CREATE TABLE IF NOT EXISTS photos (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  file_name TEXT NOT NULL,
  source TEXT NOT NULL,                -- 'photographer' | 'guest'
  source_folder TEXT NOT NULL DEFAULT '',
  file_size INTEGER NOT NULL,
  modified_at_unix INTEGER NOT NULL,
  missing INTEGER NOT NULL DEFAULT 0,
  last_seen_scan_id INTEGER,
  sent_at_utc INTEGER,                 -- guest file-name send time (upper bound on capture)
  sequence INTEGER,                    -- camera file number (DSC_9562 -> 9562)
  -- ingest
  blake3_hash TEXT,
  format TEXT,
  camera_model TEXT,
  width INTEGER,                       -- after orientation
  height INTEGER,
  taken_at_utc INTEGER,
  taken_at_source TEXT NOT NULL DEFAULT 'none',
  jpeg_q_est REAL,
  -- quality
  tenengrad REAL,
  exposure_score REAL,
  screenshot_score REAL,
  phash INTEGER,
  clip_aesthetic REAL,
  clip_screenshot REAL,
  -- shots and chapters
  shot_id INTEGER,
  chapter TEXT,
  chapter_prob REAL,
  chapter_margin REAL,
  chapter_source TEXT,
  chapter_order REAL
);

CREATE INDEX IF NOT EXISTS idx_photos_missing ON photos(missing);
CREATE INDEX IF NOT EXISTS idx_photos_hash ON photos(blake3_hash);
CREATE INDEX IF NOT EXISTS idx_photos_shot ON photos(shot_id);
CREATE INDEX IF NOT EXISTS idx_photos_chapter ON photos(chapter);

-- Per-Photo progress of each per-Photo stage, at a given processor version.
CREATE TABLE IF NOT EXISTS photo_stages (
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  stage TEXT NOT NULL,
  version INTEGER NOT NULL,
  ok INTEGER NOT NULL,
  error TEXT,
  PRIMARY KEY (photo_id, stage)
);

-- Last successful run of each stage.
CREATE TABLE IF NOT EXISTS stage_runs (
  stage TEXT PRIMARY KEY,
  version INTEGER NOT NULL,
  settings_hash TEXT NOT NULL DEFAULT '',
  finished_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS faces (
  id INTEGER PRIMARY KEY,
  photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  face_index INTEGER NOT NULL,
  -- bbox in the upright original's pixel frame
  bbox_x1 REAL NOT NULL,
  bbox_y1 REAL NOT NULL,
  bbox_x2 REAL NOT NULL,
  bbox_y2 REAL NOT NULL,
  detection_score REAL NOT NULL,
  landmarks_json TEXT,
  sharpness REAL,
  UNIQUE(photo_id, face_index)
);

CREATE INDEX IF NOT EXISTS idx_faces_photo_id ON faces(photo_id);

-- Raw face clusters from the last cluster run. Persons as the user sees them are these
-- clusters with the People Overrides replayed on top (see album::people).
CREATE TABLE IF NOT EXISTS clusters (
  id INTEGER PRIMARY KEY,
  representative_face_id INTEGER REFERENCES faces(id) ON DELETE SET NULL,
  centroid BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS face_clusters (
  face_id INTEGER PRIMARY KEY REFERENCES faces(id) ON DELETE CASCADE,
  cluster_id INTEGER NOT NULL REFERENCES clusters(id) ON DELETE CASCADE,
  distance REAL NOT NULL,
  soft INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_face_clusters_cluster ON face_clusters(cluster_id);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS overrides (
  id INTEGER PRIMARY KEY,
  ts_ms INTEGER NOT NULL,
  kind TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  label TEXT NOT NULL,
  undone INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS jobs (
  id INTEGER PRIMARY KEY,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,                -- queued | running | succeeded | failed | cancelled
  queued_at_ms INTEGER NOT NULL,
  started_at_ms INTEGER,
  finished_at_ms INTEGER,
  done INTEGER NOT NULL DEFAULT 0,
  total INTEGER NOT NULL DEFAULT 0,
  failed INTEGER NOT NULL DEFAULT 0,
  message TEXT,
  error TEXT
);

CREATE TABLE IF NOT EXISTS job_items (
  job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  photo_id INTEGER NOT NULL,
  reason TEXT NOT NULL
);

-- Generated state (the Proposal) and other derived blobs.
CREATE TABLE IF NOT EXISTS blobs (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS exports (
  id INTEGER PRIMARY KEY,
  edition TEXT NOT NULL,
  version INTEGER NOT NULL,
  path TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  manifest_json TEXT NOT NULL,
  UNIQUE(edition, version)
);

CREATE TABLE IF NOT EXISTS vlm_cache (
  blake3_hash TEXT NOT NULL,
  task TEXT NOT NULL,
  prompt_version INTEGER NOT NULL,
  response_json TEXT NOT NULL,
  PRIMARY KEY (blake3_hash, task, prompt_version)
);
"#,
    )?;

    let image_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_image_embeddings USING vec0(embedding float[{CLIP_EMBEDDING_DIMENSIONS}])"
    );
    conn.execute(&image_sql, [])
        .context("failed to initialize sqlite-vec image table")?;
    let face_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_face_embeddings USING vec0(embedding float[{FACE_EMBEDDING_DIMENSIONS}])"
    );
    conn.execute(&face_sql, [])
        .context("failed to initialize sqlite-vec face table")?;

    Ok(())
}
