//! Builds the Proposal: Spread budgets, selection, Heroes, layout, Locked Spreads, Group
//! Spreads. And Editions: the master sequence minus other Groups' Spreads.

use std::collections::{HashMap, HashSet};

use crate::{
    decisions::Decisions,
    layout::{LayoutShot, MIN_PER_SPREAD, layout_chapter, layout_spread},
    model::{ChapterKey, Conflict, EditionDef, GroupKey, PersonId, PhotoId, Proposal, Spread, SpreadKind},
    select::{Candidate, GroupPick, SelectInput, select},
};

/// Chapters where an automatic Hero makes sense: the big moments of the Couple.
const HERO_CHAPTERS: [&str; 3] = ["mairie_ceremony", "mairie_after", "bnf_couple"];

#[derive(Debug, Clone, Default)]
pub struct AlbumInput {
    pub candidates: Vec<Candidate>,
    pub layout: HashMap<PhotoId, LayoutShot>,
    pub groups: Vec<GroupPick>,
    pub quotas: HashMap<PersonId, u32>,
    pub couple: HashSet<PersonId>,
}

/// Photo Spreads in `ours`: every page but the opening and closing ones.
pub fn photo_spreads(page_target: u32) -> usize {
    (page_target.saturating_sub(2) / 2) as usize
}

pub fn build(input: &AlbumInput, decisions: &Decisions) -> Proposal {
    // A Group whose Spread cannot be laid out (too few photos) gives its Spread back to the
    // Chapters, so `ours` stays on its page target.
    let mut skipped: HashSet<GroupKey> = HashSet::new();
    loop {
        let proposal = build_skipping(input, decisions, &skipped);
        let missing: Vec<GroupKey> = input
            .groups
            .iter()
            .filter(|g| !g.members.is_empty() && !skipped.contains(&g.key))
            .filter(|g| !proposal.spreads.iter().any(|s| s.group.as_ref() == Some(&g.key)))
            .map(|g| g.key.clone())
            .collect();
        if missing.is_empty() {
            return proposal;
        }
        skipped.extend(missing);
    }
}

