//! Replays the active Overrides into the current Decisions.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::model::{Override, PhotoId, Settings, Spread};

#[derive(Debug, Clone, Default)]
pub struct Decisions {
    pub settings: Settings,
    pub pins: HashSet<PhotoId>,
    pub bans: HashSet<PhotoId>,
    pub heroes: HashSet<PhotoId>,
    pub chapter_moves: HashMap<PhotoId, String>,
    pub shot_splits: HashSet<PhotoId>,
    pub locked: BTreeMap<u64, Spread>,
    pub chapter_spreads: HashMap<String, u32>,
    pub texts: BTreeMap<String, String>,
    /// People Overrides, in order; resolved against face clusters by `people::resolve`.
    pub people: Vec<Override>,
}

/// Whether a Photo may appear in the album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eligibility {
    Banned,
    Pinned,
    Rejected(String),
    Eligible,
}

impl Decisions {
    pub fn replay<'a>(overrides: impl IntoIterator<Item = &'a Override>) -> Self {
        let mut decisions = Self::default();
        let mut settings = serde_json::to_value(Settings::default()).expect("settings serialize");

        for item in overrides {
            match item {
                Override::Pin { photo, on } => toggle(&mut decisions.pins, *photo, *on),
                Override::Ban { photo, on } => toggle(&mut decisions.bans, *photo, *on),
                Override::Hero { photo, on } => toggle(&mut decisions.heroes, *photo, *on),
                Override::MoveChapter { photo, chapter } => {
                    decisions.chapter_moves.insert(*photo, chapter.clone());
                }
                Override::SplitShot { photo } => {
                    decisions.shot_splits.insert(*photo);
                }
                Override::LockSpread { spread } => {
                    let mut spread = spread.clone();
                    spread.locked = true;
                    decisions.locked.insert(spread.uid, spread);
                }
                Override::UnlockSpread { uid } => {
                    decisions.locked.remove(uid);
                }
                Override::MoveSpread { uid, chapter, position } => {
                    if let Some(spread) = decisions.locked.get_mut(uid) {
                        spread.chapter = chapter.clone();
                        spread.position = *position;
                    }
                }
                Override::SetChapterSpreads { chapter, count } => {
                    decisions.chapter_spreads.insert(chapter.clone(), *count);
                }
                Override::SetText { key, value } => {
                    decisions.texts.insert(key.clone(), value.clone());
                }
                Override::SetSetting { key, value } => set_path(&mut settings, key, value.clone()),
                Override::NamePerson { .. }
                | Override::SetRole { .. }
                | Override::SetGroup { .. }
                | Override::SetQuota { .. }
                | Override::MergePersons { .. }
                | Override::SplitFaces { .. }
                | Override::AssignFace { .. }
                | Override::RemoveFace { .. } => decisions.people.push(item.clone()),
            }
        }

        // A setting that no longer fits the schema falls back to the defaults rather than
        // making the whole album unreadable.
        decisions.settings = serde_json::from_value(settings).unwrap_or_default();
        decisions
    }

    /// Ban > Pin > Rejected.
    pub fn eligibility(&self, photo: PhotoId, rejected: Option<&str>) -> Eligibility {
        if self.bans.contains(&photo) {
            Eligibility::Banned
        } else if self.pins.contains(&photo) {
            Eligibility::Pinned
        } else if let Some(reason) = rejected {
            Eligibility::Rejected(reason.to_string())
        } else {
            Eligibility::Eligible
        }
    }
}

fn toggle(set: &mut HashSet<PhotoId>, photo: PhotoId, on: bool) {
    if on {
        set.insert(photo);
    } else {
        set.remove(&photo);
    }
}

/// Sets `a.b.c` inside a JSON value, creating objects on the way; a number indexes a list.
fn set_path(root: &mut serde_json::Value, key: &str, value: serde_json::Value) {
    let mut node = root;
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        if node.is_array()
            && let Ok(index) = part.parse::<usize>()
        {
            // An index past the end is ignored rather than growing the list.
            let Some(item) = node.as_array_mut().and_then(|list| list.get_mut(index)) else { return };
            if last {
                *item = value;
                return;
            }
            node = item;
            continue;
        }
        if !node.is_object() {
            *node = serde_json::Value::Object(Default::default());
        }
        let map = node.as_object_mut().expect("just made an object");
        if last {
            map.insert(part.to_string(), value);
            return;
        }
        node = map.entry(part).or_insert(serde_json::Value::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ban_beats_pin_and_pin_beats_rejection() {
        let decisions = Decisions::replay(&[
            Override::Pin { photo: 1, on: true },
            Override::Ban { photo: 1, on: true },
            Override::Pin { photo: 2, on: true },
            Override::Ban { photo: 3, on: true },
            Override::Ban { photo: 3, on: false },
        ]);

        assert_eq!(decisions.eligibility(1, None), Eligibility::Banned);
        assert_eq!(decisions.eligibility(2, Some("blurry")), Eligibility::Pinned);
        assert_eq!(
            decisions.eligibility(3, Some("blurry")),
            Eligibility::Rejected("blurry".into())
        );
        assert_eq!(decisions.eligibility(4, None), Eligibility::Eligible);
    }

    #[test]
    fn settings_overrides_change_nested_values() {
        let decisions = Decisions::replay(&[
            Override::SetSetting {
                key: "gate.guest_min_quality".into(),
                value: serde_json::json!(0.4),
            },
            Override::SetSetting {
                key: "page_target".into(),
                value: serde_json::json!(40),
            },
            Override::SetSetting {
                key: "chapters.2.starts_at".into(),
                value: serde_json::json!("14:05"),
            },
        ]);

        assert_eq!(decisions.settings.gate.guest_min_quality, 0.4);
        assert_eq!(decisions.settings.page_target, 40);
        assert_eq!(decisions.settings.max_heroes, 3);
        assert_eq!(decisions.settings.chapters[2].starts_at.as_deref(), Some("14:05"));
        assert_eq!(decisions.settings.chapters.len(), Settings::default().chapters.len());
    }
}
