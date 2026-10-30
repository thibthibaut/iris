//! Domain types of the album (see CONTEXT.md). Pure data, no I/O.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type PhotoId = i64;
pub type FaceId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Photographer,
    Guest,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photographer => "photographer",
            Self::Guest => "guest",
        }
    }

    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "photographer" => Ok(Self::Photographer),
            "guest" => Ok(Self::Guest),
            _ => Err("unknown photo source"),
        }
    }
}

#[cfg(test)]
mod source_tests {
    use super::Source;

    #[test]
    fn parses_known_sources_and_rejects_unknown_values() {
        assert_eq!(Source::parse("photographer"), Ok(Source::Photographer));
        assert_eq!(Source::parse("guest"), Ok(Source::Guest));
        assert!(Source::parse("archive").is_err());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Bride,
    Groom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterDef {
    pub key: String,
    pub title_fr: String,
    #[serde(default)]
    pub title_ar: String,
    /// CLIP prompts describing the moment, used to split multi-Chapter folders and to
    /// place Guest Photos when there is no Photographer reference.
    #[serde(default)]
    pub prompts: Vec<String>,
    /// Relative weight when sharing the album's Spreads between Chapters.
    #[serde(default = "one")]
    pub weight: f64,
    /// Rough start time on the day ("HH:MM", Paris), used with guest send times.
    #[serde(default)]
    pub starts_at: Option<String>,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupDef {
    pub key: String,
    pub name: String,
    /// The Group Spread's title in Tunisian Arabic.
    #[serde(default)]
    pub title_ar: String,
    /// The Edition tied to this Group, if any (`friends` has none).
    pub edition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditionDef {
    pub key: String,
    pub name: String,
    /// `None` = everyone (`ours`).
    pub group: Option<String>,
}

/// Every setting of the album, edited from the GUI. Stored as defaults + `SetSetting`
/// Overrides, so every change can be undone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub wedding_date: String,
    pub chapters: Vec<ChapterDef>,
    /// Photographer folder → Chapter keys, in order.
    pub folder_chapters: BTreeMap<String, Vec<String>>,
    pub groups: Vec<GroupDef>,
    pub editions: Vec<EditionDef>,
    pub page_target: u32,
    pub shots_per_spread: f64,
    pub max_heroes: u32,
    pub couple_share: f64,
    pub group_spread_chapter: String,
    pub gate: Gate,
    pub shots: ShotSettings,
    pub face_min_height_frac: f64,
    pub face_min_cluster_size: usize,
    pub face_soft_distance: f64,
    pub printer: String,
    pub vlm: VlmSettings,
    pub design: Design,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gate {
    pub guest_min_quality: f64,
    pub guest_min_jpeg_q: f64,
    pub guest_min_megapixels: f64,
    pub photographer_min_quality: f64,
    pub screenshot_max: f64,
    pub min_face_sharpness: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShotSettings {
    pub max_hamming: u32,
    pub min_clip_cosine: f64,
    pub burst_seconds: i64,
    pub burst_sequence_gap: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VlmSettings {
    pub model: String,
    pub max_calls: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Design {
    pub paper: String,
    pub ink: String,
    pub emerald: String,
    pub red_orange: String,
    pub pink: String,
    pub yellow: String,
    pub blue: String,
    pub title_font: String,
    pub body_font: String,
    pub arabic_font: String,
    pub names: String,
    pub cover_date_fr: String,
    pub cover_date_ar: String,
}

impl Default for Settings {
    fn default() -> Self {
        let chapter = |key: &str, fr: &str, ar: &str, weight: f64, starts_at: &str, prompts: &[&str]| {
            ChapterDef {
                key: key.into(),
                title_fr: fr.into(),
                title_ar: ar.into(),
                prompts: prompts.iter().map(|p| (*p).to_string()).collect(),
                weight,
                starts_at: Some(starts_at.into()),
            }
        };
        let chapters = vec![
            chapter("prep_bride", "Les préparatifs de la mariée", "تحضيرات العروسة", 1.0, "09:00", &[
                "a bride getting ready, hair and make-up, in a bedroom",
                "a wedding dress hanging, bridal shoes and jewellery",
            ]),
            chapter("prep_groom", "Les préparatifs du marié", "تحضيرات العريس", 0.8, "10:00", &[
                "a groom getting dressed in a suit and tie",
                "men getting ready in an apartment before a wedding",
            ]),
            chapter("mairie_before", "Devant la mairie", "قدّام البلدية", 1.0, "13:15", &[
                "wedding guests gathering outside a town hall",
                "people waiting in a town hall hallway",
            ]),
            chapter("mairie_ceremony", "La cérémonie", "الكتبة", 1.4, "13:45", &[
                "a civil wedding ceremony inside a town hall wedding room",
                "a couple exchanging wedding rings at a town hall",
            ]),
            chapter("mairie_after", "La sortie de la mairie", "خرجة البلدية", 1.0, "14:30", &[
                "a newly married couple leaving a town hall under applause",
                "a large posed group photo of wedding guests on steps outside",
            ]),
            chapter("bnf_couple", "Séance à la BNF", "تصاور في المكتبة", 1.3, "15:30", &[
                "a couple posing for wedding portraits at a modern library",
            ]),
            chapter("cocktail", "Le cocktail", "الكوكتال", 1.0, "18:00", &[
                "wedding guests having drinks at an outdoor cocktail party",
            ]),
            chapter("dinner", "Le dîner", "العشا", 0.9, "20:30", &[
                "wedding guests seated at dinner tables in a restaurant",
                "speeches and toasts at a wedding dinner",
            ]),
            chapter("party", "La soirée", "السهرية", 1.0, "23:00", &[
                "people dancing at a wedding party at night",
                "a wedding cake being cut",
            ]),
        ];
        let folder = |name: &str, keys: &[&str]| {
            (name.to_string(), keys.iter().map(|k| (*k).to_string()).collect())
        };
        Self {
            wedding_date: "2025-07-19".into(),
            chapters,
            folder_chapters: BTreeMap::from([
                folder("preparatifs", &["prep_bride"]),
                folder("mairie", &["mairie_before", "mairie_ceremony", "mairie_after"]),
                folder("groupe", &["mairie_after"]),
                folder("couple", &["bnf_couple"]),
                folder("cocktail", &["cocktail"]),
                folder("diner et soiree", &["dinner", "party"]),
            ]),
            groups: vec![
                GroupDef { key: "bride_family".into(), name: "Famille de la mariée".into(), title_ar: "عايلة العروسة".into(), edition: Some("bride_parents".into()) },
                GroupDef { key: "groom_dad_family".into(), name: "Famille du papa du marié".into(), title_ar: "عايلة بابا العريس".into(), edition: Some("dad".into()) },
                GroupDef { key: "groom_mom_family".into(), name: "Famille de la maman du marié".into(), title_ar: "عايلة ماما العريس".into(), edition: Some("mom".into()) },
                GroupDef { key: "friends".into(), name: "Amis".into(), title_ar: "الصحاب".into(), edition: None },
            ],
            editions: vec![
                EditionDef { key: "ours".into(), name: "Nous".into(), group: None },
                EditionDef { key: "bride_parents".into(), name: "Parents de la mariée".into(), group: Some("bride_family".into()) },
                EditionDef { key: "dad".into(), name: "Papa".into(), group: Some("groom_dad_family".into()) },
                EditionDef { key: "mom".into(), name: "Maman".into(), group: Some("groom_mom_family".into()) },
            ],
            page_target: 48,
            shots_per_spread: 6.0,
            max_heroes: 3,
            couple_share: 0.7,
            group_spread_chapter: "mairie_after".into(),
            gate: Gate::default(),
            shots: ShotSettings::default(),
            face_min_height_frac: 0.02,
            face_min_cluster_size: 5,
            face_soft_distance: 0.9,
            printer: "flexilivre_a4_landscape_hard".into(),
            vlm: VlmSettings::default(),
            design: Design::default(),
        }
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            guest_min_quality: 0.55,
            guest_min_jpeg_q: 60.0,
            guest_min_megapixels: 1.5,
            photographer_min_quality: 0.35,
            screenshot_max: 0.5,
            min_face_sharpness: 0.15,
        }
    }
}

impl Default for ShotSettings {
    fn default() -> Self {
        Self {
            max_hamming: 8,
            min_clip_cosine: 0.95,
            burst_seconds: 10,
            burst_sequence_gap: 3,
        }
    }
}

impl Default for VlmSettings {
    fn default() -> Self {
        Self {
            model: "mistralai/mistral-medium-3".into(),
            max_calls: 400,
        }
    }
}

impl Default for Design {
    fn default() -> Self {
        Self {
            paper: "#FFFFFF".into(),
            ink: "#1E1E1E".into(),
            emerald: "#3E9C82".into(),
            red_orange: "#E05A3A".into(),
            pink: "#F27BA5".into(),
            yellow: "#F6C445".into(),
            blue: "#2F6FDE".into(),
            title_font: "Righteous".into(),
            body_font: "Playfair Display".into(),
            arabic_font: "Lalezar".into(),
            names: "Emna & Thibaut".into(),
            cover_date_fr: "19 juillet 2025 · Paris".into(),
            cover_date_ar: "19 جويلية 2025 · باريس".into(),
        }
    }
}

impl Settings {
    pub fn chapter_index(&self, key: &str) -> Option<usize> {
        self.chapters.iter().position(|chapter| chapter.key == key)
    }

    pub fn edition(&self, key: &str) -> Option<&EditionDef> {
        self.editions.iter().find(|edition| edition.key == key)
    }
}

/// One user decision. Overrides refer to Photos and faces, never to Shots, clusters or
/// Slots, so they survive every regeneration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Override {
    // People: each one names a face of the Person it acts on.
    NamePerson { face: FaceId, name: String },
    SetRole { face: FaceId, role: Option<Role> },
    SetGroup { face: FaceId, group: String, member: bool },
    SetQuota { face: FaceId, quota: u32 },
    MergePersons { face: FaceId, into_face: FaceId },
    SplitFaces { faces: Vec<FaceId> },
    AssignFace { face: FaceId, to_face: FaceId },
    RemoveFace { face: FaceId },
    // Photos
    Pin { photo: PhotoId, on: bool },
    Ban { photo: PhotoId, on: bool },
    Hero { photo: PhotoId, on: bool },
    MoveChapter { photo: PhotoId, chapter: String },
    SplitShot { photo: PhotoId },
    // Album
    LockSpread { spread: Spread },
    UnlockSpread { uid: u64 },
    SetChapterSpreads { chapter: String, count: u32 },
    MoveSpread { uid: u64, chapter: String, position: u32 },
    // Text and settings
    SetText { key: String, value: String },
    SetSetting { key: String, value: serde_json::Value },
}

/// A rectangle in millimetres, origin at the top-left of the Spread.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn aspect(&self) -> f64 {
        self.w / self.h
    }
}

/// The visible part of a Photo, as fractions of its upright width and height.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Crop {
    pub const FULL: Self = Self { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacedPhoto {
    pub photo: PhotoId,
    pub rect: Rect,
    pub crop: Crop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadKind {
    Core,
    Group,
}

/// Two facing Pages (594 × 210 mm). Chapter titles and Group Spreads carry text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spread {
    pub uid: u64,
    pub chapter: String,
    pub kind: SpreadKind,
    /// The Group of a Group Spread.
    #[serde(default)]
    pub group: Option<String>,
    /// Page template ids, left then right.
    pub templates: [String; 2],
    pub photos: Vec<PlacedPhoto>,
    /// Chapter title shown on this Spread (its first one).
    #[serde(default)]
    pub title: bool,
    #[serde(default)]
    pub locked: bool,
    /// Position inside its Chapter.
    #[serde(default)]
    pub position: u32,
}

impl Spread {
    pub fn in_edition(&self, edition: &EditionDef) -> bool {
        match (self.kind, &edition.group) {
            (SpreadKind::Core, _) => true,
            (SpreadKind::Group, None) => true,
            (SpreadKind::Group, Some(group)) => self.group.as_ref() == Some(group),
        }
    }
}

/// The generated album: the master sequence of Spreads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub spreads: Vec<Spread>,
    /// Per Slot alternatives for the candidate tray: photo → ranked alternatives.
    #[serde(default)]
    pub alternatives: BTreeMap<PhotoId, Vec<PhotoId>>,
    /// Persons whose Quota was not reached: (Person, Quota, Shots placed).
    #[serde(default)]
    pub unmet_quotas: Vec<(i64, u32, u32)>,
    /// Share of the selected Shots showing the Couple.
    #[serde(default)]
    pub couple_share: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeopleOverrideKind {
    Name,
    Role,
    Group,
    Quota,
    Merge,
    Split,
    Assign,
    Remove,
}

/// An Override that cannot apply to the album as it stands, shown where it bites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Conflict {
    /// A Banned Photo still printed because its Spread is Locked.
    BannedOnLockedSpread { photo: PhotoId, spread: u64 },
    /// A Chapter move still refers to a Photo that no longer exists.
    MoveChapterPhotoMissing { photo: PhotoId, chapter: String },
    /// A Chapter move names a Chapter that has since been removed or renamed.
    MoveChapterChapterMissing { photo: PhotoId, chapter: String },
    /// A Locked Spread still places a moved Photo in its previous Chapter.
    MoveChapterOnLockedSpread { photo: PhotoId, spread: u64, chapter: String },
    /// A People Override still refers to a face that no longer exists.
    PeopleOverrideFaceMissing { operation: PeopleOverrideKind, face: FaceId },
    /// A People Override refers to a target face that no longer exists.
    PeopleOverrideTargetMissing { operation: PeopleOverrideKind, face: FaceId, target: FaceId },
    /// A People Override refers to a detected face that no longer belongs to a Person.
    PeopleOverridePersonMissing { operation: PeopleOverrideKind, face: FaceId },
    /// A Group referenced by a People Override no longer exists.
    PeopleOverrideGroupMissing { face: FaceId, group: String },
    /// A Pinned Photo with no room: its Chapter is all Locked, or the layout could not fit it.
    PinNotPlaced { photo: PhotoId },
}

pub const PAGE_W: f64 = 297.0;
pub const PAGE_H: f64 = 210.0;
pub const SPREAD_W: f64 = PAGE_W * 2.0;
