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
        let mut setting_text_order = HashMap::<String, usize>::new();
        let mut text_order = HashMap::<String, usize>::new();

        for (order, item) in overrides.into_iter().enumerate() {
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
                    text_order.insert(key.clone(), order);
                }
                Override::SetSetting { key, value } => {
                    let previous = settings.clone();
                    set_path(&mut settings, key, value.clone());
                    for text_key in setting_text_keys(&previous, &settings, key) {
                        setting_text_order.insert(text_key, order);
                    }
                }
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
        decisions.settings.normalize_group_editions();
        for (key, setting_order) in setting_text_order {
            if text_order.get(&key).is_none_or(|text_order| setting_order > *text_order)
                && let Some(value) = setting_text_value(&decisions.settings, &key)
            {
                decisions.texts.insert(key, value);
            }
        }
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

/// The Chapter and Group title fields also have text entries for the Design page. When a
/// SetSetting updates one of those fields, remember which title it supersedes so a later
/// SetText can still take precedence.
fn setting_text_keys(before: &serde_json::Value, after: &serde_json::Value, path: &str) -> Vec<String> {
    let mut parts = path.split('.');
    let Some(section) = parts.next() else { return Vec::new() };
    let Some(old_items) = before.get(section).and_then(serde_json::Value::as_array) else { return Vec::new() };
    let Some(new_items) = after.get(section).and_then(serde_json::Value::as_array) else { return Vec::new() };
    let title_fields = match section {
        "chapters" => [("title_fr", "fr"), ("title_ar", "ar")],
        "groups" => [("name", "fr"), ("title_ar", "ar")],
        _ => return Vec::new(),
    };
    let mut affected = Vec::new();
    match (section, parts.next(), parts.next()) {
        ("chapters", None, _) | ("groups", None, _) => {
            for item in new_items {
                let Some(key) = item.get("key").and_then(serde_json::Value::as_str) else { continue };
                let old = old_items.iter().find(|old| old.get("key").and_then(serde_json::Value::as_str) == Some(key));
                for (field, language) in title_fields {
                    if old.and_then(|old| old.get(field)) != item.get(field) {
                        let prefix = if section == "chapters" { "chapter" } else { "group" };
                        affected.push(format!("{prefix}.{key}.{language}"));
                    }
                }
            }
        }
        ("chapters", Some(index), field) | ("groups", Some(index), field) => {
            let Ok(index) = index.parse::<usize>() else { return affected };
            let (Some(old), Some(item)) = (old_items.get(index), new_items.get(index)) else { return affected };
            let Some(key) = item.get("key").and_then(serde_json::Value::as_str) else { return affected };
            let prefix = if section == "chapters" { "chapter" } else { "group" };
            for (title_field, language) in title_fields {
                if field.is_none_or(|field| field == title_field) && old.get(title_field) != item.get(title_field) {
                    affected.push(format!("{prefix}.{key}.{language}"));
                }
            }
        }
        _ => {}
    }
    affected
}

fn setting_text_value(settings: &Settings, text_key: &str) -> Option<String> {
    let mut parts = text_key.split('.');
    let (section, key, language) = (parts.next()?, parts.next()?, parts.next()?);
    match (section, language) {
        ("chapter", "fr") => settings.chapters.iter().find(|chapter| chapter.key == key).map(|chapter| chapter.title_fr.clone()),
        ("chapter", "ar") => settings.chapters.iter().find(|chapter| chapter.key == key).map(|chapter| chapter.title_ar.clone()),
        ("group", "fr") => settings.groups.iter().find(|group| group.key == key).map(|group| group.name.clone()),
        ("group", "ar") => settings.groups.iter().find(|group| group.key == key).map(|group| group.title_ar.clone()),
        _ => None,
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

    #[test]
    fn settings_title_updates_override_older_design_text_but_later_text_still_wins() {
        let mut settings = Settings::default();
        let chapter = settings.chapters[0].key.clone();
        settings.chapters[0].title_fr = "Titre des paramètres".into();
        let chapters = serde_json::to_value(&settings.chapters).unwrap();
        let key = format!("chapter.{chapter}.fr");

        let settings_wins = Decisions::replay(&[
            Override::SetText { key: key.clone(), value: "Ancien titre de design".into() },
            Override::SetSetting { key: "chapters".into(), value: chapters.clone() },
        ]);
        assert_eq!(settings_wins.texts.get(&key).map(String::as_str), Some("Titre des paramètres"));

        let design_wins = Decisions::replay(&[
            Override::SetSetting { key: "chapters".into(), value: chapters },
            Override::SetText { key: key.clone(), value: "Titre modifié dans Design".into() },
        ]);
        assert_eq!(design_wins.texts.get(&key).map(String::as_str), Some("Titre modifié dans Design"));
    }

    #[test]
    fn group_title_settings_supersede_only_the_group_fields_that_changed() {
        let mut settings = Settings::default();
        let group = settings.groups[0].key.clone();
        let other_group = settings.groups[1].key.clone();
        settings.groups[0].name = "Nom mis à jour".into();
        settings.groups[0].title_ar = "عنوان محدث".into();
        let groups = serde_json::to_value(&settings.groups).unwrap();
        let french_key = format!("group.{group}.fr");
        let arabic_key = format!("group.{group}.ar");
        let other_key = format!("group.{other_group}.fr");

        let decisions = Decisions::replay(&[
            Override::SetText { key: french_key.clone(), value: "Ancien nom".into() },
            Override::SetText { key: arabic_key.clone(), value: "عنوان قديم".into() },
            Override::SetText { key: other_key.clone(), value: "Nom personnalisé".into() },
            Override::SetSetting { key: "groups".into(), value: groups },
        ]);

        assert_eq!(decisions.texts.get(&french_key).map(String::as_str), Some("Nom mis à jour"));
        assert_eq!(decisions.texts.get(&arabic_key).map(String::as_str), Some("عنوان محدث"));
        assert_eq!(decisions.texts.get(&other_key).map(String::as_str), Some("Nom personnalisé"));
    }

    #[test]
    fn removing_a_group_clears_edition_links_and_keeps_other_settings() {
        let mut settings = Settings::default();
        settings.groups.retain(|group| group.key != "bride_family");
        let decisions = Decisions::replay(&[Override::SetSetting {
            key: "groups".into(),
            value: serde_json::to_value(&settings.groups).unwrap(),
        }]);

        assert!(decisions.settings.edition("bride_parents").unwrap().group.is_none());
        assert_eq!(decisions.settings.groups.iter().find(|group| group.key == "groom_dad_family").unwrap().edition.as_deref(), Some("dad"));
        assert_eq!(decisions.settings.wedding_date, Settings::default().wedding_date);
        assert_eq!(decisions.settings.chapters.len(), Settings::default().chapters.len());
    }
}
