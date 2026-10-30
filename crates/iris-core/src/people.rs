//! Persons = face clusters with the People Overrides replayed on top.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::model::{FaceId, Override, PhotoId, Role};

/// One detected face and the cluster the last cluster run put it in.
#[derive(Debug, Clone)]
pub struct FaceMembership {
    pub face: FaceId,
    pub photo: PhotoId,
    pub cluster: Option<i64>,
    /// Attached to its cluster by nearest centroid, not by HDBSCAN: to confirm.
    pub soft: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Person {
    /// Smallest face id of the Person: stable across re-clustering as long as that face
    /// stays with them. Overrides made from the GUI use it as their anchor.
    pub anchor: FaceId,
    pub faces: Vec<FaceId>,
    pub soft_faces: Vec<FaceId>,
    pub name: Option<String>,
    pub role: Option<Role>,
    pub groups: BTreeSet<String>,
    pub quota: u32,
}

#[derive(Debug, Clone, Default)]
pub struct People {
    pub persons: Vec<Person>,
    person_of_face: HashMap<FaceId, usize>,
}

impl People {
    pub fn person_of_face(&self, face: FaceId) -> Option<&Person> {
        self.person_of_face.get(&face).map(|&index| &self.persons[index])
    }

    pub fn by_anchor(&self, anchor: FaceId) -> Option<&Person> {
        self.persons.iter().find(|person| person.anchor == anchor)
    }

    pub fn with_role(&self, role: Role) -> Option<&Person> {
        self.persons.iter().find(|person| person.role == Some(role))
    }
}

/// Structural Overrides (merge, split, assign, remove) are replayed first, in order; then
/// attributes (name, role, Groups, Quota) land on whichever Person holds their face.
pub fn resolve(faces: &[FaceMembership], ops: &[Override]) -> People {
    let known_faces: HashSet<FaceId> = faces.iter().map(|face| face.face).collect();
    let mut label: HashMap<FaceId, i64> = faces
        .iter()
        .filter_map(|face| face.cluster.map(|cluster| (face.face, cluster)))
        .collect();
    let mut soft: BTreeSet<FaceId> = faces.iter().filter(|face| face.soft).map(|face| face.face).collect();
    let mut next_label = -1_i64;
    let mut fresh_label = || {
        next_label -= 1;
        next_label
    };

    for op in ops {
        match op {
            Override::MergePersons { face, into_face } => {
                if let (Some(&from), Some(&to)) = (label.get(face), label.get(into_face)) {
                    for value in label.values_mut().filter(|value| **value == from) {
                        *value = to;
                    }
                }
            }
            Override::SplitFaces { faces } => {
                let new_label = fresh_label();
                for face in faces {
                    if known_faces.contains(face) {
                        label.insert(*face, new_label);
                        soft.remove(face);
                    }
                }
            }
            Override::AssignFace { face, to_face } => {
                if known_faces.contains(face) && known_faces.contains(to_face) {
                    let target = match label.get(to_face).copied() {
                        Some(target) => target,
                        None => {
                            let target = fresh_label();
                            label.insert(*to_face, target);
                            target
                        }
                    };
                    label.insert(*face, target);
                    soft.remove(face);
                }
            }
            Override::RemoveFace { face } => {
                label.remove(face);
                soft.remove(face);
            }
            _ => {}
        }
    }

    let mut members: BTreeMap<i64, Vec<FaceId>> = BTreeMap::new();
    for (&face, &value) in &label {
        members.entry(value).or_default().push(face);
    }
    let mut persons: Vec<Person> = members
        .into_values()
        .map(|mut faces| {
            faces.sort_unstable();
            Person {
                anchor: faces[0],
                soft_faces: faces.iter().copied().filter(|face| soft.contains(face)).collect(),
                faces,
                ..Person::default()
            }
        })
        .collect();
    persons.sort_by_key(|person| person.anchor);
    let person_of_face: HashMap<FaceId, usize> = persons
        .iter()
        .enumerate()
        .flat_map(|(index, person)| person.faces.iter().map(move |&face| (face, index)))
        .collect();

    for op in ops {
        let (face, apply): (FaceId, Box<dyn Fn(&mut Person)>) = match op {
            Override::NamePerson { face, name } => {
                let name = name.trim().to_string();
                (*face, Box::new(move |p| p.name = (!name.is_empty()).then(|| name.clone())))
            }
            Override::SetRole { face, role } => {
                let role = *role;
                (*face, Box::new(move |p| p.role = role))
            }
            Override::SetGroup { face, group, member } => {
                let (group, member) = (group.clone(), *member);
                (*face, Box::new(move |p| {
                    if member {
                        p.groups.insert(group.clone());
                    } else {
                        p.groups.remove(&group);
                    }
                }))
            }
            Override::SetQuota { face, quota } => {
                let quota = *quota;
                (*face, Box::new(move |p| p.quota = quota))
            }
            _ => continue,
        };
        if let Some(&index) = person_of_face.get(&face) {
            apply(&mut persons[index]);
        }
    }

    // A role belongs to one Person only: the latest assignment wins.
    for role in [Role::Bride, Role::Groom] {
        let latest = ops.iter().rev().find_map(|op| match op {
            Override::SetRole { face, role: Some(r) } if *r == role => person_of_face.get(face).copied(),
            _ => None,
        });
        for (index, person) in persons.iter_mut().enumerate() {
            if person.role == Some(role) && Some(index) != latest {
                person.role = None;
            }
        }
    }

    People { persons, person_of_face }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faces(clusters: &[(FaceId, i64)]) -> Vec<FaceMembership> {
        clusters
            .iter()
            .map(|&(face, cluster)| FaceMembership { face, photo: face * 10, cluster: Some(cluster), soft: false })
            .collect()
    }

    fn named<'a>(people: &'a People, name: &str) -> &'a Person {
        people
            .persons
            .iter()
            .find(|person| person.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no person named {name}"))
    }

