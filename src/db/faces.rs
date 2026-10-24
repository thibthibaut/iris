use anyhow::{Context, Result};
use iris_core::model::{FaceId, PhotoId};
use rusqlite::params;

use super::{Database, FACE_EMBEDDING_DIMENSIONS, decode_f32_vec, encode_f32_vec};

#[derive(Debug, Clone)]
pub struct NewFace {
    pub bbox: [f64; 4],
    pub detection_score: f64,
    pub landmarks_json: Option<String>,
    pub sharpness: f64,
    pub embedding: Vec<f32>,
}

/// One face with its Photo's dimensions and its cluster.
#[derive(Debug, Clone)]
pub struct FaceRow {
    pub id: FaceId,
    pub photo_id: PhotoId,
    /// x1, y1, x2, y2 in the upright original's pixels.
    pub bbox: [f64; 4],
    pub detection_score: f64,
    pub sharpness: Option<f64>,
    pub photo_width: i64,
    pub photo_height: i64,
    pub cluster: Option<i64>,
    pub soft: bool,
}

impl FaceRow {
    pub fn height_frac(&self) -> f64 {
        (self.bbox[3] - self.bbox[1]) / self.photo_height.max(1) as f64
    }

    /// Bbox as fractions of the Photo's width and height.
    pub fn normalized(&self) -> [f64; 4] {
        let (w, h) = (self.photo_width.max(1) as f64, self.photo_height.max(1) as f64);
        [self.bbox[0] / w, self.bbox[1] / h, self.bbox[2] / w, self.bbox[3] / h]
    }
}

#[derive(Debug, Clone)]
pub struct SavedCluster {
    pub centroid: Vec<f32>,
    pub representative_face: FaceId,
    /// (face, distance to centroid, soft)
    pub members: Vec<(FaceId, f64, bool)>,
}

impl Database {
    pub fn replace_faces(&self, photo_id: PhotoId, faces: &[NewFace]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let old: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM faces WHERE photo_id = ?")?;
            let rows = stmt.query_map(params![photo_id], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for face_id in old {
            tx.execute("DELETE FROM vec_face_embeddings WHERE rowid = ?", params![face_id])?;
        }
        tx.execute("DELETE FROM faces WHERE photo_id = ?", params![photo_id])?;
        for (index, face) in faces.iter().enumerate() {
            anyhow::ensure!(face.embedding.len() == FACE_EMBEDDING_DIMENSIONS, "face embedding dimension mismatch");
            tx.execute(
                r#"
INSERT INTO faces(photo_id, face_index, bbox_x1, bbox_y1, bbox_x2, bbox_y2, detection_score,
  landmarks_json, sharpness)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
"#,
                params![
                    photo_id,
                    index as i64,
                    face.bbox[0],
                    face.bbox[1],
                    face.bbox[2],
                    face.bbox[3],
                    face.detection_score,
                    face.landmarks_json,
                    face.sharpness,
                ],
            )?;
            let face_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO vec_face_embeddings(rowid, embedding) VALUES (?, ?)",
                params![face_id, encode_f32_vec(&face.embedding)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn face_embeddings(&self) -> Result<Vec<(FaceId, Vec<f32>)>> {
        let conn = self.conn();
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
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        rows.map(|row| {
            let (id, bytes) = row?;
            Ok((id, decode_f32_vec(&bytes, FACE_EMBEDDING_DIMENSIONS)?))
        })
        .collect()
    }

    /// Replaces all clusters with the result of a new cluster run.
    pub fn save_clusters(&self, clusters: &[SavedCluster]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM face_clusters", [])?;
        tx.execute("DELETE FROM clusters", [])?;
        for cluster in clusters {
            tx.execute(
                "INSERT INTO clusters(representative_face_id, centroid) VALUES (?, ?)",
                params![cluster.representative_face, encode_f32_vec(&cluster.centroid)],
            )?;
            let cluster_id = tx.last_insert_rowid();
            for (face, distance, soft) in &cluster.members {
                tx.execute(
                    "INSERT INTO face_clusters(face_id, cluster_id, distance, soft) VALUES (?, ?, ?, ?)",
                    params![face, cluster_id, distance, soft],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn faces(&self) -> Result<Vec<FaceRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            r#"
SELECT f.id, f.photo_id, f.bbox_x1, f.bbox_y1, f.bbox_x2, f.bbox_y2, f.detection_score,
  f.sharpness, p.width, p.height, fc.cluster_id, COALESCE(fc.soft, 0)
FROM faces f
JOIN photos p ON p.id = f.photo_id
LEFT JOIN face_clusters fc ON fc.face_id = f.id
WHERE p.missing = 0
ORDER BY f.id
"#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FaceRow {
                id: row.get(0)?,
                photo_id: row.get(1)?,
                bbox: [row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?],
                detection_score: row.get(6)?,
                sharpness: row.get(7)?,
                photo_width: row.get::<_, Option<i64>>(8)?.unwrap_or(1),
                photo_height: row.get::<_, Option<i64>>(9)?.unwrap_or(1),
                cluster: row.get(10)?,
                soft: row.get(11)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().context("failed to read faces")
    }
}
