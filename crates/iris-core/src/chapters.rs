//! Chapters: which moment of the day each Photo belongs to.

/// Splits a time-ordered sequence into contiguous blocks, one per Chapter in order
/// (a block may be empty), maximising the summed fit. `scores[i][c]` is how well Photo `i`
/// fits Chapter `c`. Returns the Chapter index of each Photo.
pub fn split_contiguous(scores: &[Vec<f64>]) -> Vec<usize> {
    let Some(chapters) = scores.first().map(Vec::len).filter(|&k| k > 0) else {
        return vec![0; scores.len()];
    };
    // best[i][c]: best total for Photos 0..=i with Photo i in Chapter c.
    let mut best = vec![vec![f64::NEG_INFINITY; chapters]; scores.len()];
    let mut from = vec![vec![0_usize; chapters]; scores.len()];
    for (i, row) in scores.iter().enumerate() {
        for c in 0..chapters {
            let (previous, prev_c) = if i == 0 {
                (0.0, c)
            } else {
                (0..=c)
                    .map(|p| (best[i - 1][p], p))
                    .max_by(|a, b| a.0.total_cmp(&b.0))
                    .expect("at least one previous Chapter")
            };
            best[i][c] = previous + row[c];
            from[i][c] = prev_c;
        }
    }
    let mut out = vec![0; scores.len()];
    let Some(last) = scores.len().checked_sub(1) else { return out };
    let mut c = (0..chapters).max_by(|&a, &b| best[last][a].total_cmp(&best[last][b])).expect("chapters > 0");
    for i in (0..=last).rev() {
        out[i] = c;
        c = from[i][c];
    }
    out
}

/// What we know about a Guest Photo when choosing its Chapter.
#[derive(Debug, Clone, Default)]
pub struct GuestEvidence {
    /// Nearest reference Photos (Chapter index, similarity in 0..1), most similar first.
    pub neighbours: Vec<(usize, f64)>,
    /// Zero-shot probability per Chapter, from the Chapters' descriptions.
    pub zero_shot: Vec<f64>,
    /// The send time proves the Photo was taken before Chapters after this one started.
    pub latest_possible: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChapterGuess {
    pub chapter: usize,
    pub probability: f64,
    /// Probability gap to the second-best Chapter: low means "check me".
    pub margin: f64,
}

/// Neighbour votes (weighted by similarity) dominate; the Chapters' descriptions break
/// ties and decide alone when no reference exists (the groom's preparation).
pub fn guess_guest_chapter(evidence: &GuestEvidence, chapters: usize) -> Option<ChapterGuess> {
    const VOTE_WEIGHT: f64 = 0.8;
    let mut votes = vec![0.0; chapters];
    for &(chapter, similarity) in &evidence.neighbours {
        if chapter < chapters {
            votes[chapter] += similarity.max(0.0);
        }
    }
    let vote_total: f64 = votes.iter().sum();
    let zero_shot_total: f64 = evidence.zero_shot.iter().take(chapters).sum();
    let mut probs: Vec<f64> = (0..chapters)
        .map(|c| {
            let zero_shot = evidence.zero_shot.get(c).copied().unwrap_or(0.0) / zero_shot_total.max(1e-9);
            if vote_total > 0.0 {
                VOTE_WEIGHT * votes[c] / vote_total + (1.0 - VOTE_WEIGHT) * zero_shot
            } else {
                zero_shot
            }
        })
        .collect();
    if let Some(latest) = evidence.latest_possible {
        probs.iter_mut().skip(latest + 1).for_each(|p| *p = 0.0);
    }
    let total: f64 = probs.iter().sum();
    if total <= 0.0 {
        return None;
    }
    probs.iter_mut().for_each(|p| *p /= total);
    let mut order: Vec<usize> = (0..chapters).collect();
    order.sort_by(|&a, &b| probs[b].total_cmp(&probs[a]));
    let second = order.get(1).map_or(0.0, |&c| probs[c]);
    Some(ChapterGuess { chapter: order[0], probability: probs[order[0]], margin: probs[order[0]] - second })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contiguous_split_follows_the_majority_and_ignores_outliers() {
        // 3 Chapters, 9 Photos in time order; Photo 4 looks like Chapter 2 but sits
        // in the middle of Chapter 1.
        let fits = [0, 0, 0, 1, 2, 1, 1, 2, 2];
        let scores: Vec<Vec<f64>> = fits
            .iter()
            .map(|&best| (0..3).map(|c| if c == best { 0.8 } else { 0.1 }).collect())
            .collect();

        assert_eq!(split_contiguous(&scores), vec![0, 0, 0, 1, 1, 1, 1, 2, 2]);
    }

    #[test]
    fn guest_chapter_comes_from_neighbours_within_the_time_bound() {
        let chapters = 4;
        let looks_like_party = GuestEvidence {
            neighbours: vec![(3, 0.9), (3, 0.85), (2, 0.6)],
            zero_shot: vec![0.25; 4],
            latest_possible: None,
        };
        let guess = guess_guest_chapter(&looks_like_party, chapters).unwrap();
        assert_eq!(guess.chapter, 3);
        assert!(guess.margin > 0.15);

        // Sent before Chapter 3 started: it cannot be from Chapter 3.
        let sent_early = GuestEvidence { latest_possible: Some(2), ..looks_like_party };
        assert_eq!(guess_guest_chapter(&sent_early, chapters).unwrap().chapter, 2);
    }
}
