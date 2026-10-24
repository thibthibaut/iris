//! The album as the user sees it: stored analysis + the Overrides replayed on top.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use iris_core::{
    decisions::{Decisions, Eligibility},
    model::{PhotoId, Role, Source},
    people::{FaceMembership, People},
    quality::{Evaluation, Measurements, evaluate},
};

use crate::db::{Database, FaceRow, PhotoRecord};

pub struct AlbumState {
    pub decisions: Decisions,
    pub people: People,
    pub faces: Vec<FaceRow>,
    pub photos: Vec<PhotoRecord>,
    pub by_id: HashMap<PhotoId, usize>,
    pub evaluations: HashMap<PhotoId, Evaluation>,
    pub faces_by_photo: HashMap<PhotoId, Vec<usize>>,
}

impl AlbumState {
    pub fn load(db: &Database) -> Result<Self> {
        let decisions = Decisions::replay(&db.active_overrides()?);
        let faces = db.faces()?;
        let memberships: Vec<FaceMembership> = faces
            .iter()
            .map(|face| FaceMembership { face: face.id, photo: face.photo_id, cluster: face.cluster, soft: face.soft })
            .collect();
        let people = iris_core::people::resolve(&memberships, &decisions.people);
        let photos = db.all_photos()?;
        let by_id = photos.iter().enumerate().map(|(i, photo)| (photo.id, i)).collect();
        let mut faces_by_photo: HashMap<PhotoId, Vec<usize>> = HashMap::new();
        for (index, face) in faces.iter().enumerate() {
            faces_by_photo.entry(face.photo_id).or_default().push(index);
        }
        let mut state = Self { decisions, people, faces, photos, by_id, evaluations: HashMap::new(), faces_by_photo };
        state.evaluations = state
            .photos
            .iter()
            .map(|photo| {
                let mut evaluation = evaluate(&state.measurements(photo), &state.decisions.settings.gate);
                if photo.blake3_hash.is_none() {
                    evaluation.rejected = Some("not read yet".into());
                }
                (photo.id, evaluation)
            })
            .collect();
        Ok(state)
    }

    pub fn photo(&self, id: PhotoId) -> Option<&PhotoRecord> {
        self.by_id.get(&id).map(|&i| &self.photos[i])
    }

    /// Faces big enough to count (not the background crowd).
    pub fn counted_faces(&self, photo: PhotoId) -> impl Iterator<Item = &FaceRow> {
        let min = self.decisions.settings.face_min_height_frac;
        self.faces_by_photo
            .get(&photo)
            .into_iter()
            .flatten()
            .map(|&i| &self.faces[i])
            .filter(move |face| face.height_frac() >= min)
    }

    /// Persons (by anchor face) visible in a Photo.
    pub fn persons_in(&self, photo: PhotoId) -> Vec<i64> {
        let mut anchors: Vec<i64> = self
            .counted_faces(photo)
            .filter_map(|face| self.people.person_of_face(face.id).map(|p| p.anchor))
            .collect();
        anchors.sort_unstable();
        anchors.dedup();
        anchors
    }

    pub fn couple_anchors(&self) -> HashSet<i64> {
        [Role::Bride, Role::Groom]
            .into_iter()
            .filter_map(|role| self.people.with_role(role).map(|p| p.anchor))
            .collect()
    }

    fn measurements(&self, photo: &PhotoRecord) -> Measurements {
        let couple = self.couple_anchors();
        let faces: Vec<&FaceRow> = self.counted_faces(photo.id).collect();
        let biggest = faces.iter().map(|f| f.height_frac()).fold(0.0, f64::max);
        let important = faces
            .iter()
            .filter(|face| {
                let in_couple = self.people.person_of_face(face.id).is_some_and(|p| couple.contains(&p.anchor));
                in_couple || face.height_frac() >= biggest * 0.6
            })
            .filter_map(|face| face.sharpness)
            .collect();
        Measurements {
            source: photo.source,
            tenengrad: photo.tenengrad,
            exposure: photo.exposure_score,
            clip_aesthetic: photo.clip_aesthetic,
            clip_screenshot: photo.clip_screenshot,
            jpeg_quality: photo.jpeg_q_est,
            megapixels: photo.megapixels(),
            important_face_sharpness: important,
        }
    }

    /// The Chapter after the user's moves.
    pub fn chapter_of(&self, photo: &PhotoRecord) -> Option<String> {
        self.decisions.chapter_moves.get(&photo.id).cloned().or_else(|| photo.chapter.clone())
    }

    pub fn eligibility(&self, photo: PhotoId) -> Eligibility {
        let rejected = self.evaluations.get(&photo).and_then(|e| e.rejected.as_deref());
        self.decisions.eligibility(photo, rejected)
    }

    pub fn is_photographer(&self, photo: &PhotoRecord) -> bool {
        photo.source == Some(Source::Photographer)
    }
}
