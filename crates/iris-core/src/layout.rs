//! Layout: where each Shot goes on a Spread, and how it is cropped. All in millimetres.
//!
//! A page Template is an arrangement of its Shots in justified rows or columns
//! (`rows:2-3` = a row of 2 then a row of 3), or `hero` (one Shot, full bleed). Rows and
//! columns keep each Photo's own shape, so nothing is cropped; heroes are cropped around
//! the faces.

use crate::model::{Crop, PAGE_H, PAGE_W, PhotoId, PlacedPhoto, Rect};

pub const MARGIN: f64 = 15.0;
pub const GAP: f64 = 4.0;
/// Height kept free at the top of a page that carries a Chapter title.
pub const TITLE_BAND: f64 = 46.0;

/// What layout needs to know about a Shot.
#[derive(Debug, Clone, Default)]
pub struct LayoutShot {
    pub photo: PhotoId,
    /// Width / height of the upright Photo.
    pub aspect: f64,
    pub quality: f64,
    /// Longest side printable at 250 ppi, in mm.
    pub max_print_mm: f64,
    pub hero: bool,
    /// Important faces, as fractions of the Photo (x1, y1, x2, y2).
    pub faces: Vec<[f64; 4]>,
}

/// The content box of a page: whole page minus margins (and the title band).
pub fn content_box(right_page: bool, title: bool) -> Rect {
    let x = if right_page { PAGE_W } else { 0.0 } + MARGIN;
    let top = MARGIN + if title { TITLE_BAND } else { 0.0 };
    Rect { x, y: top, w: PAGE_W - 2.0 * MARGIN, h: PAGE_H - MARGIN - top }
}

