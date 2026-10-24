//! Timeline: one strip per Chapter; drag a Photo to another Chapter.

use askama::Template;
use axum::{extract::State, response::Html};

use super::{AppState, WebResult, blocking, render};

struct Strip {
    key: String,
    title: String,
    tiles: Vec<TimelineTile>,
    to_check: usize,
}

struct TimelineTile {
    id: i64,
    copies: usize,
    guest: bool,
    uncertain: bool,
    moved: bool,
    excluded: bool,
    hint: String,
}

#[derive(Template)]
#[template(path = "timeline.html")]
struct TimelinePage {
    nav: &'static str,
    strips: Vec<Strip>,
    total_to_check: usize,
}

pub async fn page(State(state): State<AppState>) -> WebResult<Html<String>> {
    let page = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let settings = &snapshot.decisions.settings;
        let sizes = super::photos::shot_sizes(&snapshot.photos);
        let mut strips: Vec<Strip> = settings
            .chapters
            .iter()
            .map(|c| Strip { key: c.key.clone(), title: c.title_fr.clone(), tiles: Vec::new(), to_check: 0 })
            .chain(std::iter::once(Strip { key: "none".into(), title: "Not placed yet".into(), tiles: Vec::new(), to_check: 0 }))
            .collect();
        let mut photos: Vec<_> = snapshot
            .photos
            .iter()
            .filter(|p| p.shot_id.is_none_or(|shot| shot == p.id))
            .collect();
        photos.sort_by(|a, b| a.chapter_order.unwrap_or(f64::MAX).total_cmp(&b.chapter_order.unwrap_or(f64::MAX)));
        for photo in photos {
            let chapter = snapshot.chapter_of(photo);
            let index = chapter.as_deref().and_then(|key| settings.chapter_index(key)).unwrap_or(strips.len() - 1);
            let moved = snapshot.decisions.chapter_moves.contains_key(&photo.id);
            let uncertain = !moved && photo.chapter_source.as_deref() == Some("visual") && photo.chapter_margin.is_some_and(|m| m < 0.15);
            let excluded = !matches!(
                snapshot.eligibility(photo.id),
                iris_core::decisions::Eligibility::Eligible | iris_core::decisions::Eligibility::Pinned
            );
            strips[index].to_check += usize::from(uncertain);
            strips[index].tiles.push(TimelineTile {
                id: photo.id,
                copies: photo.shot_id.and_then(|s| sizes.get(&s).copied()).unwrap_or(1),
                guest: photo.source == Some(iris_core::model::Source::Guest),
                uncertain,
                moved,
                excluded,
                hint: format!(
                    "{}{}",
                    photo.file_name,
                    photo.chapter_prob.map(|p| format!(" · {:.0} % sure", p * 100.0)).unwrap_or_default()
                ),
            });
        }
        if strips.last().is_some_and(|s| s.tiles.is_empty()) {
            strips.pop();
        }
        Ok(TimelinePage { nav: "timeline", total_to_check: strips.iter().map(|s| s.to_check).sum(), strips })
    })
    .await?;
    render(&page)
}
