use std::collections::HashMap;

use anyhow::Result;
use iris_core::{
    chapters::{GuestEvidence, guess_guest_chapter, split_contiguous},
    model::Source,
    decisions::Decisions,
};

use crate::{
    db::PhotoRecord,
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
    stages::embed::{ClipText, dot, normalize, softmax},
};

const NEIGHBOURS: usize = 10;
/// Chapters may start a bit earlier than their nominal time.
const START_SLACK_MS: i64 = 45 * 60 * 1000;

/// Puts every Photo in a Chapter: the photographer's folders first (split by content when
/// a folder spans several moments), then guests by visual neighbours and send time.
pub struct Chapters;

impl Stage for Chapters {
    fn name(&self) -> &'static str {
        "chapters"
    }

    fn title(&self) -> &'static str {
        "Place in Chapters"
    }

    fn description(&self) -> &'static str {
        "Photographer folders give the Chapters; guest photos join the Chapter of the photographer's photos they look like."
    }

    fn version(&self) -> i64 {
        2
    }

    fn deps(&self) -> &'static [&'static str] {
        &["embed", "shots"]
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn decisions_key(&self, decisions: &Decisions) -> String {
        let settings = &decisions.settings;
        let chapters: Vec<_> = settings.chapters.iter().map(|c| (&c.key, &c.prompts, &c.starts_at)).collect();
        format!("{}{}", serde_json::to_string(&chapters).unwrap_or_default(), serde_json::to_string(&settings.folder_chapters).unwrap_or_default())
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, _cancel: &CancelToken) -> Result<()> {
        let settings = ctx.settings();
        let chapters = &settings.chapters;
        let photos = ctx.db.all_photos()?;
        let embeddings: HashMap<i64, Vec<f32>> = ctx.db.image_embeddings()?.into_iter().collect();
        sink.start(photos.len());

        sink.message("reading the Chapters' descriptions");
        let clip = ClipText::load()?;
        let prompt_vectors: Vec<Vec<f32>> = chapters
            .iter()
            .map(|chapter| {
                let prompts: Vec<&str> = if chapter.prompts.is_empty() {
                    vec![chapter.title_fr.as_str()]
                } else {
                    chapter.prompts.iter().map(String::as_str).collect()
                };
                let vectors = clip.embed(&prompts)?;
                let mut mean = vec![0.0; vectors[0].len()];
                for vector in &vectors {
                    mean.iter_mut().zip(vector).for_each(|(m, v)| *m += v);
                }
                Ok(normalize(mean))
            })
            .collect::<Result<_>>()?;
        let zero_shot = |photo: i64| -> Vec<f64> {
            embeddings.get(&photo).map_or_else(
                || vec![1.0 / chapters.len() as f64; chapters.len()],
                |e| softmax(&prompt_vectors.iter().map(|p| dot(p, e) * 100.0).collect::<Vec<_>>()).into_iter().map(f64::from).collect(),
            )
        };

        // (chapter index, probability, margin, source, order)
        let mut assigned: HashMap<i64, (usize, f64, f64, &'static str, f64)> = HashMap::new();

        let wedding_day = settings.wedding_date.clone();
        let starts: Vec<Option<i64>> = chapters
            .iter()
            .map(|c| {
                let time = c.starts_at.as_ref()?;
                let local = chrono::NaiveDateTime::parse_from_str(&format!("{wedding_day} {time}"), "%Y-%m-%d %H:%M").ok()?;
                iris_core::capture::paris_to_utc_ms(local)
            })
            .collect();
        // 1. Photographer folders: content, plus the Chapters' start times when known.
        let mut by_folder: HashMap<&str, Vec<&PhotoRecord>> = HashMap::new();
        for photo in photos.iter().filter(|p| p.source == Some(Source::Photographer)) {
            by_folder.entry(photo.source_folder.as_str()).or_default().push(photo);
        }
        for (folder, mut members) in by_folder {
            let Some(keys) = settings.folder_chapters.get(folder) else { continue };
            let indices: Vec<usize> = keys.iter().filter_map(|key| settings.chapter_index(key)).collect();
            if indices.is_empty() {
                continue;
            }
            members.sort_by_key(|p| (p.taken_at_utc, p.sequence, p.file_name.clone()));
            let split = if indices.len() == 1 {
                vec![0; members.len()]
            } else {
                let scores: Vec<Vec<f64>> = members
                    .iter()
                    .map(|p| {
                        let probs = zero_shot(p.id);
                        let sub: Vec<f64> = indices.iter().map(|&c| probs[c]).collect();
                        let total: f64 = sub.iter().sum::<f64>().max(1e-9);
                        indices
                            .iter()
                            .enumerate()
                            .map(|(k, &c)| {
                                let in_window = match (p.taken_at_utc, starts[c]) {
                                    (Some(t), Some(start)) => {
                                        let next = indices.get(k + 1).and_then(|&n| starts[n]).unwrap_or(i64::MAX);
                                        t >= start && t < next
                                    }
                                    _ => false,
                                };
                                sub[k] / total + if in_window { 0.6 } else { 0.0 }
                            })
                            .collect()
                    })
                    .collect();
                split_contiguous(&scores)
            };
            let source = if indices.len() == 1 { "folder" } else { "folder split" };
            for (position, (photo, block)) in members.iter().zip(split).enumerate() {
                let chapter = indices[block];
                assigned.insert(photo.id, (chapter, 1.0, 1.0, source, position as f64));
            }
        }

        // 2. References for guests: the photographer's Photos and the user's moves.
        for (photo, key) in &ctx.decisions.chapter_moves {
            if let Some(index) = settings.chapter_index(key) {
                let order = assigned.get(photo).map_or(0.0, |a| a.4);
                assigned.insert(*photo, (index, 1.0, 1.0, "you", order));
            }
        }
        let references: Vec<(i64, usize, f64, &Vec<f32>)> = assigned
            .iter()
            .filter_map(|(id, a)| embeddings.get(id).map(|e| (*id, a.0, a.4, e)))
            .collect();

        // 3. Guests (and photographer Photos from unmapped folders).
        let people = crate::state::AlbumState::load(&ctx.db)?;
        let groom = people.people.with_role(iris_core::model::Role::Groom).map(|p| p.anchor);
        let bride = people.people.with_role(iris_core::model::Role::Bride).map(|p| p.anchor);
        let mut low_confidence = 0;
        let unplaced: Vec<&PhotoRecord> = photos.iter().filter(|p| !assigned.contains_key(&p.id)).collect();
        for photo in unplaced {
            let mut neighbours: Vec<(usize, f64, f64)> = embeddings
                .get(&photo.id)
                .map(|e| references.iter().map(|(_, chapter, order, r)| (*chapter, f64::from(dot(e, r)), *order)).collect())
                .unwrap_or_default();
            neighbours.sort_by(|a, b| b.1.total_cmp(&a.1));
            neighbours.truncate(NEIGHBOURS);
            let latest_possible = photo.sent_at_utc.and_then(|sent| {
                starts
                    .iter()
                    .enumerate()
                    .filter(|(_, start)| start.is_some_and(|start| start - START_SLACK_MS <= sent))
                    .map(|(c, _)| c)
                    .max()
                    // Only meaningful when sent on the day itself.
                    .filter(|_| starts.iter().flatten().any(|start| sent - start < 24 * 3600 * 1000))
            });
            let evidence = GuestEvidence {
                neighbours: neighbours.iter().map(|(c, s, _)| (*c, *s)).collect(),
                zero_shot: zero_shot(photo.id),
                latest_possible,
            };
            // Mistral's answer for an unsure photo, when the user asked for it.
            if let Some(index) = crate::stages::vlm::chapter(&ctx.db, photo).and_then(|key| settings.chapter_index(&key)) {
                let order = neighbours.iter().find(|(c, _, _)| *c == index).map_or(1e6, |(_, s, o)| o + 0.5 - s * 0.1);
                assigned.insert(photo.id, (index, 0.9, 0.5, "mistral", order));
                sink.inc(1);
                continue;
            }
            let referenced: Vec<bool> = (0..chapters.len()).map(|c| references.iter().any(|r| r.1 == c)).collect();
            // The groom without the bride, before the town hall: his preparation.
            if let (Some(groom_prep), Some(bride_prep)) = (settings.chapter_index("prep_groom"), settings.chapter_index("prep_bride")) {
                let persons = people.persons_in(photo.id);
                let before_mairie = photo.sent_at_utc.zip(settings.chapter_index("mairie_before").and_then(|c| starts[c])).is_some_and(|(sent, start)| sent < start);
                let looks_like_prep = evidence.neighbours.iter().take(3).any(|(c, _)| *c == bride_prep || *c == groom_prep);
                if groom.is_some_and(|g| persons.contains(&g)) && !bride.is_some_and(|b| persons.contains(&b)) && (before_mairie || looks_like_prep) {
                    assigned.insert(photo.id, (groom_prep, 0.8, 0.5, "groom without bride", 0.0));
                    sink.inc(1);
                    continue;
                }
            }
            // A Chapter nobody photographed can only win on its description.
            let best_described = (0..chapters.len()).max_by(|&a, &b| evidence.zero_shot[a].total_cmp(&evidence.zero_shot[b]));
            if let Some(c) = best_described.filter(|&c| !referenced[c] && evidence.zero_shot[c] >= 0.35 && evidence.latest_possible.is_none_or(|l| c <= l)) {
                assigned.insert(photo.id, (c, evidence.zero_shot[c], 0.1, "description", 0.0));
                sink.inc(1);
                continue;
            }
            if let Some(guess) = guess_guest_chapter(&evidence, chapters.len()) {
                if guess.margin < 0.15 {
                    low_confidence += 1;
                }
                // Sit next to the most similar reference of the same Chapter.
                let order = neighbours.iter().find(|(c, _, _)| *c == guess.chapter).map_or(1e6, |(_, s, o)| o + 0.5 - s * 0.1);
                assigned.insert(photo.id, (guess.chapter, guess.probability, guess.margin, "visual", order));
            }
            sink.inc(1);
        }

        let rows: Vec<_> = assigned
            .into_iter()
            .map(|(photo, (chapter, prob, margin, source, order))| (photo, chapters[chapter].key.clone(), prob, margin, source, order))
            .collect();
        ctx.db.save_chapters(&rows)?;
        sink.message(&format!("{} photos placed, {low_confidence} to check", rows.len()));
        Ok(())
    }
}
