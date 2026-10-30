use anyhow::{Context, Result};
use iris_core::model::Override;
use rusqlite::{OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use super::{Database, now_ms};

#[derive(Debug, Clone)]
pub struct OverrideRow {
    pub id: i64,
    pub ts_ms: i64,
    pub label: String,
    pub undone: bool,
    pub item: Override,
}

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub stage: String,
    pub status: String,
    pub queued_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub done: i64,
    pub total: i64,
    pub failed: i64,
    pub message: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StageRun {
    pub version: i64,
    pub settings_hash: String,
    pub finished_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct ExportRecord {
    pub id: i64,
    pub edition: String,
    pub version: i64,
    pub path: String,
    pub created_at_ms: i64,
    pub manifest: serde_json::Value,
}

impl Database {
    // ---- Overrides -------------------------------------------------------------------

    /// Records a decision. A new decision drops the redo stack (undone Overrides).
    pub fn add_override(&self, item: &Override, label: &str) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM overrides WHERE undone = 1", [])?;
        let kind = serde_json::to_value(item)?
            .get("kind")
            .and_then(|kind| kind.as_str().map(str::to_string))
            .unwrap_or_default();
        tx.execute(
            "INSERT INTO overrides(ts_ms, kind, payload_json, label) VALUES (?, ?, ?, ?)",
            params![now_ms(), kind, serde_json::to_string(item)?, label],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn active_overrides(&self) -> Result<Vec<Override>> {
        Ok(self
            .override_rows(None)?
            .into_iter()
            .filter(|row| !row.undone)
            .map(|row| row.item)
            .collect())
    }

    /// All Overrides in order, or the latest `limit` (newest first).
    pub fn override_rows(&self, limit: Option<usize>) -> Result<Vec<OverrideRow>> {
        let conn = self.conn();
        let sql = match limit {
            Some(limit) => format!("SELECT id, ts_ms, label, undone, payload_json FROM overrides ORDER BY id DESC LIMIT {limit}"),
            None => "SELECT id, ts_ms, label, undone, payload_json FROM overrides ORDER BY id".into(),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, String>(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, ts_ms, label, undone, payload): (i64, i64, String, bool, String) = row?;
            // Overrides from an older payload shape are skipped rather than breaking replay.
            if let Ok(item) = serde_json::from_str(&payload) {
                out.push(OverrideRow { id, ts_ms, label, undone, item });
            }
        }
        Ok(out)
    }

    /// Undoes the latest active Override; returns its label.
    pub fn undo(&self) -> Result<Option<String>> {
        let conn = self.conn();
        let latest: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, label FROM overrides WHERE undone = 0 ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, _)) = &latest {
            conn.execute("UPDATE overrides SET undone = 1 WHERE id = ?", params![id])?;
        }
        Ok(latest.map(|(_, label)| label))
    }

    /// Redoes the earliest undone Override; returns its label.
    pub fn redo(&self) -> Result<Option<String>> {
        let conn = self.conn();
        let earliest: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, label FROM overrides WHERE undone = 1 ORDER BY id ASC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, _)) = &earliest {
            conn.execute("UPDATE overrides SET undone = 0 WHERE id = ?", params![id])?;
        }
        Ok(earliest.map(|(_, label)| label))
    }

    /// A number that changes whenever the Overrides change (for caches).
    pub fn overrides_revision(&self) -> Result<String> {
        self.conn()
            .query_row(
                "SELECT COUNT(*) || ':' || COALESCE(MAX(id), 0) || ':' || COALESCE(SUM(undone), 0) FROM overrides",
                [],
                |row| row.get(0),
            )
            .context("failed to read overrides revision")
    }

    // ---- Stage runs ------------------------------------------------------------------

    pub fn record_stage_run(&self, stage: &str, version: i64, settings_hash: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO stage_runs(stage, version, settings_hash, finished_at_ms) VALUES (?, ?, ?, ?)",
            params![stage, version, settings_hash, now_ms()],
        )?;
        Ok(())
    }

    pub fn stage_run(&self, stage: &str) -> Result<Option<StageRun>> {
        self.conn()
            .query_row(
                "SELECT version, settings_hash, finished_at_ms FROM stage_runs WHERE stage = ?",
                params![stage],
                |row| Ok(StageRun { version: row.get(0)?, settings_hash: row.get(1)?, finished_at_ms: row.get(2)? }),
            )
            .optional()
            .context("failed to read stage run")
    }

    // ---- Jobs ------------------------------------------------------------------------

    pub fn create_job(&self, stage: &str) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO jobs(stage, status, queued_at_ms) VALUES (?, 'queued', ?)",
            params![stage, now_ms()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn start_job(&self, job_id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET status = 'running', started_at_ms = ? WHERE id = ?",
            params![now_ms(), job_id],
        )?;
        Ok(())
    }

    pub fn update_job(&self, job_id: i64, done: i64, total: i64, failed: i64, message: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET done = ?, total = ?, failed = ?, message = COALESCE(?, message) WHERE id = ?",
            params![done, total, failed, message, job_id],
        )?;
        Ok(())
    }

    pub fn finish_job(&self, job_id: i64, status: &str, error: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET status = ?, finished_at_ms = ?, error = ? WHERE id = ?",
            params![status, now_ms(), error, job_id],
        )?;
        Ok(())
    }

    /// Jobs left running by a previous process are marked as interrupted.
    pub fn fail_stale_jobs(&self) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET status = 'cancelled', error = 'interrupted (app restarted)', finished_at_ms = ? WHERE status IN ('queued', 'running')",
            params![now_ms()],
        )?;
        Ok(())
    }

    pub fn add_job_item(&self, job_id: i64, photo_id: i64, reason: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO job_items(job_id, photo_id, reason) VALUES (?, ?, ?)",
            params![job_id, photo_id, reason],
        )?;
        Ok(())
    }

    pub fn jobs(&self, limit: usize) -> Result<Vec<JobRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, stage, status, queued_at_ms, started_at_ms, finished_at_ms, done, total, failed, message, error FROM jobs ORDER BY id DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok(JobRow {
                id: row.get(0)?,
                stage: row.get(1)?,
                status: row.get(2)?,
                queued_at_ms: row.get(3)?,
                started_at_ms: row.get(4)?,
                finished_at_ms: row.get(5)?,
                done: row.get(6)?,
                total: row.get(7)?,
                failed: row.get(8)?,
                message: row.get(9)?,
                error: row.get(10)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().context("failed to read jobs")
    }

    pub fn last_job(&self, stage: &str) -> Result<Option<JobRow>> {
        Ok(self.jobs(200)?.into_iter().find(|job| job.stage == stage))
    }

    // ---- Blobs -----------------------------------------------------------------------

    pub fn put_blob<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO blobs(key, value_json, updated_at_ms) VALUES (?, ?, ?)",
            params![key, serde_json::to_string(value)?, now_ms()],
        )?;
        Ok(())
    }

    pub fn blob<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = self
            .conn()
            .query_row("SELECT value_json FROM blobs WHERE key = ?", params![key], |row| row.get(0))
            .optional()?;
        Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
    }

    // ---- Exports ---------------------------------------------------------------------

    pub fn next_export_version(&self, edition: &str) -> Result<i64> {
        self.conn()
            .query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM exports WHERE edition = ?",
                params![edition],
                |row| row.get(0),
            )
            .context("failed to read export version")
    }

    pub fn add_export(&self, edition: &str, version: i64, path: &str, manifest: &serde_json::Value) -> Result<()> {
        self.conn().execute(
            "INSERT INTO exports(edition, version, path, created_at_ms, manifest_json) VALUES (?, ?, ?, ?, ?)",
            params![edition, version, path, now_ms(), manifest.to_string()],
        )?;
        Ok(())
    }

    /// Mark or unmark an Export for retention. The flag lives in the manifest so older
    /// databases need no schema change.
    pub fn set_export_keep(&self, id: i64, keep: bool) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row("SELECT manifest_json FROM exports WHERE id = ?", params![id], |row| row.get(0))
            .optional()?;
        let Some(raw) = raw else {
            tx.commit()?;
            return Ok(false);
        };
        let mut manifest: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        if !manifest.is_object() {
            manifest = serde_json::json!({});
        }
        manifest["keep_version"] = serde_json::Value::Bool(keep);
        tx.execute("UPDATE exports SET manifest_json = ? WHERE id = ?", params![manifest.to_string(), id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Remove old, unkept Export history entries and return their file paths for cleanup.
    /// The newest version is always retained, whether or not it has been marked to keep.
    pub fn prune_exports(&self, edition: &str, latest_version: i64) -> Result<Vec<String>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let rows = {
            let mut stmt = tx.prepare("SELECT id, path, manifest_json FROM exports WHERE edition = ? AND version < ?")?;
            let rows = stmt.query_map(params![edition, latest_version], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut paths = Vec::new();
        for (id, path, raw_manifest) in rows {
            let keep = serde_json::from_str::<serde_json::Value>(&raw_manifest)
                .ok()
                .and_then(|manifest| manifest.get("keep_version").and_then(serde_json::Value::as_bool))
                .unwrap_or(false);
            if !keep {
                tx.execute("DELETE FROM exports WHERE id = ?", params![id])?;
                paths.push(path);
            }
        }
        tx.commit()?;
        Ok(paths)
    }

    pub fn exports(&self) -> Result<Vec<ExportRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, edition, version, path, created_at_ms, manifest_json FROM exports ORDER BY edition, version DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ExportRecord {
                id: row.get(0)?,
                edition: row.get(1)?,
                version: row.get(2)?,
                path: row.get(3)?,
                created_at_ms: row.get(4)?,
                manifest: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().context("failed to read exports")
    }

    pub fn delete_export(&self, id: i64) -> Result<Option<String>> {
        let conn = self.conn();
        let path: Option<String> = conn
            .query_row("SELECT path FROM exports WHERE id = ?", params![id], |row| row.get(0))
            .optional()?;
        conn.execute("DELETE FROM exports WHERE id = ?", params![id])?;
        Ok(path)
    }

    // ---- VLM cache -------------------------------------------------------------------

    pub fn vlm_cached(&self, hash: &str, task: &str, prompt_version: i64) -> Result<Option<serde_json::Value>> {
        let raw: Option<String> = self
            .conn()
            .query_row(
                "SELECT response_json FROM vlm_cache WHERE blake3_hash = ? AND task = ? AND prompt_version = ?",
                params![hash, task, prompt_version],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
    }

    /// Every cached answer of a task: (content hash, response).
    pub fn vlm_answers(&self, task: &str) -> Result<Vec<(String, serde_json::Value)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT blake3_hash, response_json FROM vlm_cache WHERE task = ?")?;
        let rows = stmt.query_map(params![task], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (hash, raw) = row?;
            Ok((hash, serde_json::from_str(&raw).unwrap_or_default()))
        })
        .collect()
    }

    pub fn put_vlm(&self, hash: &str, task: &str, prompt_version: i64, response: &serde_json::Value) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO vlm_cache(blake3_hash, task, prompt_version, response_json) VALUES (?, ?, ?, ?)",
            params![hash, task, prompt_version, response.to_string()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod export_tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn pruning_removes_unkept_history_and_keeps_requested_versions() {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.add_export("ours", 1, "/exports/v1.pdf", &serde_json::json!({ "keep_version": false })).unwrap();
        db.add_export("ours", 2, "/exports/v2.pdf", &serde_json::json!({ "keep_version": false })).unwrap();
        db.add_export("ours", 3, "/exports/v3.pdf", &serde_json::json!({ "keep_version": false })).unwrap();
        let version_one = db.exports().unwrap().into_iter().find(|row| row.version == 1).unwrap().id;

        assert!(db.set_export_keep(version_one, true).unwrap());
        let removed_paths = db.prune_exports("ours", 3).unwrap();
        let remaining: Vec<i64> = db.exports().unwrap().into_iter().map(|row| row.version).collect();

        assert_eq!(removed_paths, vec!["/exports/v2.pdf"]);
        assert_eq!(remaining, vec![3, 1]);
    }
}
