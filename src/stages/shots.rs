use std::collections::HashMap;

use anyhow::Result;
use iris_core::{
    model::Source,
    decisions::Decisions,
    shots::{ShotInput, group_shots},
};

use crate::jobs::{CancelToken, ProgressSink, Stage, StageCtx};

/// Groups copies and burst frames into Shots; the best copy represents the Shot.
pub struct Shots;

impl Stage for Shots {
    fn name(&self) -> &'static str {
        "shots"
    }

    fn title(&self) -> &'static str {
        "Find copies and bursts"
    }

    fn description(&self) -> &'static str {
        "Groups forwarded copies and burst frames into Shots, keeping the best copy of each."
    }

    fn version(&self) -> i64 {
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &["quality", "embed"]
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn decisions_key(&self, decisions: &Decisions) -> String {
        let mut splits: Vec<_> = decisions.shot_splits.iter().collect();
        splits.sort();
        format!("{}|{splits:?}", serde_json::to_string(&decisions.settings.shots).unwrap_or_default())
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, _cancel: &CancelToken) -> Result<()> {
        let photos = ctx.db.all_photos()?;
        let embeddings: HashMap<i64, Vec<f32>> = ctx.db.image_embeddings()?.into_iter().collect();
        sink.start(photos.len());
        let inputs: Vec<ShotInput> = photos
            .iter()
            .map(|photo| ShotInput {
                photo: photo.id,
                phash: photo.phash,
                embedding: embeddings.get(&photo.id).cloned(),
                time_ms: photo.taken_at_utc.or(photo.sent_at_utc),
                sequence: photo.sequence,
                photographer: photo.source == Some(Source::Photographer),
                pixels: (photo.width.unwrap_or(0) * photo.height.unwrap_or(0)) as f64,
                jpeg_quality: photo.jpeg_q_est,
            })
            .collect();
        let shots = group_shots(&inputs, &ctx.settings().shots, &ctx.decisions.shot_splits);
        // A Shot is identified by its representative's Photo id.
        let rows: Vec<(i64, i64)> = shots
            .iter()
            .flat_map(|shot| shot.members.iter().map(move |&member| (member, shot.representative)))
            .collect();
        ctx.db.save_shots(&rows)?;
        sink.inc(photos.len());
        let grouped = shots.iter().filter(|shot| shot.members.len() > 1).count();
        sink.message(&format!("{} Shots from {} photos ({grouped} with copies or bursts)", shots.len(), photos.len()));
        Ok(())
    }
}
