//! Editing one Spread: every change re-lays out the pages it touches. The caller records
//! the result as a Locked Spread.

use std::collections::HashMap;

use crate::{
    layout::{LayoutShot, layout_page, layout_spread, page_options, template_size},
    model::{ChapterKey, Crop, PAGE_W, PhotoId, Proposal, Spread, TemplateKey},
};

#[derive(Debug, Clone)]
pub enum Edit {
    Swap { a: PhotoId, b: PhotoId },
    Place { old: PhotoId, new: PhotoId },
    Remove { photo: PhotoId },
    /// Adds a Shot to a page (0 = left, 1 = right).
    Add { photo: PhotoId, page: usize },
    SetTemplate { page: usize, template: TemplateKey },
    Relayout,
    Crop { photo: PhotoId, crop: Crop },
    SetTitle { title: bool },
}

/// The Photos of a Spread in reading order, split into (left, right).
pub fn pages(spread: &Spread) -> (Vec<PhotoId>, Vec<PhotoId>) {
    let left = spread.photos.iter().filter(|p| p.rect.x < PAGE_W).map(|p| p.photo).collect();
    let right = spread.photos.iter().filter(|p| p.rect.x >= PAGE_W).map(|p| p.photo).collect();
    (left, right)
}

fn lay(spread: &Spread, left: &[PhotoId], right: &[PhotoId], templates: [TemplateKey; 2], shots: &HashMap<PhotoId, LayoutShot>) -> Option<Spread> {
    let get = |ids: &[PhotoId]| -> Option<Vec<LayoutShot>> { ids.iter().map(|id| shots.get(id).cloned()).collect() };
    let (left_shots, right_shots) = (get(left)?, get(right)?);
    let mut templates = templates;
    for (page, (ids, page_shots)) in [(left, &left_shots), (right, &right_shots)].into_iter().enumerate() {
        if ids.is_empty() {
            templates[page] = TemplateKey::default();
        } else if template_size(&templates[page]) != ids.len() || layout_page(page_shots, &templates[page], page == 1, false).is_none() {
            templates[page] = page_options(page_shots, page == 1, spread.title && page == 0).into_iter().next()?.0;
        }
    }
    let mut photos = Vec::new();
    if !left.is_empty() {
        photos.extend(layout_page(&left_shots, &templates[0], false, spread.title)?);
    }
    if !right.is_empty() {
        photos.extend(layout_page(&right_shots, &templates[1], true, false)?);
    }
    // Keep the user's crops of Photos that stay in a Slot of the same shape.
    for placed in &mut photos {
        if let Some(old) = spread.photos.iter().find(|p| p.photo == placed.photo)
            && (old.rect.aspect() - placed.rect.aspect()).abs() < 0.01
        {
            placed.crop = old.crop;
        }
    }
    Some(Spread { templates, photos, ..spread.clone() })
}

/// Applies one edit; `None` when it does not make sense for this Spread.
pub fn apply(spread: &Spread, edit: &Edit, shots: &HashMap<PhotoId, LayoutShot>) -> Option<Spread> {
    let (mut left, mut right) = pages(spread);
    let templates = spread.templates.clone();
    match edit {
        Edit::Swap { a, b } => {
            for id in left.iter_mut().chain(right.iter_mut()) {
                if id == a {
                    *id = *b;
                } else if id == b {
                    *id = *a;
                }
            }
            lay(spread, &left, &right, templates, shots)
        }
        Edit::Place { old, new } => {
            let slot = left.iter_mut().chain(right.iter_mut()).find(|id| *id == old)?;
            *slot = *new;
            lay(spread, &left, &right, templates, shots)
        }
        Edit::Remove { photo } => {
            left.retain(|id| id != photo);
            right.retain(|id| id != photo);
            lay(spread, &left, &right, templates, shots)
        }
        Edit::Add { photo, page } => {
            if *page == 0 { left.push(*photo) } else { right.push(*photo) }
            lay(spread, &left, &right, templates, shots)
        }
        Edit::SetTemplate { page, template } => {
            let mut templates = templates;
            *templates.get_mut(*page)? = template.clone();
            lay(spread, &left, &right, templates, shots)
        }
        Edit::Relayout => {
            let all: Vec<LayoutShot> = left.iter().chain(&right).map(|id| shots.get(id).cloned()).collect::<Option<_>>()?;
            let layout = layout_spread(&all, spread.title)?;
            Some(Spread { templates: layout.templates, photos: layout.photos, ..spread.clone() })
        }
        Edit::Crop { photo, crop } => {
            let mut edited = spread.clone();
            let placed = edited.photos.iter_mut().find(|p| p.photo == *photo)?;
            placed.crop = *crop;
            Some(edited)
        }
        Edit::SetTitle { title } => {
            let edited = Spread { title: *title, ..spread.clone() };
            lay(&edited, &left, &right, templates, shots)
        }
    }
}

