//! Selection: which Shots make the album. Greedy and submodular: each pick lowers the
//! value of similar Shots and of people already shown enough.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::model::{GroupKey, PersonId, PhotoId};

/// One Shot that could go in the album (its representative Photo).
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub photo: PhotoId,
    pub chapter: usize,
    /// Position in the Chapter (time order).
    pub order: f64,
    pub quality: f64,
    /// Persons (anchors) clearly visible.
    pub persons: Vec<PersonId>,
    /// Height of the biggest Couple face, as a share of the Photo's height (0 if none).
    pub couple_face: f64,
    pub photographer: bool,
    pub group_portrait: bool,
    /// Unit-length CLIP embedding.
    pub embedding: Vec<f32>,
    pub time_ms: Option<i64>,
    /// Not Banned, and not Rejected unless Pinned.
    pub eligible: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone)]
pub struct GroupPick {
    pub key: GroupKey,
    pub members: HashSet<PersonId>,
    pub size: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SelectInput {
    pub candidates: Vec<Candidate>,
    /// How many Shots each Chapter's core Spreads should hold.
    pub budgets: Vec<usize>,
    pub quotas: HashMap<PersonId, u32>,
    pub couple: HashSet<PersonId>,
    pub couple_share: f64,
    pub groups: Vec<GroupPick>,
    /// Shots already placed in Locked Spreads.
    pub locked: HashSet<PhotoId>,
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Selected Shots per Chapter, in Chapter order.
    pub chapters: Vec<Vec<PhotoId>>,
    pub groups: Vec<(GroupKey, Vec<PhotoId>)>,
    /// For the candidate tray: per selected Shot, the best unselected alternatives.
    pub alternatives: BTreeMap<PhotoId, Vec<PhotoId>>,
    pub couple_share: f64,
    /// (person, quota, reached)
    pub unmet_quotas: Vec<(PersonId, u32, u32)>,
}

const ALTERNATIVES: usize = 12;

fn dot(a: &[f32], b: &[f32]) -> f64 {
    f64::from(a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>())
}

/// Group Spreads first (they take their Group's best photos), then the core: Pins, then
/// the greedy pick across all Chapters until each Chapter's budget is full.
pub fn select(input: &SelectInput) -> Selection {
    let candidates = &input.candidates;
    let chapters = input.budgets.len();
    let mut taken: HashSet<usize> = HashSet::new();
    let mut counts: HashMap<PersonId, u32> = HashMap::new();
    let count_in = |counts: &mut HashMap<PersonId, u32>, candidate: &Candidate| {
        for person in &candidate.persons {
            *counts.entry(*person).or_insert(0) += 1;
        }
    };
    for (i, candidate) in candidates.iter().enumerate() {
        if input.locked.contains(&candidate.photo) {
            taken.insert(i);
            count_in(&mut counts, candidate);
        }
    }

    // 1. Group Spreads: photos where the Group's members are most of the people shown.
    let mut groups = Vec::new();
    for group in &input.groups {
        let pool: Vec<usize> = (0..candidates.len())
            .filter(|i| !taken.contains(i) && candidates[*i].eligible)
            .filter(|&i| {
                let others: Vec<&PersonId> = candidates[i].persons.iter().filter(|p| !input.couple.contains(p)).collect();
                let members = others.iter().filter(|p| group.members.contains(p)).count();
                members > 0 && members * 2 >= others.len()
            })
            .collect();
        let mut covered: HashSet<PersonId> = HashSet::new();
        let mut picked: Vec<usize> = Vec::new();
        while picked.len() < group.size {
            let best = pool
                .iter()
                .copied()
                .filter(|i| !picked.contains(i))
                .map(|i| {
                    let c = &candidates[i];
                    let members: Vec<PersonId> = c.persons.iter().copied().filter(|p| group.members.contains(p)).collect();
                    let new = members.iter().filter(|p| !covered.contains(p)).count() as f64;
                    let similar = picked.iter().map(|&j| dot(&c.embedding, &candidates[j].embedding)).fold(0.0, f64::max);
                    let gain = 0.5 * c.quality + new + 0.3 * f64::from(u8::from(c.group_portrait)) + 0.2 * members.len() as f64
                        - 2.0 * ((similar - 0.85) / 0.15).max(0.0);
                    (i, gain)
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let Some((i, _)) = best else { break };
            covered.extend(candidates[i].persons.iter().copied().filter(|p| group.members.contains(p)));
            picked.push(i);
        }
        picked.sort_by(|a, b| candidates[*a].order.total_cmp(&candidates[*b].order));
        for &i in &picked {
            taken.insert(i);
            count_in(&mut counts, &candidates[i]);
        }
        if !picked.is_empty() {
            groups.push((group.key.clone(), picked.iter().map(|&i| candidates[i].photo).collect()));
        }
    }

    // 2. Core: Pins first, then the best gain until every Chapter is full.
    let mut selected: Vec<Vec<usize>> = vec![Vec::new(); chapters];
    let mut max_similar = vec![0.0_f64; candidates.len()];
    let mut couple_shots = 0_usize;
    let pick = |i: usize,
                selected: &mut Vec<Vec<usize>>,
                taken: &mut HashSet<usize>,
                counts: &mut HashMap<PersonId, u32>,
                max_similar: &mut Vec<f64>,
                couple_shots: &mut usize| {
        let chosen = &candidates[i];
        taken.insert(i);
        selected[chosen.chapter].push(i);
        count_in(counts, chosen);
        if chosen.couple_face > 0.0 {
            *couple_shots += 1;
        }
        for (j, other) in candidates.iter().enumerate() {
            if other.chapter == chosen.chapter {
                max_similar[j] = max_similar[j].max(dot(&chosen.embedding, &other.embedding));
            }
        }
    };
    for i in 0..candidates.len() {
        if candidates[i].pinned && candidates[i].eligible && !taken.contains(&i) && candidates[i].chapter < chapters {
            pick(i, &mut selected, &mut taken, &mut counts, &mut max_similar, &mut couple_shots);
        }
    }
    loop {
        let total: usize = selected.iter().map(Vec::len).sum();
        let share = if total == 0 { 1.0 } else { couple_shots as f64 / total as f64 };
        let pressure = 1.0 + 3.0 * (input.couple_share - share).max(0.0);
        let best = candidates
            .iter()
            .enumerate()
            .filter(|(i, c)| c.eligible && !taken.contains(i) && c.chapter < chapters && selected[c.chapter].len() < input.budgets[c.chapter])
            .map(|(i, c)| {
                let mut gain = c.quality;
                if c.couple_face > 0.0 {
                    gain += 0.6 * (0.5 + (c.couple_face * 3.0).min(1.0)) * pressure;
                }
                for person in &c.persons {
                    let count = f64::from(counts.get(person).copied().unwrap_or(0));
                    if let Some(&quota) = input.quotas.get(person).filter(|q| **q > 0) {
                        gain += 1.5 * (f64::from(quota) - count).max(0.0) / f64::from(quota);
                    }
                }
                let coverage: f64 = c
                    .persons
                    .iter()
                    .filter(|p| !input.couple.contains(p))
                    .map(|p| 0.25 / (1.0 + f64::from(counts.get(p).copied().unwrap_or(0))))
                    .sum();
                gain += coverage.min(1.0);
                if c.photographer {
                    gain += 0.15;
                }
                gain -= 2.0 * ((max_similar[i] - 0.75) / 0.25).max(0.0);
                let close_in_time = selected[c.chapter].iter().any(|&j| {
                    matches!((c.time_ms, candidates[j].time_ms), (Some(a), Some(b)) if (a - b).abs() < 30_000)
                });
                if close_in_time {
                    gain -= 0.3;
                }
                (i, gain)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let Some((i, _)) = best else { break };
        pick(i, &mut selected, &mut taken, &mut counts, &mut max_similar, &mut couple_shots);
    }

    // 3. Alternatives for the candidate tray.
    let mut alternatives = BTreeMap::new();
    for chosen in selected.iter().flatten() {
        let c = &candidates[*chosen];
        let mut others: Vec<(PhotoId, f64)> = candidates
            .iter()
            .enumerate()
            .filter(|(j, o)| o.eligible && o.chapter == c.chapter && !taken.contains(j))
            .map(|(_, o)| (o.photo, 0.6 * dot(&c.embedding, &o.embedding) + 0.4 * o.quality))
            .collect();
        others.sort_by(|a, b| b.1.total_cmp(&a.1));
        alternatives.insert(c.photo, others.into_iter().take(ALTERNATIVES).map(|(photo, _)| photo).collect());
    }

    let total: usize = selected.iter().map(Vec::len).sum();
    let mut unmet_quotas: Vec<(i64, u32, u32)> = input
        .quotas
        .iter()
        .filter(|(_, quota)| **quota > 0)
        .map(|(person, quota)| (*person, *quota, counts.get(person).copied().unwrap_or(0)))
        .filter(|(_, quota, reached)| reached < quota)
        .collect();
    unmet_quotas.sort_unstable();
    Selection {
        chapters: selected
            .into_iter()
            .map(|mut chapter| {
                chapter.sort_by(|a, b| candidates[*a].order.total_cmp(&candidates[*b].order));
                chapter.into_iter().map(|i| candidates[i].photo).collect()
            })
            .collect(),
        groups,
        alternatives,
        couple_share: if total == 0 { 0.0 } else { couple_shots as f64 / total as f64 },
        unmet_quotas,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(photo: PhotoId, quality: f64, angle: f32) -> Candidate {
        Candidate {
            photo,
            quality,
            embedding: vec![angle.cos(), angle.sin()],
            eligible: true,
            order: photo as f64,
            ..Candidate::default()
        }
    }

    #[test]
    fn pins_and_quotas_get_in_bans_never_do() {
        let mut banned = shot(5, 0.95, 0.4);
        banned.eligible = false;
        let mut pinned = shot(4, 0.2, 0.8);
        pinned.pinned = true;
        let mut aunt = shot(6, 0.3, 1.2);
        aunt.persons = vec![PersonId::from(7)];
        let input = SelectInput {
            candidates: vec![
                shot(1, 0.9, 0.0),
                shot(2, 0.8, 1.5),
                shot(3, 0.85, 0.01), // nearly the same picture as 1
                pinned,
                banned,
                aunt,
            ],
            budgets: vec![3],
            quotas: HashMap::from([(PersonId::from(7), 1)]),
            ..SelectInput::default()
        };

        let selection = select(&input);
        let chosen = &selection.chapters[0];

        assert_eq!(chosen.len(), 3);
        assert!(chosen.contains(&4), "pinned Shot missing: {chosen:?}");
        assert!(chosen.contains(&6), "quota not met: {chosen:?}");
        assert!(!chosen.contains(&5), "banned Shot selected");
        assert!(selection.unmet_quotas.is_empty());
    }

    #[test]
    fn group_spreads_take_their_members_photos_and_core_does_not_repeat_them() {
        let mut portrait = shot(1, 0.6, 0.0);
        portrait.persons = vec![PersonId::from(10), PersonId::from(11)];
        portrait.group_portrait = true;
        let mut uncle = shot(2, 0.5, 1.0);
        uncle.persons = vec![PersonId::from(10)];
        let mut friend = shot(3, 0.9, 2.0);
        friend.persons = vec![PersonId::from(20)];
        let input = SelectInput {
            candidates: vec![portrait, uncle, friend, shot(4, 0.7, 3.0)],
            budgets: vec![4],
            groups: vec![GroupPick { key: "fam".into(), members: HashSet::from([PersonId::from(10), PersonId::from(11)]), size: 2 }],
            ..SelectInput::default()
        };

        let selection = select(&input);

        assert_eq!(selection.groups, vec![(GroupKey::from("fam"), vec![1, 2])]);
        assert_eq!(selection.chapters[0], vec![3, 4]);
    }
}