fn build_skipping(input: &AlbumInput, decisions: &Decisions, skipped: &HashSet<GroupKey>) -> Proposal {
    let settings = &decisions.settings;
    let chapters = &settings.chapters;
    let locked: Vec<&Spread> = decisions.locked.values().collect();
    let locked_photos: HashSet<PhotoId> = locked.iter().flat_map(|s| s.photos.iter().map(|p| p.photo)).collect();
    let locked_groups: HashSet<GroupKey> = locked.iter().filter(|s| s.kind == SpreadKind::Group).filter_map(|s| s.group.clone()).collect();
    let groups: Vec<GroupPick> = input
        .groups
        .iter()
        .filter(|g| !g.members.is_empty() && !locked_groups.contains(&g.key) && !skipped.contains(&g.key))
        .cloned()
        .collect();
    let group_count = groups.len() + locked_groups.len();
    let core_total = photo_spreads(settings.page_target).saturating_sub(group_count);

    // Spreads per Chapter: the user's counts, the rest shared by weight × √(usable Shots).
    let usable: Vec<usize> = (0..chapters.len())
        .map(|c| input.candidates.iter().filter(|x| x.chapter == c && x.eligible && !locked_photos.contains(&x.photo)).count())
        .collect();
    let locked_in = |c: usize| locked.iter().filter(|s| s.kind == SpreadKind::Core && s.chapter == chapters[c].key).count();
    let mut budget: Vec<usize> = vec![0; chapters.len()];
    let mut fixed = vec![false; chapters.len()];
    for (c, chapter) in chapters.iter().enumerate() {
        if let Some(&count) = decisions.chapter_spreads.get(&chapter.key) {
            budget[c] = count as usize;
            fixed[c] = true;
        } else if usable[c] > 0 || locked_in(c) > 0 {
            budget[c] = 1.max(locked_in(c));
        }
    }
    let weights: Vec<f64> = (0..chapters.len())
        .map(|c| if fixed[c] || budget[c] == 0 { 0.0 } else { chapters[c].weight * (usable[c] as f64).sqrt() })
        .collect();
    let mut remaining = core_total.saturating_sub(budget.iter().sum());
    let weight_total: f64 = weights.iter().sum();
    if weight_total > 0.0 {
        let shares: Vec<f64> = weights.iter().map(|w| w / weight_total * remaining as f64).collect();
        for (c, share) in shares.iter().enumerate() {
            budget[c] += share.floor() as usize;
        }
        remaining = remaining.saturating_sub(shares.iter().map(|s| s.floor() as usize).sum());
        let mut order: Vec<usize> = (0..chapters.len()).filter(|&c| weights[c] > 0.0).collect();
        order.sort_by(|&a, &b| (shares[b] - shares[b].floor()).total_cmp(&(shares[a] - shares[a].floor())));
        for c in order.into_iter().cycle().take(remaining) {
            budget[c] += 1;
        }
    }
    let generated: Vec<usize> = (0..chapters.len()).map(|c| budget[c].saturating_sub(locked_in(c))).collect();
    let per_spread = settings.shots_per_spread.max(1.0);

    let selection = select(&SelectInput {
        candidates: input.candidates.clone(),
        budgets: generated.iter().map(|&g| (g as f64 * per_spread).round() as usize).collect(),
        quotas: input.quotas.clone(),
        couple: input.couple.clone(),
        couple_share: settings.couple_share,
        groups,
        locked: locked_photos.clone(),
    });

    // Heroes: the user's, then the best Couple moment of the big Chapters.
    let by_photo: HashMap<PhotoId, &Candidate> = input.candidates.iter().map(|c| (c.photo, c)).collect();
    let selected: HashSet<PhotoId> = selection.chapters.iter().flatten().copied().collect();
    let mut heroes: Vec<PhotoId> = decisions.heroes.iter().copied().filter(|p| selected.contains(p)).collect();
    for key in HERO_CHAPTERS {
        if heroes.len() >= settings.max_heroes as usize {
            break;
        }
        let Some(c) = settings.chapter_index(&ChapterKey::from(key)) else { continue };
        let already = heroes.iter().any(|h| by_photo.get(h).is_some_and(|x| x.chapter == c));
        let best = selection.chapters[c]
            .iter()
            .filter_map(|p| by_photo.get(p))
            .filter(|x| x.couple_face > 0.0 && x.photographer && input.layout.get(&x.photo).is_some_and(|l| l.aspect >= 1.3))
            .max_by(|a, b| (a.quality + a.couple_face).total_cmp(&(b.quality + b.couple_face)));
        if let (false, Some(best)) = (already, best) {
            heroes.push(best.photo);
        }
    }
    heroes.truncate(settings.max_heroes as usize);
    let shot = |photo: &PhotoId| -> Option<LayoutShot> {
        let mut shot = input.layout.get(photo)?.clone();
        shot.hero = heroes.contains(photo);
        Some(shot)
    };

    let mut spreads = Vec::new();
    for (c, chapter) in chapters.iter().enumerate() {
        let shots: Vec<LayoutShot> = selection.chapters[c].iter().filter_map(shot).collect();
        let mut chapter_locked: Vec<Spread> =
            locked.iter().filter(|s| s.kind == SpreadKind::Core && s.chapter == chapter.key).map(|s| (*s).clone()).collect();
        chapter_locked.sort_by_key(|s| s.position);
        let title = !chapter_locked.iter().any(|s| s.title);
        let mut fresh = layout_chapter(&shots, generated[c], title, per_spread).into_iter().enumerate().map(|(i, layout)| Spread {
            uid: 0,
            chapter: chapter.key.clone(),
            kind: SpreadKind::Core,
            group: None,
            templates: layout.templates,
            photos: layout.photos,
            title: title && i == 0,
            locked: false,
            position: 0,
        });
        let total = chapter_locked.len() + generated[c];
        let mut chapter_spreads: Vec<Spread> = Vec::new();
        let mut locked_iter = chapter_locked.into_iter().peekable();
        for position in 0..total {
            let next = if locked_iter.peek().is_some_and(|s| s.position as usize <= position) {
                locked_iter.next()
            } else {
                fresh.next().or_else(|| locked_iter.next())
            };
            if let Some(spread) = next {
                chapter_spreads.push(spread);
            }
        }
        chapter_spreads.extend(locked_iter);
        if settings.group_spread_chapter == chapter.key {
            for (key, photos) in &selection.groups {
                let shots: Vec<LayoutShot> = photos.iter().filter_map(shot).collect();
                if shots.len() >= MIN_PER_SPREAD
                    && let Some(layout) = layout_spread(&shots, true)
                {
                    chapter_spreads.push(Spread {
                        uid: 0,
                        chapter: chapter.key.clone(),
                        kind: SpreadKind::Group,
                        group: Some(key.clone()),
                        templates: layout.templates,
                        photos: layout.photos,
                        title: true,
                        locked: false,
                        position: 0,
                    });
                }
            }
            chapter_spreads.extend(locked.iter().filter(|s| s.kind == SpreadKind::Group).map(|s| (*s).clone()));
        }
        for (position, spread) in chapter_spreads.iter_mut().enumerate() {
            spread.position = position as u32;
            if !spread.locked {
                spread.uid = uid(spread);
            }
        }
        spreads.extend(chapter_spreads);
    }

    Proposal {
        spreads,
        alternatives: selection.alternatives,
        unmet_quotas: selection.unmet_quotas.clone(),
        couple_share: selection.couple_share,
    }
}