    #[test]
    fn decisions_follow_faces_across_reclustering() {
        let ops = vec![
            Override::NamePerson { face: 1, name: "Samia".into() },
            Override::MergePersons { face: 4, into_face: 2 },
            Override::SetQuota { face: 5, quota: 3 },
            Override::SetGroup { face: 1, group: "bride_family".into(), member: true },
        ];

        // Two cluster runs that number and cut the clusters differently.
        for run in [
            faces(&[(1, 10), (2, 10), (3, 10), (4, 11), (5, 11), (6, 12)]),
            faces(&[(1, 7), (2, 7), (3, 8), (4, 8), (5, 8), (6, 9)]),
        ] {
            let people = resolve(&run, &ops);
            let samia = named(&people, "Samia");
            assert_eq!(samia.faces, vec![1, 2, 3, 4, 5]);
            assert_eq!(samia.quota, 3);
            assert!(samia.groups.contains("bride_family"));
            assert_eq!(people.persons.len(), 2);
        }
    }

    #[test]
    fn split_faces_become_their_own_person() {
        let run = faces(&[(1, 10), (2, 10), (3, 10)]);
        let ops = vec![
            Override::NamePerson { face: 1, name: "Emna".into() },
            Override::SplitFaces { faces: vec![3] },
            Override::NamePerson { face: 3, name: "Emna's sister".into() },
            Override::RemoveFace { face: 2 },
        ];

        let people = resolve(&run, &ops);
        assert_eq!(named(&people, "Emna").faces, vec![1]);
        assert_eq!(named(&people, "Emna's sister").faces, vec![3]);
        assert!(people.person_of_face(2).is_none());
    }

    #[test]
    fn overrides_for_missing_faces_do_not_create_phantom_people() {
        let run = faces(&[(1, 10), (2, 11)]);
        let ops = vec![
            Override::SplitFaces { faces: vec![99] },
            Override::AssignFace { face: 100, to_face: 1 },
            Override::AssignFace { face: 1, to_face: 101 },
        ];

        let people = resolve(&run, &ops);

        assert_eq!(people.persons.len(), 2);
        assert!(people.by_anchor(99).is_none());
        assert!(people.by_anchor(100).is_none());
        assert!(people.by_anchor(101).is_none());
        assert!(people.person_of_face(1).is_some());
    }
}
