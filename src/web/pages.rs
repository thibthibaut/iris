//! Settings, undo/redo and the decision history.

use anyhow::Result;
use askama::Template;
use axum::{
    extract::State,
    response::{Html, IntoResponse, Response},
};
use iris_core::model::{Override, Settings};
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{AppState, WebResult, blocking, pipeline::ago, render, toast};

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    nav: &'static str,
    s: Settings,
    folders: Vec<(String, String)>,
    chapter_rows: Vec<ChapterSettingRow>,
    group_rows: Vec<GroupSettingRow>,
    edition_rows: Vec<EditionSettingRow>,
    json: String,
}

struct ChapterSettingRow {
    key: String,
    title_fr: String,
    title_ar: String,
    prompts: String,
    weight: f64,
}

struct GroupSettingRow {
    key: String,
    name: String,
    title_ar: String,
    edition: String,
}

struct EditionSettingRow {
    key: String,
    name: String,
    group: String,
}

pub async fn settings(State(state): State<AppState>) -> WebResult<Html<String>> {
    let page = blocking(move || {
        let db = state.db()?;
        let mut s = state.snapshot(&db)?.decisions.settings.clone();
        s.normalize_group_editions();
        let chapter_rows = s
            .chapters
            .iter()
            .map(|chapter| ChapterSettingRow {
                key: chapter.key.clone(),
                title_fr: chapter.title_fr.clone(),
                title_ar: chapter.title_ar.clone(),
                prompts: chapter.prompts.join("\n"),
                weight: chapter.weight,
            })
            .collect();
        let group_rows = s
            .groups
            .iter()
            .map(|group| GroupSettingRow {
                key: group.key.clone(),
                name: group.name.clone(),
                title_ar: group.title_ar.clone(),
                edition: group
                    .edition
                    .as_deref()
                    .and_then(|key| s.edition(key))
                    .map(|edition| edition.name.clone())
                    .unwrap_or_default(),
            })
            .collect();
        let edition_rows = s
            .editions
            .iter()
            .map(|edition| EditionSettingRow {
                key: edition.key.clone(),
                name: edition.name.clone(),
                group: edition.group.clone().unwrap_or_default(),
            })
            .collect();
        Ok(SettingsPage {
            nav: "settings",
            folders: s
                .folder_chapters
                .iter()
                .map(|(folder, keys)| (folder.clone(), keys.join(", ")))
                .collect(),
            chapter_rows,
            group_rows,
            edition_rows,
            json: serde_json::to_string_pretty(&s)?,
            s,
        })
    })
    .await?;
    render(&page)
}

#[derive(Deserialize)]
pub struct SettingForm {
    key: String,
    value: String,
    /// number | text | list | bool | json
    #[serde(default)]
    kind: String,
}

const BODY_FONTS: &[&str] = &[
    "Playfair Display",
    "Righteous",
    "Bungee",
    "Monoton",
    "Shrikhand",
];

pub async fn save_setting(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<SettingForm>,
) -> WebResult<Response> {
    let label = blocking(move || {
        let db = state.db()?;
        let value = match form.kind.as_str() {
            "number" => serde_json::json!(form.value.trim().replace(',', ".").parse::<f64>()?),
            "date" => {
                chrono::NaiveDate::parse_from_str(&form.value, "%Y-%m-%d")?;
                serde_json::json!(form.value)
            }
            "font" => {
                anyhow::ensure!(
                    BODY_FONTS.contains(&form.value.as_str()),
                    "choose a bundled print font"
                );
                serde_json::json!(form.value)
            }
            "list" => serde_json::json!(
                form.value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .collect::<Vec<_>>()
            ),
            "bool" => serde_json::json!(form.value == "true" || form.value == "on"),
            "json" => {
                // Importing a whole settings file: one Override per top-level key.
                let parsed: serde_json::Value = serde_json::from_str(&form.value)?;
                let _: Settings = serde_json::from_value(parsed.clone())?;
                let mut count = 0;
                for (key, value) in parsed.as_object().into_iter().flatten() {
                    state.decide(
                        &db,
                        Override::SetSetting {
                            key: key.clone(),
                            value: value.clone(),
                        },
                        &format!("Imported setting {key}"),
                    )?;
                    count += 1;
                }
                return Ok(format!("Imported {count} settings"));
            }
            _ => serde_json::json!(form.value),
        };
        let label = format!("Set {} to {}", form.key, form.value.trim());
        state.decide(
            &db,
            Override::SetSetting {
                key: form.key,
                value,
            },
            &label,
        )?;
        Ok(label)
    })
    .await?;
    Ok(toast(Html(String::new()), &label))
}

