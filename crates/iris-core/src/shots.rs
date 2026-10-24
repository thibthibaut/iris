//! Shots: Photos that show the same picture (copies, forwards, bursts).

use std::collections::{BTreeMap, HashSet};

use crate::model::{PhotoId, ShotSettings};

#[derive(Debug, Clone, Default)]
pub struct ShotInput {
    pub photo: PhotoId,
    pub phash: Option<u64>,
    /// Unit-length CLIP embedding.
    pub embedding: Option<Vec<f32>>,
    /// Capture time, or for guests the send time (forwards of a burst arrive together).
    pub time_ms: Option<i64>,
    /// Camera file number, photographer only.
    pub sequence: Option<i64>,
    pub photographer: bool,
    pub pixels: f64,
    pub jpeg_quality: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    /// Smallest Photo id of the Shot.
    pub id: i64,
    pub members: Vec<PhotoId>,
    /// The copy used in the album: biggest, least compressed, photographer first.
    pub representative: PhotoId,
}

/// Two Photos are the same Shot when their perceptual hashes are close (a copy), or when
/// they look alike and were taken within a burst (close in time or in camera file number).
/// Photos the user split out stay alone.
pub fn group_shots(photos: &[ShotInput], settings: &ShotSettings, splits: &HashSet<PhotoId>) -> Vec<Shot> {
    let mut parent: Vec<usize> = (0..photos.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    for i in 0..photos.len() {
        if splits.contains(&photos[i].photo) {
            continue;
        }
        for j in i + 1..photos.len() {
            if !splits.contains(&photos[j].photo) && same_shot(&photos[i], &photos[j], settings) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..photos.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    groups
        .into_values()
        .map(|indices| {
            let best = indices
                .iter()
                .copied()
                .max_by(|&a, &b| {
                    let (a, b) = (&photos[a], &photos[b]);
                    a.pixels
                        .total_cmp(&b.pixels)
                        .then(a.jpeg_quality.unwrap_or(100.0).total_cmp(&b.jpeg_quality.unwrap_or(100.0)))
                        .then(a.photographer.cmp(&b.photographer))
                        .then(b.photo.cmp(&a.photo))
                })
                .expect("groups are never empty");
            let mut members: Vec<PhotoId> = indices.iter().map(|&i| photos[i].photo).collect();
            members.sort_unstable();
            Shot { id: members[0], representative: photos[best].photo, members }
        })
        .collect()
}

fn same_shot(a: &ShotInput, b: &ShotInput, settings: &ShotSettings) -> bool {
    if let (Some(x), Some(y)) = (a.phash, b.phash)
        && (x ^ y).count_ones() <= settings.max_hamming
    {
        return true;
    }
    let similar = match (&a.embedding, &b.embedding) {
        (Some(x), Some(y)) => f64::from(x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>()) >= settings.min_clip_cosine,
        _ => false,
    };
    if !similar {
        return false;
    }
    let same_burst_sequence = a.photographer
        && b.photographer
        && matches!((a.sequence, b.sequence), (Some(x), Some(y)) if (x - y).abs() <= settings.burst_sequence_gap);
    let same_burst_time = a.photographer == b.photographer
        && matches!((a.time_ms, b.time_ms), (Some(x), Some(y)) if (x - y).abs() <= settings.burst_seconds * 1000);
    same_burst_sequence || same_burst_time
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(angle: f32) -> Vec<f32> {
        vec![angle.cos(), angle.sin()]
    }

    fn members(shots: &[Shot]) -> Vec<Vec<PhotoId>> {
        let mut out: Vec<_> = shots.iter().map(|shot| shot.members.clone()).collect();
        out.sort();
        out
    }

    #[test]
    fn copies_and_bursts_share_a_shot() {
        let settings = ShotSettings::default();
        let photos = vec![
            // 1 and 2: a WhatsApp forward of the same picture, 2 recompressed.
            ShotInput { photo: 1, phash: Some(0xF0F0), embedding: Some(unit(0.0)), pixels: 3e6, jpeg_quality: Some(92.0), ..Default::default() },
            ShotInput { photo: 2, phash: Some(0xF0F1), embedding: Some(unit(0.01)), pixels: 1e6, jpeg_quality: Some(70.0), ..Default::default() },
            // 3 and 4: two photographer frames one file number apart.
            ShotInput { photo: 3, phash: Some(0x0FFF_0000), embedding: Some(unit(1.0)), sequence: Some(100), photographer: true, pixels: 36e6, ..Default::default() },
            ShotInput { photo: 4, phash: Some(0x00FF_FF00), embedding: Some(unit(1.05)), sequence: Some(101), photographer: true, pixels: 36e6, ..Default::default() },
            // 5: looks like 3 but taken much later: another Shot.
            ShotInput { photo: 5, phash: Some(0xFFFF_FFFF_0000), embedding: Some(unit(1.02)), sequence: Some(180), photographer: true, pixels: 36e6, ..Default::default() },
        ];

        let shots = group_shots(&photos, &settings, &HashSet::new());

        assert_eq!(members(&shots), vec![vec![1, 2], vec![3, 4], vec![5]]);
        let copy = shots.iter().find(|shot| shot.members.contains(&1)).unwrap();
        assert_eq!(copy.representative, 1);
    }

    #[test]
    fn a_split_photo_gets_its_own_shot() {
        let settings = ShotSettings::default();
        let photos = vec![
            ShotInput { photo: 1, phash: Some(7), ..Default::default() },
            ShotInput { photo: 2, phash: Some(7), ..Default::default() },
            ShotInput { photo: 3, phash: Some(7), ..Default::default() },
        ];

        let shots = group_shots(&photos, &settings, &HashSet::from([2]));

        assert_eq!(members(&shots), vec![vec![1, 3], vec![2]]);
    }
}