/// The album as shown: the generated Proposal with each Locked Spread in place of the one
/// it was made from, ordered by position inside its Chapter.
pub fn apply_locks(proposal: &Proposal, locked: &std::collections::BTreeMap<u64, Spread>, chapter_order: &[ChapterKey]) -> Proposal {
    let mut spreads: Vec<Spread> = proposal
        .spreads
        .iter()
        .map(|spread| locked.get(&spread.uid).cloned().unwrap_or_else(|| spread.clone()))
        .collect();
    for spread in locked.values() {
        if !spreads.iter().any(|s| s.uid == spread.uid) {
            spreads.push(spread.clone());
        }
    }
    let chapter_index = |key: &ChapterKey| chapter_order.iter().position(|k| k == key).unwrap_or(usize::MAX);
    // Stable: unlocked Spreads keep their relative order around the Locked ones.
    let mut indexed: Vec<(usize, f64, Spread)> = spreads
        .into_iter()
        .enumerate()
        .map(|(i, spread)| {
            let at = if spread.locked { f64::from(spread.position) - 0.5 } else { i as f64 };
            (chapter_index(&spread.chapter), at, spread)
        })
        .collect();
    // Re-number unlocked positions inside each Chapter before mixing in Locked positions.
    let mut position_in_chapter: HashMap<usize, f64> = HashMap::new();
    for (chapter, at, spread) in &mut indexed {
        if !spread.locked {
            let next = position_in_chapter.entry(*chapter).or_insert(0.0);
            *at = *next;
            *next += 1.0;
        }
    }
    indexed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    Proposal { spreads: indexed.into_iter().map(|(_, _, s)| s).collect(), ..proposal.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::layout_page, model::{SpreadKind, PAGE_H, PAGE_W}};

    #[test]
    fn selecting_a_hero_template_on_a_locked_spread_caps_the_cropped_slot() {
        let shot = LayoutShot { photo: 9, aspect: 2.0, max_print_mm: 300.0, ..LayoutShot::default() };
        let photos = layout_page(&[shot.clone()], "rows:1", true, false).unwrap();
        let spread = Spread {
            uid: 1,
            chapter: "mairie_couple".into(),
            kind: SpreadKind::Core,
            group: None,
            templates: [String::new(), "rows:1".into()],
            photos,
            title: false,
            locked: true,
            position: 0,
        };
        let shots = HashMap::from([(shot.photo, shot.clone())]);

        let edited = apply(&spread, &Edit::SetTemplate { page: 1, template: "hero".into() }, &shots).unwrap();
        let placed = edited.photos.iter().find(|p| p.photo == shot.photo).unwrap();
        let source_long = shot.aspect.max(1.0);
        let ppi_x = 250.0 * shot.max_print_mm * shot.aspect * placed.crop.w / source_long / placed.rect.w;
        let ppi_y = 250.0 * shot.max_print_mm * placed.crop.h / source_long / placed.rect.h;

        assert!(edited.locked, "the user's locked state should survive a template change");
        assert_eq!(edited.templates[1], "hero");
        assert!(placed.rect.w < PAGE_W && placed.rect.h < PAGE_H, "Hero did not shrink around its cropped image: {:?}", placed.rect);
        assert!(ppi_x >= 249.999 && ppi_y >= 249.999, "cropped Hero prints at {} ppi", ppi_x.min(ppi_y));
    }
}