#[derive(Deserialize, Default)]
pub struct DefinitionForm {
    entity: String,
    action: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    title_fr: String,
    #[serde(default)]
    title_ar: String,
    #[serde(default)]
    prompts: String,
    #[serde(default)]
    weight: String,
    #[serde(default)]
    group: String,
}

pub async fn save_definition(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<DefinitionForm>,
) -> WebResult<Response> {
    let label = blocking(move || {
        let db = state.db()?;
        let mut settings = state.snapshot(&db)?.decisions.settings.clone();
        let (setting, label) = apply_definition_edit(&mut settings, &form)?;
        let value = match setting {
            "chapters" => serde_json::to_value(&settings.chapters)?,
            "groups" => serde_json::to_value(&settings.groups)?,
            "editions" => serde_json::to_value(&settings.editions)?,
            _ => unreachable!("definition editor only changes a collection"),
        };
        state.decide(
            &db,
            Override::SetSetting {
                key: setting.into(),
                value,
            },
            &label,
        )?;
        Ok(label)
    })
    .await?;
    Ok(toast(Html(String::new()), &label))
}

fn apply_definition_edit(
    settings: &mut Settings,
    form: &DefinitionForm,
) -> Result<(&'static str, String)> {
    let label = match (form.entity.as_str(), form.action.as_str()) {
        ("chapter", "save") => {
            let title_fr = checked_text(&form.title_fr, "French Chapter title", 160, true)?;
            let title_ar = checked_text(&form.title_ar, "Arabic Chapter title", 160, false)?;
            let prompts: Vec<String> = form
                .prompts
                .lines()
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
                .map(str::to_string)
                .collect();
            anyhow::ensure!(
                prompts.len() <= 20,
                "enter at most 20 CLIP prompts per Chapter"
            );
            anyhow::ensure!(
                prompts
                    .iter()
                    .all(|prompt| prompt.chars().count() <= 240
                        && !prompt.chars().any(char::is_control)),
                "each CLIP prompt must be at most 240 characters"
            );
            let weight = form.weight.trim().replace(',', ".").parse::<f64>()?;
            anyhow::ensure!(
                weight.is_finite() && weight > 0.0,
                "Chapter weight must be a finite number greater than zero"
            );
            let chapter = settings
                .chapters
                .iter_mut()
                .find(|chapter| chapter.key == form.key)
                .ok_or_else(|| anyhow::anyhow!("Chapter not found"))?;
            chapter.title_fr = title_fr;
            chapter.title_ar = title_ar;
            chapter.prompts = prompts;
            chapter.weight = weight;
            format!("Updated Chapter {}", chapter.key)
        }
        ("group", "add") => {
            let name = checked_text(&form.name, "Group name", 120, true)?;
            let title_ar = checked_text(&form.title_ar, "Arabic Group Spread title", 160, false)?;
            let key = fresh_key(
                "group",
                &settings
                    .groups
                    .iter()
                    .map(|group| group.key.clone())
                    .collect::<Vec<_>>(),
            );
            settings.groups.push(iris_core::model::GroupDef {
                key: key.clone(),
                name,
                title_ar,
                edition: None,
            });
            format!("Added Group {key}")
        }
        ("group", "save") => {
            let name = checked_text(&form.name, "Group name", 120, true)?;
            let title_ar = checked_text(&form.title_ar, "Arabic Group Spread title", 160, false)?;
            let group = settings
                .groups
                .iter_mut()
                .find(|group| group.key == form.key)
                .ok_or_else(|| anyhow::anyhow!("Group not found"))?;
            group.name = name;
            group.title_ar = title_ar;
            format!("Updated Group {}", group.key)
        }
        ("group", "remove") => {
            let before = settings.groups.len();
            settings.groups.retain(|group| group.key != form.key);
            anyhow::ensure!(settings.groups.len() < before, "Group not found");
            format!("Removed Group {}", form.key)
        }
        ("edition", "add") => {
            let name = checked_text(&form.name, "Edition name", 120, true)?;
            let key = fresh_key(
                "edition",
                &settings
                    .editions
                    .iter()
                    .map(|edition| edition.key.clone())
                    .collect::<Vec<_>>(),
            );
            let group = selected_group(settings, &form.group)?;
            settings.editions.push(iris_core::model::EditionDef {
                key: key.clone(),
                name,
                group: None,
            });
            associate_group(settings, &key, group)?;
            format!("Added Edition {key}")
        }
        ("edition", "save") => {
            let name = checked_text(&form.name, "Edition name", 120, true)?;
            let group = selected_group(settings, &form.group)?;
            anyhow::ensure!(
                settings
                    .editions
                    .iter()
                    .any(|edition| edition.key == form.key),
                "Edition not found"
            );
            anyhow::ensure!(
                form.key != "ours" || group.is_none(),
                "the everyone Edition cannot be tied to one Group"
            );
            let edition = settings
                .editions
                .iter_mut()
                .find(|edition| edition.key == form.key)
                .ok_or_else(|| anyhow::anyhow!("Edition not found"))?;
            edition.name = name;
            associate_group(settings, &form.key, group)?;
            format!("Updated Edition {}", form.key)
        }
        ("edition", "remove") => {
            anyhow::ensure!(form.key != "ours", "the everyone Edition is required");
            let before = settings.editions.len();
            settings.editions.retain(|edition| edition.key != form.key);
            anyhow::ensure!(settings.editions.len() < before, "Edition not found");
            format!("Removed Edition {}", form.key)
        }
        _ => anyhow::bail!("unsupported settings definition change"),
    };

    settings.normalize_group_editions();
    let setting = match form.entity.as_str() {
        "chapter" => "chapters",
        "group" => "groups",
        "edition" => "editions",
        _ => unreachable!("unsupported definition entity already rejected"),
    };
    Ok((setting, label))
}