/// Overrides the album as shown cannot honour (Ban > Pin > Rejected).
pub fn conflicts(album: &Proposal, decisions: &Decisions) -> Vec<Conflict> {
    let mut found: Vec<Conflict> = album.spreads.iter().flat_map(|s| spread_conflicts(s, decisions)).collect();
    let placed: HashSet<PhotoId> = album.spreads.iter().flat_map(|s| s.photos.iter().map(|p| p.photo)).collect();
    let mut pins: Vec<PhotoId> = decisions.pins.iter().copied().filter(|p| !placed.contains(p) && !decisions.bans.contains(p)).collect();
    pins.sort_unstable();
    found.extend(pins.into_iter().map(|photo| Conflict::PinNotPlaced { photo }));
    found
}

/// The conflicts shown on one Spread.
pub fn spread_conflicts(spread: &Spread, decisions: &Decisions) -> Vec<Conflict> {
    if !spread.locked {
        return Vec::new();
    }
    spread
        .photos
        .iter()
        .filter(|p| decisions.bans.contains(&p.photo))
        .map(|p| Conflict::BannedOnLockedSpread { photo: p.photo, spread: spread.uid })
        .collect()
}

/// A stable identity from what the Spread shows.
fn uid(spread: &Spread) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(spread.chapter.as_str().as_bytes());
    feed(spread.group.as_ref().map_or("", GroupKey::as_str).as_bytes());
    for placed in &spread.photos {
        feed(&placed.photo.to_le_bytes());
    }
    hash & 0x001F_FFFF_FFFF_FFFF
}

pub fn edition_spreads<'a>(proposal: &'a Proposal, edition: &EditionDef) -> Vec<&'a Spread> {
    proposal.spreads.iter().filter(|spread| spread.in_edition(edition)).collect()
}

