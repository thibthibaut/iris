use std::collections::BTreeMap;

use anyhow::{Context, Result};
use hdbscan::{Hdbscan, HdbscanHyperParams};
use innr::l2_distance;
use tracing::info;

use crate::{
    app::AppContext,
    db::{FaceClusterAssignmentInput, FaceClusterInput, FaceClusterRunParams, FaceEmbeddingRow},
    processors::progress,
    traits::BatchProcessor,
};

pub struct ClusterFacesProcessor;

impl BatchProcessor for ClusterFacesProcessor {
    fn name(&self) -> &'static str {
        "cluster-faces"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let params = FaceClusterRunParams {
            min_cluster_size: ctx.config.faces.min_cluster_size,
        };
        let faces = ctx.db.face_embeddings_for_clustering()?;
        let input_faces = faces.len();
        let run_id = ctx.db.start_face_cluster_run(&params)?;

        let result = self.run_inner(ctx, run_id, &params, faces);
        if let Err(error) = &result {
            ctx.db
                .fail_face_cluster_run(run_id, &format!("{error:#}"))?;
        }
        result.map(|_| {
            info!(
                processor = self.name(),
                run_id, input_faces, "face clustering completed"
            );
        })
    }
}

impl ClusterFacesProcessor {
    fn run_inner(
        &self,
        ctx: &AppContext,
        run_id: i64,
        params: &FaceClusterRunParams,
        faces: Vec<FaceEmbeddingRow>,
    ) -> Result<()> {
        let input_faces = faces.len();
        if input_faces < params.min_cluster_size {
            ctx.db.clear_face_clusters()?;
            ctx.db.finish_empty_face_cluster_run(
                run_id,
                "not_enough_faces",
                input_faces,
                input_faces,
            )?;
            info!(
                processor = self.name(),
                input_faces, "not enough faces to cluster"
            );
            return Ok(());
        }

        let data = faces
            .iter()
            .map(|face| face.embedding.clone())
            .collect::<Vec<_>>();
        let hyper_params = HdbscanHyperParams::builder()
            .min_cluster_size(params.min_cluster_size)
            .build();
        let clusterer = Hdbscan::new(&data, hyper_params);

        let pb = progress::spinner("hdbscan");
        let labels = clusterer
            .cluster_par()
            .context("HDBSCAN clustering failed")?;
        pb.finish_and_clear();

        anyhow::ensure!(
            labels.len() == faces.len(),
            "HDBSCAN returned {} labels for {} faces",
            labels.len(),
            faces.len()
        );

        let mut grouped = BTreeMap::<i32, Vec<usize>>::new();
        let mut noise_faces = 0;
        for (index, label) in labels.iter().copied().enumerate() {
            if label < 0 {
                noise_faces += 1;
            } else {
                grouped.entry(label).or_default().push(index);
            }
        }

        if grouped.is_empty() {
            ctx.db.clear_face_clusters()?;
            ctx.db.finish_empty_face_cluster_run(
                run_id,
                "no_clusters",
                input_faces,
                noise_faces,
            )?;
            info!(
                processor = self.name(),
                input_faces, noise_faces, "face clustering produced no clusters"
            );
            return Ok(());
        }

        let clusters = grouped
            .into_iter()
            .map(|(cluster_label, indices)| build_cluster(cluster_label, &faces, &indices))
            .collect::<Result<Vec<_>>>()?;

        let summary = ctx
            .db
            .save_face_clusters(run_id, input_faces, noise_faces, &clusters)?;
        info!(
            processor = self.name(),
            input_faces,
            noise_faces,
            cluster_count = clusters.len(),
            created_persons = summary.created_persons,
            assigned_faces = summary.assigned_faces,
            "face clusters saved"
        );
        Ok(())
    }
}

fn build_cluster(
    cluster_label: i32,
    faces: &[FaceEmbeddingRow],
    indices: &[usize],
) -> Result<FaceClusterInput> {
    anyhow::ensure!(!indices.is_empty(), "cluster must not be empty");
    let mut centroid = vec![0.0; faces[indices[0]].embedding.len()];
    for &index in indices {
        for (sum, value) in centroid.iter_mut().zip(&faces[index].embedding) {
            *sum += *value;
        }
    }
    let scale = 1.0 / indices.len() as f32;
    for value in &mut centroid {
        *value *= scale;
    }

    let mut representative_face_id = faces[indices[0]].face_id;
    let mut best_distance = f64::INFINITY;
    let mut assignments = Vec::with_capacity(indices.len());
    for &index in indices {
        let distance = f64::from(l2_distance(&faces[index].embedding, &centroid));
        if distance < best_distance {
            best_distance = distance;
            representative_face_id = faces[index].face_id;
        }
        assignments.push(FaceClusterAssignmentInput {
            face_id: faces[index].face_id,
            distance_to_centroid: distance,
        });
    }

    Ok(FaceClusterInput {
        cluster_label,
        centroid,
        representative_face_id,
        assignments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_centroid_uses_closest_representative() {
        let faces = vec![
            FaceEmbeddingRow {
                face_id: 1,
                embedding: vec![1.0, 0.0],
            },
            FaceEmbeddingRow {
                face_id: 2,
                embedding: vec![0.8, 0.6],
            },
        ];

        let cluster = build_cluster(7, &faces, &[0, 1]).unwrap();

        assert_eq!(cluster.cluster_label, 7);
        assert_eq!(cluster.assignments.len(), 2);
        assert!(cluster.representative_face_id == 1 || cluster.representative_face_id == 2);
    }
}
