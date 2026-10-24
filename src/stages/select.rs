use std::collections::HashMap;

use anyhow::Result;
use iris_core::decisions::Decisions;

use crate::{
    album::{album_input, page_numbers, preview},
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
    state::AlbumState,
};

/// Chooses the Shots and lays out every Spread around the Locked ones, then renders the
/// previews.
pub struct Select;

impl Stage for Select {
    fn name(&self) -> &'static str {
        "select"
    }

    fn title(&self) -> &'static str {
        "Build the album"
    }

    fn description(&self) -> &'static str {
        "Chooses the Shots, lays out the Spreads (keeping your Locked ones) and renders the previews."
    }

    fn version(&self) -> i64 {
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &["chapters", "shots", "cluster", "quality"]
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn decisions_key(&self, decisions: &Decisions) -> String {
        // Everything the selection reads from the user's decisions.
        let mut pins: Vec<_> = decisions.pins.iter().collect();
        let mut bans: Vec<_> = decisions.bans.iter().collect();
        let mut heroes: Vec<_> = decisions.heroes.iter().collect();
        let mut moves: Vec<_> = decisions.chapter_moves.iter().collect();
        let mut spreads: Vec<_> = decisions.chapter_spreads.iter().collect();
        pins.sort();
        bans.sort();
        heroes.sort();
        moves.sort();
        spreads.sort();
        format!(
            "{}|{pins:?}|{bans:?}|{heroes:?}|{moves:?}|{spreads:?}|{:?}|{:?}",
            serde_json::to_string(&decisions.settings).unwrap_or_default(),
            decisions.people,
            decisions.locked.keys().collect::<Vec<_>>()
        )
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        sink.message("choosing Shots");
        let state = AlbumState::load(&ctx.db)?;
        let embeddings: HashMap<i64, Vec<f32>> = ctx.db.image_embeddings()?.into_iter().collect();
        let input = album_input(&state, &embeddings);
        let proposal = iris_core::album::build(&input, &state.decisions);
        ctx.db.put_blob("proposal", &proposal)?;

        let album = crate::album::current(&ctx.db, &state)?.unwrap_or(proposal);
        let numbers = page_numbers(&album.spreads);
        sink.start(album.spreads.len());
        sink.message("rendering previews");
        for spread in &album.spreads {
            cancel.check()?;
            preview(&ctx.config, &state, spread, numbers.get(&spread.uid).copied().unwrap_or_default())?;
            sink.inc(1);
        }
        let placed: usize = album.spreads.iter().map(|s| s.photos.len()).sum();
        sink.message(&format!("{} Spreads, {placed} Shots", album.spreads.len()));
        Ok(())
    }
}
