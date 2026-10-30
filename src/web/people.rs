//! People page: name Persons, mark the Couple, fill Groups, set Quotas, merge and split.

use std::collections::{HashMap, HashSet};

use askama::Template;
use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse, Response},
};
use iris_core::model::{GroupDef, GroupKey, Override, PersonId, Role};
use serde::Deserialize;

use super::{AppState, WebResult, blocking, render, toast};
use crate::state::AlbumState;

pub struct PersonCard {
    pub anchor: i64,
    pub name: String,
    pub named: bool,
    pub role: Option<Role>,
    pub groups: Vec<String>,
    pub quota: u32,
    pub photos: usize,
    pub soft: usize,
    pub in_album: Option<usize>,
}

impl PersonCard {
    pub fn below_quota(&self) -> bool {
        self.in_album.is_some_and(|count| count < self.quota as usize)
    }

    pub fn role_name(&self) -> &'static str {
        match self.role {
            Some(Role::Bride) => "Bride",
            Some(Role::Groom) => "Groom",
            None => "",
        }
    }
}

/// How many Shots of the album show each Person (any Edition).
pub fn album_counts(state: &AlbumState, db: &crate::db::Database) -> anyhow::Result<Option<HashMap<PersonId, usize>>> {
    let proposal: Option<iris_core::model::Proposal> = db.blob("proposal")?;
    Ok(proposal.map(|proposal| {
        let mut counts = HashMap::new();
        for spread in &proposal.spreads {
            for placed in &spread.photos {
                for anchor in state.persons_in(placed.photo) {
                    *counts.entry(anchor).or_insert(0) += 1;
                }
            }
        }
        counts
    }))
}

pub fn cards(state: &AlbumState, counts: &Option<HashMap<PersonId, usize>>) -> Vec<PersonCard> {
    let face_photo: HashMap<i64, i64> = state.faces.iter().map(|f| (f.id, f.photo_id)).collect();
    let mut cards: Vec<PersonCard> = state
        .people
        .persons
        .iter()
        .map(|person| {
            let photos: HashSet<i64> = person.faces.iter().filter_map(|f| face_photo.get(f).copied()).collect();
            PersonCard {
                anchor: person.anchor.0,
                name: person.name.clone().unwrap_or_default(),
                named: person.name.is_some(),
                role: person.role,
                groups: person.groups.iter().map(|group| group.as_str().to_owned()).collect(),
                quota: person.quota,
                photos: photos.len(),
                soft: person.soft_faces.len(),
                in_album: counts.as_ref().map(|c| c.get(&person.anchor).copied().unwrap_or(0)),
            }
        })
        .collect();
    cards.sort_by(|a, b| b.role.is_some().cmp(&a.role.is_some()).then(b.named.cmp(&a.named)).then(b.photos.cmp(&a.photos)));
    cards
}

#[derive(Template)]
#[template(path = "people.html")]
struct PeoplePage {
    nav: &'static str,
    board: Board,
}

#[derive(Template)]
#[template(path = "people_board.html")]
struct Board {
    persons: Vec<PersonCard>,
    groups: Vec<GroupDef>,
    bride: Option<String>,
    groom: Option<String>,
    to_confirm: usize,
    unnamed: usize,
    has_album: bool,
}

impl Board {
    fn group_name<'a>(&'a self, key: &'a str) -> &'a str {
        self.groups.iter().find(|g| g.key.as_str() == key).map_or(key, |g| g.name.as_str())
    }

    fn members(&self, group: &str) -> Vec<&PersonCard> {
        self.persons.iter().filter(|p| p.groups.iter().any(|g| g.as_str() == group)).collect()
    }
}

fn board(state: &AppState) -> anyhow::Result<Board> {
    let db = state.db()?;
    let snapshot = state.snapshot(&db)?;
    let counts = album_counts(&snapshot, &db)?;
    let persons = cards(&snapshot, &counts);
    let name_of = |role| snapshot.people.with_role(role).map(|p| p.name.clone().unwrap_or_else(|| format!("Person {}", p.anchor)));
    Ok(Board {
        bride: name_of(Role::Bride),
        groom: name_of(Role::Groom),
        to_confirm: persons.iter().map(|p| p.soft).sum(),
        unnamed: persons.iter().filter(|p| !p.named).count(),
        groups: snapshot.decisions.settings.groups.clone(),
        has_album: counts.is_some(),
        persons,
    })
}

pub async fn page(State(state): State<AppState>) -> WebResult<Html<String>> {
    let board = blocking(move || board(&state)).await?;
    render(&PeoplePage { nav: "people", board })
}

pub async fn board_fragment(State(state): State<AppState>) -> WebResult<Html<String>> {
    let board = blocking(move || board(&state)).await?;
    render(&board)
}

#[derive(Deserialize)]
pub struct PersonForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    member: String,
    #[serde(default)]
    quota: String,
    /// Comma-separated anchors (merge) or face ids (split).
    #[serde(default)]
    ids: String,
}

