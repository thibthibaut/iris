mod album;
mod faces;
mod photos;
mod schema;

use std::{path::Path, sync::Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

pub use album::{ExportRecord, JobRow, OverrideRow, StageRun};
pub use faces::{FaceRow, NewFace, SavedCluster};
pub use photos::{IngestResult, PhotoFilter, PhotoRecord, QualityResult, ScannedPhoto};
pub use schema::{CLIP_EMBEDDING_DIMENSIONS, FACE_EMBEDDING_DIMENSIONS};

/// One SQLite connection. Open one per thread (jobs, web requests); WAL lets them overlap.
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        schema::initialize(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn encode_f32_vec(values: &[f32]) -> Vec<u8> {
    bytemuck::cast_slice(values).to_vec()
}

fn decode_f32_vec(bytes: &[u8], dimensions: usize) -> Result<Vec<f32>> {
    let expected_len = dimensions * std::mem::size_of::<f32>();
    anyhow::ensure!(
        bytes.len() == expected_len,
        "embedding byte length mismatch: expected {expected_len}, got {}",
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

    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| unsafe {
        let f: unsafe extern "C" fn() = sqlite_vec::sqlite3_vec_init;
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            unsafe extern "C" fn(),
            SqliteExtensionEntry,
        >(f)));
    });
}