fn checked_text(value: &str, label: &str, max_chars: usize, required: bool) -> Result<String> {
    let value = value.trim();
    anyhow::ensure!(!required || !value.is_empty(), "{label} cannot be empty");
    anyhow::ensure!(
        value.chars().count() <= max_chars,
        "{label} must be at most {max_chars} characters"
    );
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "{label} cannot contain control characters"
    );
    Ok(value.to_string())
}

fn fresh_key(prefix: &str, used: &[String]) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = format!("{prefix}_{now}");
    let mut key = base.clone();
    let mut suffix = 2;
    while used.iter().any(|existing| existing == &key) {
        key = format!("{base}_{suffix}");
        suffix += 1;
    }
    key
}

fn selected_group(settings: &Settings, value: &str) -> Result<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        settings.groups.iter().any(|group| group.key == value),
        "choose an existing Group"
    );
    Ok(Some(value.to_string()))
}

fn associate_group(
    settings: &mut Settings,
    edition_key: &str,
    group: Option<String>,
) -> Result<()> {
    anyhow::ensure!(
        edition_key != "ours" || group.is_none(),
        "the everyone Edition cannot be tied to one Group"
    );
    if let Some(group) = group.as_deref() {
        anyhow::ensure!(
            settings
                .groups
                .iter()
                .any(|candidate| candidate.key == group),
            "choose an existing Group"
        );
        for edition in &mut settings.editions {
            if edition.key != edition_key && edition.group.as_deref() == Some(group) {
                edition.group = None;
            }
        }
    }
    let edition = settings
        .editions
        .iter_mut()
        .find(|edition| edition.key == edition_key)
        .ok_or_else(|| anyhow::anyhow!("Edition not found"))?;
    edition.group = group;
    Ok(())
}

pub async fn undo(State(state): State<AppState>) -> WebResult<Response> {
    let label = blocking(move || {
        let label = state.db()?.undo()?;
        state.jobs().notify("overrides");
        Ok(label)
    })
    .await?;
    let message = label.map_or("Nothing to undo".into(), |label| format!("Undid: {label}"));
    Ok(toast(Html(String::new()), &message))
}

pub async fn redo(State(state): State<AppState>) -> WebResult<Response> {
    let label = blocking(move || {
        let label = state.db()?.redo()?;
        state.jobs().notify("overrides");
        Ok(label)
    })
    .await?;
    let message = label.map_or("Nothing to redo".into(), |label| format!("Redid: {label}"));
    Ok(toast(Html(String::new()), &message))
}

#[derive(Template)]
#[template(path = "history.html")]
struct History {
    rows: Vec<(String, String, bool)>,
}

