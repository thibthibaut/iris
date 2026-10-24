use std::collections::BTreeMap;

use anyhow::{Context, Result};
use hdbscan::{Hdbscan, HdbscanHyperParams};
use iris_core::decisions::Decisions;

use crate::{
    db::SavedCluster,
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
};

/// Groups faces into clusters (HDBSCAN). Faces left over are attached to the nearest
/// cluster when close enough, flagged soft for the user to confirm.
pub struct Cluster;

impl Stage for Cluster {
    fn name(&self) -> &'static str {
        "cluster"
    }

    fn title(&self) -> &'static str {
        "Group faces"
    }

    fn description(&self) -> &'static str {
        "Clusters faces into Persons. Leftover faces close to a Person are attached as 'to confirm'."
    }

    fn version(&self) -> i64 {
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &["faces"]
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn decisions_key(&self, decisions: &Decisions) -> String {
        let settings = &decisions.settings;
        format!("{}:{}:{}", settings.face_min_cluster_size, settings.face_soft_distance, settings.face_min_height_frac)
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, _cancel: &CancelToken) -> Result<()> {
        let settings = ctx.settings();
        // Background faces are too small to identify reliably and never count anyway.
        let big: std::collections::HashSet<i64> = ctx
            .db
            .faces()?
            .into_iter()
            .filter(|face| face.height_frac() >= settings.face_min_height_frac)
            .map(|face| face.id)
            .collect();
        let faces: Vec<(i64, Vec<f32>)> =
            ctx.db.face_embeddings()?.into_iter().filter(|(id, _)| big.contains(id)).collect();
        sink.start(faces.len());
        let min_size = settings.face_min_cluster_size.max(2);
        if faces.len() < min_size {
            ctx.db.save_clusters(&[])?;
            sink.message("not enough faces to cluster");
            return Ok(());
        }

        sink.message("clustering");
        let data: Vec<Vec<f32>> = faces.iter().map(|(_, embedding)| embedding.clone()).collect();
        let params = HdbscanHyperParams::builder().min_cluster_size(min_size).build();
        let labels = Hdbscan::new(&data, params).cluster_par().context("HDBSCAN failed")?;

        let mut grouped: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
        let mut noise = Vec::new();
        for (index, label) in labels.iter().copied().enumerate() {
            if label < 0 {
                noise.push(index);
            } else {
                grouped.entry(label).or_default().push(index);
            }
        }

        let mut clusters: Vec<SavedCluster> = grouped
            .into_values()
            .map(|members| {
                let centroid = centroid(members.iter().map(|&i| data[i].as_slice()));
                let mut members: Vec<(i64, f64, bool)> = members
                    .iter()
                    .map(|&i| (faces[i].0, f64::from(l2(&data[i], &centroid)), false))
                    .collect();
                members.sort_by(|a, b| a.1.total_cmp(&b.1));
                SavedCluster { representative_face: members[0].0, centroid, members }
            })
            .collect();

        let mut soft = 0;
        for index in noise {
            let nearest = clusters
                .iter()
                .enumerate()
                .map(|(c, cluster)| (c, f64::from(l2(&data[index], &cluster.centroid))))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((c, distance)) = nearest
                && distance < settings.face_soft_distance
            {
                clusters[c].members.push((faces[index].0, distance, true));
                soft += 1;
            }
        }
        ctx.db.save_clusters(&clusters)?;
        sink.inc(faces.len());
        sink.message(&format!("{} Persons, {soft} faces to confirm", clusters.len()));
        Ok(())
    }
}

fn centroid<'a>(vectors: impl Iterator<Item = &'a [f32]>) -> Vec<f32> {
    let mut sum: Vec<f32> = Vec::new();
    let mut count = 0.0;
    for vector in vectors {
        if sum.is_empty() {
            sum = vec![0.0; vector.len()];
        }
        sum.iter_mut().zip(vector).for_each(|(s, v)| *s += v);
        count += 1.0;
    }
    sum.iter_mut().for_each(|s| *s /= count);
    sum
}

fn l2(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}
