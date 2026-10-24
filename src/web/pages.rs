//! Settings, undo/redo and the decision history.

use askama::Template;
use axum::{
    extract::State,
    response::{Html, IntoResponse, Response},
};
use iris_core::model::{Override, Settings};
use serde::Deserialize;

use super::{AppState, WebResult, blocking, pipeline::ago, render, toast};

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    nav: &'static str,
    s: Settings,
    folders: Vec<(String, String)>,
    json: String,
}

pub async fn settings(State(state): State<AppState>) -> WebResult<Html<String>> {
    let page = blocking(move || {
        let db = state.db()?;
        let s = state.snapshot(&db)?.decisions.settings.clone();
        Ok(SettingsPage {
            nav: "settings",
            folders: s.folder_chapters.iter().map(|(folder, keys)| (folder.clone(), keys.join(", "))).collect(),
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

pub async fn save_setting(State(state): State<AppState>, axum::Form(form): axum::Form<SettingForm>) -> WebResult<Response> {
    let label = blocking(move || {
        let db = state.db()?;
        let value = match form.kind.as_str() {
            "number" => serde_json::json!(form.value.trim().replace(',', ".").parse::<f64>()?),
            "list" => serde_json::json!(
                form.value.split(',').map(str::trim).filter(|v| !v.is_empty()).collect::<Vec<_>>()
            ),
            "bool" => serde_json::json!(form.value == "true" || form.value == "on"),
            "json" => {
                // Importing a whole settings file: one Override per top-level key.
                let parsed: serde_json::Value = serde_json::from_str(&form.value)?;
                let _: Settings = serde_json::from_value(parsed.clone())?;
                let mut count = 0;
                for (key, value) in parsed.as_object().into_iter().flatten() {
                    state.decide(&db, Override::SetSetting { key: key.clone(), value: value.clone() }, &format!("Imported setting {key}"))?;
                    count += 1;
                }
                return Ok(format!("Imported {count} settings"));
            }
            _ => serde_json::json!(form.value),
        };
        let label = format!("Set {} to {}", form.key, form.value.trim());
        state.decide(&db, Override::SetSetting { key: form.key, value }, &label)?;
        Ok(label)
    })
    .await?;
    Ok(toast(Html(String::new()), &label))
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