/// Pages of an Edition: the opening and closing pages plus two per Spread.
pub fn edition_pages(proposal: &Proposal, edition: &EditionDef) -> usize {
    2 + 2 * edition_spreads(proposal, edition).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EditionKey, Override, Settings};

    fn input() -> AlbumInput {
        let mut input = AlbumInput::default();
        for photo in 1..=60 {
            let chapter = if photo <= 30 { 0 } else { 1 };
            let persons = match photo {
                41..=44 => vec![PersonId::from(100)],
                45..=48 => vec![PersonId::from(200)],
                _ => vec![],
            };
            input.candidates.push(Candidate {
                photo,
                chapter,
                order: photo as f64,
                quality: 0.5 + (photo % 7) as f64 / 20.0,
                persons,
                embedding: vec![(photo as f32).cos(), (photo as f32).sin()],
                eligible: true,
                ..Candidate::default()
            });
            input.layout.insert(photo, LayoutShot { photo, aspect: 1.5, quality: 0.5, max_print_mm: 500.0, ..LayoutShot::default() });
        }
        input.groups = vec![
            GroupPick { key: "bride_family".into(), members: HashSet::from([PersonId::from(100)]), size: 4 },
            GroupPick { key: "groom_dad_family".into(), members: HashSet::from([PersonId::from(200)]), size: 4 },
        ];
        input
    }

    fn decisions(extra: &[Override]) -> Decisions {
        let mut settings = Settings::default();
        settings.chapters.truncate(2);
        settings.group_spread_chapter = settings.chapters[1].key.clone();
        let mut overrides = vec![
            Override::SetSetting { key: "chapters".into(), value: serde_json::to_value(&settings.chapters).unwrap() },
            Override::SetSetting { key: "group_spread_chapter".into(), value: serde_json::json!(settings.group_spread_chapter) },
            Override::SetSetting { key: "page_target".into(), value: serde_json::json!(16) },
        ];
        overrides.extend_from_slice(extra);
        Decisions::replay(&overrides)
    }

    #[test]
    fn locked_spreads_survive_and_editions_drop_other_groups_spreads() {
        let first = build(&input(), &decisions(&[]));
        assert_eq!(first.spreads.len(), photo_spreads(16));

        // Lock the second Spread, then regenerate.
        let locked = first.spreads[1].clone();
        let again = build(&input(), &decisions(&[Override::LockSpread { spread: locked.clone() }]));
        let kept = again.spreads.iter().find(|s| s.uid == locked.uid).expect("locked Spread kept");
        assert_eq!(kept.photos, locked.photos);
        assert_eq!(again.spreads.iter().position(|s| s.uid == locked.uid), Some(1));
        let placed: Vec<PhotoId> = again.spreads.iter().flat_map(|s| s.photos.iter().map(|p| p.photo)).collect();
        assert_eq!(placed.len(), placed.iter().collect::<HashSet<_>>().len(), "a Shot appears twice");

        let settings = Settings::default();
        let ours = settings.edition(&EditionKey::from("ours")).unwrap();
        let dad = settings.edition(&EditionKey::from("dad")).unwrap();
        let group_of = |s: &&Spread| s.group.clone();
        assert_eq!(edition_spreads(&again, ours).len(), again.spreads.len());
        let dad_groups: Vec<_> = edition_spreads(&again, dad).iter().filter_map(group_of).collect();
        assert_eq!(dad_groups, vec![GroupKey::from("groom_dad_family")]);
        assert_eq!(edition_pages(&again, ours) - edition_pages(&again, dad), 2);
    }

    #[test]
    fn a_group_too_small_for_its_own_spread_does_not_cost_a_core_spread() {
        let mut input = input();
        // One photo of the Bride's friends: not enough for a Spread.
        input.candidates[49].persons = vec![PersonId::from(300)];
        input.groups.push(GroupPick { key: "friends".into(), members: HashSet::from([PersonId::from(300)]), size: 4 });

        let album = build(&input, &decisions(&[]));

        assert!(!album.spreads.iter().any(|s| s.group.as_ref() == Some(&GroupKey::from("friends"))));
        assert_eq!(album.spreads.len(), photo_spreads(16));
    }

    #[test]
    fn overrides_that_cannot_apply_are_reported_on_their_spread() {
        let first = build(&input(), &decisions(&[]));
        let locked = first.spreads[0].clone();
        let banned = locked.photos[0].photo;
        // Every Spread of the first Chapter Locked: no room left for a new Pin there.
        let locks: Vec<Override> = first
            .spreads
            .iter()
            .filter(|s| s.chapter == locked.chapter)
            .map(|s| Override::LockSpread { spread: s.clone() })
            .collect();
        let placed: HashSet<PhotoId> = first.spreads.iter().flat_map(|s| s.photos.iter().map(|p| p.photo)).collect();
        let pinned = (1..=30).find(|p| !placed.contains(p)).unwrap();
        let mut overrides = locks;
        overrides.push(Override::Ban { photo: banned, on: true });
        overrides.push(Override::Pin { photo: pinned, on: true });
        let decisions = decisions(&overrides);
        let mut input = input();
        input.candidates.iter_mut().filter(|c| c.photo == pinned).for_each(|c| c.pinned = true);
        input.candidates.iter_mut().filter(|c| c.photo == banned).for_each(|c| c.eligible = false);

        let album = build(&input, &decisions);
        let found = conflicts(&album, &decisions);

        assert!(found.contains(&Conflict::BannedOnLockedSpread { photo: banned, spread: locked.uid }), "{found:?}");
        assert!(found.contains(&Conflict::PinNotPlaced { photo: pinned }), "{found:?}");
        assert_eq!(found.len(), 2, "{found:?}");
    }
}