/// One People decision, then the refreshed board.
pub async fn act(
    State(state): State<AppState>,
    Path((anchor, action)): Path<(i64, String)>,
    axum::Form(form): axum::Form<PersonForm>,
) -> WebResult<Response> {
    let (board, label) = blocking(move || {
        let db = state.db()?;
        let snapshot = state.snapshot(&db)?;
        let person = snapshot.people.by_anchor(PersonId::from(anchor));
        let who = person.and_then(|p| p.name.clone()).unwrap_or_else(|| format!("Person {anchor}"));
        let (item, label) = match action.as_str() {
            "name" => {
                let name = form.name.trim().to_string();
                (Override::NamePerson { face: anchor, name: name.clone() }, format!("Named {who} “{name}”"))
            }
            "role" => {
                let role = match form.role.as_str() {
                    "bride" => Some(Role::Bride),
                    "groom" => Some(Role::Groom),
                    _ => None,
                };
                let label = match role {
                    Some(Role::Bride) => format!("{who} is the bride"),
                    Some(Role::Groom) => format!("{who} is the groom"),
                    None => format!("{who} is a guest"),
                };
                (Override::SetRole { face: anchor, role }, label)
            }
            "group" => {
                let member = form.member != "false";
                let group = GroupKey::from(form.group.as_str());
                let group_name = snapshot
                    .decisions
                    .settings
                    .groups
                    .iter()
                    .find(|g| g.key == group)
                    .map(|g| g.name.clone())
                    .ok_or_else(|| anyhow::anyhow!("unknown group {group}"))?;
                let label = if member { format!("Added {who} to {group_name}") } else { format!("Removed {who} from {group_name}") };
                (Override::SetGroup { face: anchor, group, member }, label)
            }
            "quota" => {
                let quota: u32 = form.quota.trim().parse().unwrap_or(0);
                (Override::SetQuota { face: anchor, quota }, format!("Quota of {who} → {quota}"))
            }
            "merge" => {
                let others: Vec<i64> = form.ids.split(',').filter_map(|id| id.trim().parse().ok()).filter(|&id| id != anchor).collect();
                anyhow::ensure!(!others.is_empty(), "select at least two Persons to merge");
                for other in &others {
                    db.add_override(&Override::MergePersons { face: *other, into_face: anchor }, &format!("Merged Person {other} into {who}"))?;
                }
                state.jobs().notify("overrides");
                return Ok((board(&state)?, format!("Merged {} Persons into {who}", others.len())));
            }
            _ => anyhow::bail!("unknown action {action}"),
        };
        state.decide(&db, item, &label)?;
        Ok((board(&state)?, label))
    })
    .await?;
    Ok(toast(render(&board)?, &label))
}

// ---- One Person -----------------------------------------------------------------------

#[derive(Template)]
#[template(path = "person.html")]
struct PersonPage {
    nav: &'static str,
    card: PersonCard,
    faces: Vec<(i64, i64, bool)>,
    groups: Vec<GroupDef>,
}

fn person_page(state: &AppState, anchor: i64) -> anyhow::Result<Option<PersonPage>> {
    let db = state.db()?;
    let snapshot = state.snapshot(&db)?;
    let Some(person) = snapshot.people.by_anchor(PersonId::from(anchor)).cloned() else { return Ok(None) };
    let counts = album_counts(&snapshot, &db)?;
    let card = cards(&snapshot, &counts).into_iter().find(|c| c.anchor == anchor).expect("person has a card");
    let face_photo: HashMap<i64, i64> = snapshot.faces.iter().map(|f| (f.id, f.photo_id)).collect();
    let mut faces: Vec<(i64, i64, bool)> = person
        .faces
        .iter()
        .map(|face| (*face, face_photo.get(face).copied().unwrap_or(0), person.soft_faces.contains(face)))
        .collect();
    // Faces to confirm first.
    faces.sort_by_key(|(_, _, soft)| !soft);
    Ok(Some(PersonPage { nav: "people", card, faces, groups: snapshot.decisions.settings.groups.clone() }))
}

pub async fn person(State(state): State<AppState>, Path(anchor): Path<i64>) -> WebResult<Response> {
    match blocking(move || person_page(&state, anchor)).await? {
        Some(page) => Ok(render(&page)?.into_response()),
        None => Ok(axum::response::Redirect::to("/people").into_response()),
    }
}

#[derive(Deserialize)]
pub struct FacesForm {
    #[serde(default)]
    faces: String,
    #[serde(default)]
    anchor: i64,
}

/// Face-level decisions on a Person page: confirm, remove, split.
pub async fn faces(State(state): State<AppState>, Path(action): Path<String>, axum::Form(form): axum::Form<FacesForm>) -> WebResult<Response> {
    let (target, label) = blocking(move || {
        let db = state.db()?;
        let faces: Vec<i64> = form.faces.split(',').filter_map(|f| f.trim().parse().ok()).collect();
        anyhow::ensure!(!faces.is_empty(), "select faces first");
        let count = faces.len();
        let label = match action.as_str() {
            "confirm" => {
                for face in &faces {
                    db.add_override(&Override::AssignFace { face: *face, to_face: form.anchor }, "Confirmed a face")?;
                }
                format!("Confirmed {count} faces")
            }
            "remove" => {
                for face in &faces {
                    db.add_override(&Override::RemoveFace { face: *face }, "Removed a face")?;
                }
                format!("Removed {count} faces from this Person")
            }
            "split" => {
                db.add_override(&Override::SplitFaces { faces: faces.clone() }, &format!("Split {count} faces into a new Person"))?;
                format!("Split {count} faces into a new Person")
            }
            _ => anyhow::bail!("unknown action {action}"),
        };
        state.jobs().notify("overrides");
        // The anchor may have moved (its own face was removed): find the Person again.
        let snapshot = state.snapshot(&db)?;
        Ok((snapshot.people.person_of_face(form.anchor).map(|p| p.anchor.0), label))
    })
    .await?;
    let location = target.map_or_else(|| "/people".to_string(), |anchor| format!("/people/{anchor}"));
    Ok((
        [("HX-Location", location), ("HX-Trigger", serde_json::json!({ "toast": label }).to_string())],
        Html(String::new()),
    )
        .into_response())
}