pub async fn history(State(state): State<AppState>) -> WebResult<Response> {
    let rows = blocking(move || {
        Ok(state
            .db()?
            .override_rows(Some(40))?
            .into_iter()
            .map(|row| (row.label, ago(row.ts_ms), row.undone))
            .collect())
    })
    .await?;
    Ok(render(&History { rows })?.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(entity: &str, action: &str) -> DefinitionForm {
        DefinitionForm {
            entity: entity.into(),
            action: action.into(),
            ..DefinitionForm::default()
        }
    }

    #[test]
    fn chapter_edits_keep_timing_and_folder_settings() {
        let mut settings = Settings::default();
        let chapter_key = settings.chapters[0].key.clone();
        let starts_at = settings.chapters[0].starts_at.clone();
        let folders = settings.folder_chapters.clone();
        let wedding_date = settings.wedding_date.clone();
        let mut change = form("chapter", "save");
        change.key = chapter_key.clone();
        change.title_fr = "Titre en français".into();
        change.title_ar = "عنوان بالعربي".into();
        change.prompts = "first prompt\n\nsecond prompt".into();
        change.weight = "1.75".into();

        let (key, _) = apply_definition_edit(&mut settings, &change).unwrap();

        assert_eq!(key, "chapters");
        let chapter = settings
            .chapters
            .iter()
            .find(|chapter| chapter.key == chapter_key)
            .unwrap();
        assert_eq!(chapter.title_fr, "Titre en français");
        assert_eq!(chapter.title_ar, "عنوان بالعربي");
        assert_eq!(
            chapter.prompts,
            vec!["first prompt".to_string(), "second prompt".to_string()]
        );
        assert_eq!(chapter.weight, 1.75);
        assert_eq!(chapter.starts_at, starts_at);
        assert_eq!(settings.folder_chapters, folders);
        assert_eq!(settings.wedding_date, wedding_date);
    }

    #[test]
    fn an_invalid_chapter_weight_does_not_partially_update_the_settings() {
        let mut settings = Settings::default();
        let before = settings.clone();
        let mut change = form("chapter", "save");
        change.key = settings.chapters[0].key.clone();
        change.title_fr = "Changed title".into();
        change.title_ar = "عنوان جديد".into();
        change.weight = "NaN".into();

        assert!(apply_definition_edit(&mut settings, &change).is_err());
        assert_eq!(settings, before);
    }

    #[test]
    fn removing_a_group_clears_its_edition_reference() {
        let mut settings = Settings::default();
        let mut change = form("group", "remove");
        change.key = "bride_family".into();

        let (key, _) = apply_definition_edit(&mut settings, &change).unwrap();

        assert_eq!(key, "groups");
        assert!(
            !settings
                .groups
                .iter()
                .any(|group| group.key == "bride_family")
        );
        assert!(settings.edition("bride_parents").unwrap().group.is_none());
        assert_eq!(
            settings.edition("dad").unwrap().group.as_deref(),
            Some("groom_dad_family")
        );
    }

    #[test]
    fn added_group_and_edition_share_consistent_generated_keys() {
        let mut settings = Settings::default();
        let mut add_group = form("group", "add");
        add_group.name = "Les amis proches".into();
        add_group.title_ar = "أصحابنا".into();
        apply_definition_edit(&mut settings, &add_group).unwrap();
        let group_key = settings.groups.last().unwrap().key.clone();
        assert!(group_key.starts_with("group_"));

        let mut add_edition = form("edition", "add");
        add_edition.name = "Amis proches".into();
        add_edition.group = group_key.clone();
        apply_definition_edit(&mut settings, &add_edition).unwrap();
        let edition_key = settings.editions.last().unwrap().key.clone();

        assert!(edition_key.starts_with("edition_"));
        assert_eq!(
            settings.edition(&edition_key).unwrap().group.as_deref(),
            Some(group_key.as_str())
        );
        assert_eq!(
            settings.groups.last().unwrap().edition.as_deref(),
            Some(edition_key.as_str())
        );
    }

    #[test]
    fn moving_an_edition_group_keeps_a_single_reciprocal_association() {
        let mut settings = Settings::default();
        let mut change = form("edition", "save");
        change.key = "mom".into();
        change.name = "Maman".into();
        change.group = "groom_dad_family".into();

        let (key, _) = apply_definition_edit(&mut settings, &change).unwrap();

        assert_eq!(key, "editions");
        assert!(settings.edition("dad").unwrap().group.is_none());
        assert_eq!(
            settings.edition("mom").unwrap().group.as_deref(),
            Some("groom_dad_family")
        );
        assert_eq!(
            settings
                .groups
                .iter()
                .find(|group| group.key == "groom_dad_family")
                .unwrap()
                .edition
                .as_deref(),
            Some("mom")
        );
    }
}