/// Crop of a Photo with `photo_aspect` to fill a Slot of `slot_aspect`, keeping the faces
/// whole with headroom, the faces near a thirds line, and as much of the Photo as possible.
/// Returns the crop and whether a face had to be cut.
pub fn face_crop(photo_aspect: f64, slot_aspect: f64, faces: &[[f64; 4]]) -> (Crop, bool) {
    const HEADROOM: f64 = 0.3;
    let union = faces.iter().copied().reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
    let face_height = faces.iter().map(|f| f[3] - f[1]).fold(0.0, f64::max);

    // Place a window of `size` (fraction) along one axis.
    let place = |size: f64, lo: Option<f64>, hi: Option<f64>, center: Option<f64>, default: f64| -> (f64, bool) {
        let max_start = (1.0 - size).max(0.0);
        let (Some(lo), Some(hi), Some(center)) = (lo, hi, center) else { return (default * max_start, false) };
        // The window must start in [hi - size, lo] to contain [lo, hi].
        let (from, to) = ((hi - size).max(0.0), lo.min(max_start));
        let cut = from > to + 1e-9;
        let candidates = [1.0 / 3.0, 0.5, 2.0 / 3.0].map(|third| center - third * size);
        let best = candidates
            .iter()
            .map(|&start| {
                let clamped = if cut { start.clamp(0.0, max_start) } else { start.clamp(from, to) };
                (clamped, (clamped - start).abs())
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(0.0, |(start, _)| start);
        (best, cut)
    };

    if slot_aspect < photo_aspect {
        let w = slot_aspect / photo_aspect;
        let (x, cut) = place(w, union.map(|u| u[0]), union.map(|u| u[2]), union.map(|u| (u[0] + u[2]) / 2.0), 0.5);
        (Crop { x, y: 0.0, w, h: 1.0 }, cut)
    } else {
        let h = (photo_aspect / slot_aspect).min(1.0);
        let top = union.map(|u| (u[1] - HEADROOM * face_height).max(0.0));
        let (y, cut) = place(h, top, union.map(|u| u[3]), union.map(|u| (u[1] + u[3]) / 2.0), 0.3);
        (Crop { x: 0.0, y, w: 1.0, h }, cut)
    }
}

fn parse_template(template: &str) -> Option<(&str, Vec<usize>)> {
    if template == "hero" {
        return Some(("hero", vec![1]));
    }
    let (kind, sizes) = template.split_once(':')?;
    let sizes: Vec<usize> = sizes.split('-').map(|n| n.parse().ok()).collect::<Option<_>>()?;
    matches!(kind, "rows" | "cols").then_some((kind, sizes))
}

/// Lays out one page. `template` is `hero`, `rows:a-b-…` or `cols:a-b-…`.
pub fn layout_page(shots: &[LayoutShot], template: &str, right_page: bool, title: bool) -> Option<Vec<PlacedPhoto>> {
    let (kind, sizes) = parse_template(template)?;
    if sizes.iter().sum::<usize>() != shots.len() || sizes.contains(&0) {
        return None;
    }
    if kind == "hero" {
        let shot = &shots[0];
        let x = if right_page { PAGE_W } else { 0.0 };
        let rect = Rect { x, y: 0.0, w: PAGE_W, h: PAGE_H };
        let (crop, _) = face_crop(shot.aspect, rect.aspect(), &shot.faces);
        return Some(vec![PlacedPhoto { photo: shot.photo, rect, crop }]);
    }
    let area = content_box(right_page, title);
    let rows = kind == "rows";
    // Work in "rows" space; columns are rows with the axes swapped.
    let (span, depth) = if rows { (area.w, area.h) } else { (area.h, area.w) };
    let aspect = |shot: &LayoutShot| if rows { shot.aspect } else { 1.0 / shot.aspect };

    let mut groups = Vec::new();
    let mut start = 0;
    for &size in &sizes {
        groups.push(&shots[start..start + size]);
        start += size;
    }
    // Each line fills the span at its natural depth; then everything scales to fit.
    let natural: Vec<f64> = groups
        .iter()
        .map(|g| (span - GAP * (g.len() as f64 - 1.0)) / g.iter().map(aspect).sum::<f64>())
        .collect();
    let gaps = GAP * (groups.len() as f64 - 1.0);
    let scale = ((depth - gaps) / natural.iter().sum::<f64>()).min(1.0);
    let used_depth = natural.iter().sum::<f64>() * scale + gaps;
    let mut along_depth = (depth - used_depth) / 2.0;

    let mut placed = Vec::new();
    for (group, line_depth) in groups.iter().zip(&natural) {
        let line_depth = line_depth * scale;
        let line_span: f64 = group.iter().map(|s| aspect(s) * line_depth).sum::<f64>() + GAP * (group.len() as f64 - 1.0);
        let mut along_span = (span - line_span) / 2.0;
        for shot in *group {
            let length = aspect(shot) * line_depth;
            let rect = if rows {
                Rect { x: area.x + along_span, y: area.y + along_depth, w: length, h: line_depth }
            } else {
                Rect { x: area.x + along_depth, y: area.y + along_span, w: line_depth, h: length }
            };
            placed.push(PlacedPhoto { photo: shot.photo, rect: fit_resolution(rect, shot.max_print_mm), crop: Crop::FULL });
            along_span += length + GAP;
        }
        along_depth += line_depth + GAP;
    }
    Some(placed)
}

/// Never print a Photo beyond 250 ppi: a Slot bigger than that shrinks around its center,
/// leaving white space, so low-resolution Shots end up small.
fn fit_resolution(rect: Rect, max_print_mm: f64) -> Rect {
    let longest = rect.w.max(rect.h);
    if max_print_mm <= 0.0 || longest <= max_print_mm {
        return rect;
    }
    let scale = max_print_mm / longest;
    let (w, h) = (rect.w * scale, rect.h * scale);
    Rect { x: rect.x + (rect.w - w) / 2.0, y: rect.y + (rect.h - h) / 2.0, w, h }
}

/// How good a laid-out page is: lower is better.
fn page_cost(shots: &[LayoutShot], placed: &[PlacedPhoto], template: &str, right_page: bool, title: bool) -> f64 {
    if template == "hero" {
        // Heroes are chosen, not discovered: only landscape Photos fill a page well.
        let shot = &shots[0];
        let loss = 1.0 - (shot.aspect.min(PAGE_W / PAGE_H) / shot.aspect.max(PAGE_W / PAGE_H));
        // A Hero fills the page: only Photos sharp enough for that (crop included).
        let printable = shot.max_print_mm * shot.aspect.min(PAGE_W / PAGE_H) / shot.aspect.max(PAGE_W / PAGE_H);
        return if shot.hero && printable >= PAGE_W { loss } else { 10.0 };
    }
    let area = content_box(right_page, title);
    let areas: Vec<f64> = placed.iter().map(|p| p.rect.w * p.rect.h).collect();
    let fill = areas.iter().sum::<f64>() / (area.w * area.h);
    let max_area = areas.iter().copied().fold(0.0, f64::max);
    let min_area = areas.iter().copied().fold(f64::MAX, f64::min);
    let imbalance = (0.2 - min_area / max_area).max(0.0);
    let resolution: f64 = shots
        .iter()
        .zip(placed)
        .map(|(shot, p)| (p.rect.w.max(p.rect.h) / shot.max_print_mm.max(1.0) - 1.25).max(0.0))
        .sum();
    // The best Shot should not be the smallest.
    let best = shots.iter().enumerate().max_by(|a, b| a.1.quality.total_cmp(&b.1.quality)).map_or(0, |(i, _)| i);
    let importance = if shots.len() > 2 && (areas[best] - min_area).abs() < 1e-6 && max_area > min_area * 1.3 { 0.05 } else { 0.0 };
    (1.0 - fill) + imbalance * 1.5 + resolution * 2.0 + importance
}

/// Ways to cut `n` Shots into lines (compositions of n).
fn compositions(n: usize, max_lines: usize, max_per_line: usize) -> Vec<Vec<usize>> {
    fn go(left: usize, current: &mut Vec<usize>, out: &mut Vec<Vec<usize>>, max_lines: usize, max_per_line: usize) {
        if left == 0 {
            out.push(current.clone());
            return;
        }
        if current.len() == max_lines {
            return;
        }
        for take in 1..=left.min(max_per_line) {
            current.push(take);
            go(left - take, current, out, max_lines, max_per_line);
            current.pop();
        }
    }
    let mut out = Vec::new();
    go(n, &mut Vec::new(), &mut out, max_lines, max_per_line);
    out
}

/// Every Template that fits these Shots, best first, with its cost.
pub fn page_options(shots: &[LayoutShot], right_page: bool, title: bool) -> Vec<(String, f64)> {
    let mut options = Vec::new();
    let mut try_template = |template: String| {
        if let Some(placed) = layout_page(shots, &template, right_page, title) {
            let cost = page_cost(shots, &placed, &template, right_page, title);
            options.push((template, cost));
        }
    };
    if shots.len() == 1 && !title {
        try_template("hero".into());
    }
    for sizes in compositions(shots.len(), 3, 4) {
        try_template(format!("rows:{}", join(&sizes)));
    }
    for sizes in compositions(shots.len(), 4, 3) {
        if sizes.len() > 1 || shots.len() > 1 {
            try_template(format!("cols:{}", join(&sizes)));
        }
    }
    options.sort_by(|a, b| a.1.total_cmp(&b.1));
    options.dedup_by(|a, b| a.0 == b.0);
    options
}

fn join(sizes: &[usize]) -> String {
    sizes.iter().map(usize::to_string).collect::<Vec<_>>().join("-")
}

/// A laid-out Spread before it gets an identity.
#[derive(Debug, Clone, PartialEq)]
pub struct SpreadLayout {
    pub templates: [String; 2],
    pub photos: Vec<PlacedPhoto>,
    pub cost: f64,
}

const MAX_PER_PAGE: usize = 6;

/// Best layout of these Shots (in order) on one Spread.
pub fn layout_spread(shots: &[LayoutShot], title: bool) -> Option<SpreadLayout> {
    let mut memo = PageMemo::default();
    best_spread(shots, title, &mut memo).map(|(templates, cost)| {
        let split = templates_split(&templates, shots.len());
        let mut photos = Vec::new();
        if split > 0 {
            photos.extend(layout_page(&shots[..split], &templates[0], false, title).unwrap_or_default());
        }
        if split < shots.len() {
            photos.extend(layout_page(&shots[split..], &templates[1], true, false).unwrap_or_default());
        }
        SpreadLayout { templates, photos, cost }
    })
}

/// How many Shots a Template holds.
pub fn template_size(template: &str) -> usize {
    parse_template(template).map_or(0, |(_, sizes)| sizes.iter().sum())
}

fn templates_split(templates: &[String; 2], total: usize) -> usize {
    template_size(&templates[0]).min(total)
}

#[derive(Default)]
struct PageMemo(std::collections::HashMap<(Vec<PhotoId>, bool, bool), Option<(String, f64)>>);

impl PageMemo {
    fn best(&mut self, shots: &[LayoutShot], right_page: bool, title: bool) -> Option<(String, f64)> {
        let key = (shots.iter().map(|s| s.photo).collect(), right_page, title);
        self.0
            .entry(key)
            .or_insert_with(|| page_options(shots, right_page, title).into_iter().next())
            .clone()
    }
}

fn best_spread(shots: &[LayoutShot], title: bool, memo: &mut PageMemo) -> Option<([String; 2], f64)> {
    let n = shots.len();
    if n == 0 {
        return None;
    }
    if n == 1 {
        // One Shot: alone on the right page, the left page stays for the title or blank.
        let (template, cost) = memo.best(shots, true, false)?;
        return Some(([String::new(), template], cost + 0.6));
    }
    let heroes = shots.iter().filter(|s| s.hero).count();
    let mut best: Option<([String; 2], f64)> = None;
    for left in 1..n {
        let right = n - left;
        if left > MAX_PER_PAGE || right > MAX_PER_PAGE {
            continue;
        }
        // A Hero is alone on its page.
        if heroes > 0 {
            let hero_alone = (left == 1 && shots[0].hero) || (right == 1 && shots[n - 1].hero);
            if heroes > 1 || !hero_alone {
                continue;
            }
        }
        let (Some(l), Some(r)) = (memo.best(&shots[..left], false, title), memo.best(&shots[left..], true, false)) else { continue };
        let balance = 0.04 * (left as f64 - right as f64).abs();
        let cost = l.1 + r.1 + balance;
        if best.as_ref().is_none_or(|b| cost < b.1) {
            best = Some(([l.0, r.0], cost));
        }
    }
    best
}

/// Cuts a Chapter's Shots (in order) into `spreads` Spreads of varied size and lays each
/// out. The first Spread carries the Chapter title when `title`.
pub fn layout_chapter(shots: &[LayoutShot], spreads: usize, title: bool, target_per_spread: f64) -> Vec<SpreadLayout> {
    let n = shots.len();
    if n == 0 || spreads == 0 {
        return Vec::new();
    }
    let spreads = spreads.min(n);
    let lo = if n >= 3 * spreads { 3 } else { 1 };
    let hi = if n <= 9 * spreads { 9 } else { 2 * MAX_PER_PAGE };
    let mut memo = PageMemo::default();
    let mut segment_cost = std::collections::HashMap::new();
    // cost[k][i]: best cost of laying out the first i Shots on k Spreads.
    let mut cost = vec![vec![f64::INFINITY; n + 1]; spreads + 1];
    let mut cut = vec![vec![0_usize; n + 1]; spreads + 1];
    cost[0][0] = 0.0;
    for k in 1..=spreads {
        for i in 1..=n {
            for size in lo..=hi.min(i) {
                let start = i - size;
                if !cost[k - 1][start].is_finite() {
                    continue;
                }
                let with_title = title && k == 1;
                let spread_cost = *segment_cost
                    .entry((start, i, with_title))
                    .or_insert_with(|| best_spread(&shots[start..i], with_title, &mut memo).map_or(f64::INFINITY, |b| b.1));
                let deviation = (size as f64 - target_per_spread) / target_per_spread;
                let total = cost[k - 1][start] + spread_cost + 0.15 * deviation * deviation;
                if total < cost[k][i] {
                    cost[k][i] = total;
                    cut[k][i] = start;
                }
            }
        }
    }
    if !cost[spreads][n].is_finite() {
        return Vec::new();
    }
    let mut bounds = Vec::new();
    let mut i = n;
    for k in (1..=spreads).rev() {
        let start = cut[k][i];
        bounds.push((start, i, title && k == 1));
        i = start;
    }
    bounds.reverse();
    bounds
        .into_iter()
        .filter_map(|(start, end, with_title)| layout_spread(&shots[start..end], with_title))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_crop_keeps_faces_with_headroom() {
        // A 3:2 landscape cropped to a square; one face on the right third.
        let face = [0.70, 0.30, 0.80, 0.45];
        let (crop, cut) = face_crop(1.5, 1.0, &[face]);

        assert!(!cut);
        assert!((crop.w - 1.0 / 1.5).abs() < 1e-6 && (crop.h - 1.0).abs() < 1e-6);
        assert!(crop.x <= face[0] && crop.x + crop.w >= face[2], "face cut horizontally: {crop:?}");
        assert!(crop.y <= face[1] - 0.3 * (face[3] - face[1]) + 1e-9, "no headroom: {crop:?}");
        assert!(crop.x > 0.0, "should move toward the face");
    }

    #[test]
    fn low_resolution_shots_get_slots_they_can_fill_at_250_ppi() {
        // A WhatsApp photo printable up to 90 mm, among two photographer photos.
        let shots: Vec<LayoutShot> = [(1, 500.0), (2, 90.0), (3, 500.0)]
            .into_iter()
            .map(|(photo, max_print_mm)| LayoutShot { photo, aspect: 1.5, quality: 0.5, max_print_mm, ..LayoutShot::default() })
            .collect();

        let spread = layout_spread(&shots, false).unwrap();

        let small = spread.photos.iter().find(|p| p.photo == 2).unwrap();
        assert!(small.rect.w.max(small.rect.h) <= 90.0 + 1e-6, "blown up beyond 250 ppi: {:?}", small.rect);
    }

    #[test]
    fn a_chapter_is_cut_into_full_spreads_in_order() {
        let shots: Vec<LayoutShot> = (0..20)
            .map(|i| LayoutShot {
                photo: i,
                aspect: if i % 3 == 0 { 2.0 / 3.0 } else { 1.5 },
                quality: 0.5,
                max_print_mm: 400.0,
                ..LayoutShot::default()
            })
            .collect();

        let spreads = layout_chapter(&shots, 3, true, 6.0);

        assert_eq!(spreads.len(), 3);
        let placed: Vec<PhotoId> = spreads.iter().flat_map(|s| s.photos.iter().map(|p| p.photo)).collect();
        assert_eq!(placed, (0..20).collect::<Vec<_>>(), "every Shot once, in order");
        for spread in &spreads {
            assert!((3..=9).contains(&spread.photos.len()));
            for (i, a) in spread.photos.iter().enumerate() {
                let r = a.rect;
                let page_x = if r.x >= PAGE_W { PAGE_W } else { 0.0 };
                assert!(r.x >= page_x + MARGIN - 1e-6 && r.x + r.w <= page_x + PAGE_W - MARGIN + 1e-6, "outside the page: {r:?}");
                assert!(r.y >= MARGIN - 1e-6 && r.y + r.h <= PAGE_H - MARGIN + 1e-6, "outside the page: {r:?}");
                for b in &spread.photos[i + 1..] {
                    let o = b.rect;
                    let overlap = r.x < o.x + o.w && o.x < r.x + r.w && r.y < o.y + o.h && o.y < r.y + r.h;
                    assert!(!overlap, "{r:?} overlaps {o:?}");
                }
            }
        }
        // The title band stays free on the first Spread's left page.
        assert!(spreads[0].photos.iter().filter(|p| p.rect.x < PAGE_W).all(|p| p.rect.y >= MARGIN + TITLE_BAND - 1e-6));
    }
}
