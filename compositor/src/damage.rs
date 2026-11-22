//! Damage: what changed on an output from one frame to the next, so that a
//! renderer draws only that.
//!
//! A frame's damage comes from its scene beside the last one drawn. A
//! surface that moved, appeared, or went damages where it was and where it
//! is; so does a frame that changed colour. A surface that only committed
//! damages what its commits damaged, as its client said, or all of it where
//! the commits do not say (see `surface.rs`). Elements that changed places
//! in the order damage both places. An output that changed size or
//! background is damaged whole.
//!
//! A backend draws into a buffer that holds an older frame, as the frame
//! before last for two buffers that take turns. What it must draw is the
//! damage of every frame since: the tracker keeps the last few frames'
//! damage, and a buffer older than those is drawn whole.

use std::collections::VecDeque;

use crate::{
    State,
    geometry::Rect,
    scene::{Element, Scene},
};

/// How many frames' damage is kept.
const HISTORY: usize = 4;
/// Damage of more rectangles than this is drawn as one, around them all.
const MAX_RECTS: usize = 16;

/// An element as drawn: for a surface, with the count of its commits then.
type Drawn = (Element, u64);

/// What one output's frames changed.
#[derive(Default)]
pub(crate) struct DamageTracker {
    /// The last scene drawn: its size, background, and elements.
    last: Option<((i32, i32), u32, Vec<Drawn>)>,
    /// Each frame's damage, the newest first.
    history: VecDeque<Vec<Rect>>,
}

impl DamageTracker {
    /// Whether a scene differs from the last one drawn, so that it must be
    /// drawn at all. Nothing is recorded.
    pub(crate) fn changes(&self, state: &State, scene: &Scene) -> bool {
        match &self.last {
            Some((size, background, last)) if *size == scene.size && *background == scene.background => {
                let drawn: Vec<Drawn> = scene
                    .elements
                    .iter()
                    .map(|element| (*element, commits_of(state, element)))
                    .collect();
                !changed(last, &drawn, &since(state)).is_empty()
            }
            _ => true,
        }
    }

    /// What to draw of `scene` into a buffer that holds the frame drawn
    /// `age` frames ago: 1 for the last frame, and 0 for a buffer that holds
    /// none of them. The scene is recorded as drawn.
    pub(crate) fn damage(&mut self, state: &State, scene: &Scene, age: usize) -> Vec<Rect> {
        let drawn: Vec<Drawn> = scene
            .elements
            .iter()
            .map(|element| (*element, commits_of(state, element)))
            .collect();
        let whole = Rect::new(0, 0, scene.size.0, scene.size.1);
        let frame = match &self.last {
            Some((size, background, last)) if *size == scene.size && *background == scene.background => {
                changed(last, &drawn, &since(state))
            }
            _ => vec![whole],
        };
        self.history.push_front(frame);
        self.history.truncate(HISTORY);
        self.last = Some((scene.size, scene.background, drawn));
        if age == 0 || age > self.history.len() {
            return vec![whole];
        }
        simplify(self.history.iter().take(age).flatten().copied().collect(), whole)
    }
}

/// How many commits a surface has had applied: any of them may change what
/// it shows.
fn commits_of(state: &State, element: &Element) -> u64 {
    match element {
        Element::Surface { surface, .. } => state.surfaces.get(surface).map_or(0, |surface| surface.commits),
        Element::Solid { .. } | Element::Cursor { .. } => 0,
    }
}

fn rect_of(element: &Element) -> Rect {
    match element {
        Element::Surface { rect, .. } | Element::Solid { rect, .. } | Element::Cursor { rect, .. } => *rect,
    }
}

/// What a surface's commits damaged, in its own coordinates, with its size
/// in them: None for all of it.
type Damaged = Option<(Vec<Rect>, (i32, i32))>;

/// A surface's damage since a count of its commits.
type Since<'a> = &'a dyn Fn(crate::surface::SurfaceId, u64) -> Damaged;

/// A surface's damage since a count of its commits, from the state.
fn since(state: &State) -> impl Fn(crate::surface::SurfaceId, u64) -> Damaged + '_ {
    |surface, commits| {
        let surface = state.surfaces.get(&surface)?;
        Some((surface.damage_since(commits)?, surface.current.size()))
    }
}

/// A rectangle of a surface, `size` in its own units, where the surface is
/// drawn into `rect`: scaled to it, rounded out, and cut to it.
fn onto(damaged: &Rect, size: (i32, i32), rect: &Rect) -> Rect {
    if size.0 <= 0 || size.1 <= 0 {
        return *rect;
    }
    // Cut to the surface first. A client may damage up to `i32::MAX` each
    // way, as many do to say "all of it", and that is more than the sums
    // below can hold.
    let damaged = damaged.intersection(&Rect::new(0, 0, size.0, size.1));
    if damaged.is_empty() {
        return Rect::default();
    }
    let (sx, sy) = (i64::from(rect.width), i64::from(rect.height));
    let (w, h) = (i64::from(size.0), i64::from(size.1));
    let down = |value: i64, scale: i64, of: i64| (value * scale).div_euclid(of);
    let up = |value: i64, scale: i64, of: i64| (value * scale + of - 1).div_euclid(of);
    let left = rect.x + down(i64::from(damaged.x), sx, w) as i32;
    let top = rect.y + down(i64::from(damaged.y), sy, h) as i32;
    let right = rect.x + up(i64::from(damaged.x) + i64::from(damaged.width), sx, w) as i32;
    let bottom = rect.y + up(i64::from(damaged.y) + i64::from(damaged.height), sy, h) as i32;
    Rect::new(left, top, right - left, bottom - top).intersection(rect)
}

/// Where two frames' elements differ.
fn changed(old: &[Drawn], new: &[Drawn], since: Since<'_>) -> Vec<Rect> {
    let mut damage = Vec::new();
    // What is not exactly as it was: where it is, and where it was. A
    // surface where it was, that only committed, damages what it said.
    for drawn in new.iter().filter(|drawn| !old.contains(drawn)) {
        let before = old.iter().find(|(element, _)| *element == drawn.0);
        if let (Element::Surface { surface, rect }, Some((_, commits))) = (&drawn.0, before)
            && let Some((rects, size)) = since(*surface, *commits)
        {
            damage.extend(rects.iter().map(|damaged| onto(damaged, size, rect)));
            continue;
        }
        damage.push(rect_of(&drawn.0));
    }
    for drawn in old.iter().filter(|drawn| !new.contains(drawn)) {
        // One that is where it was was dealt with above.
        if !new.iter().any(|(element, _)| *element == drawn.0) {
            damage.push(rect_of(&drawn.0));
        }
    }
    // What is on both but in another place in the order, committed or not.
    let elements = |drawn: &[Drawn]| drawn.iter().map(|(element, _)| *element).collect::<Vec<_>>();
    let (old, new) = (elements(old), elements(new));
    let kept_old = old.iter().filter(|element| new.contains(element));
    let kept_new = new.iter().filter(|element| old.contains(element));
    for (before, after) in kept_old.zip(kept_new) {
        if before != after {
            damage.push(rect_of(before));
            damage.push(rect_of(after));
        }
    }
    damage
}

/// Rectangles inside the output, with none overlapping another: those that
/// overlap become the one around both. Too many become one.
fn simplify(rects: Vec<Rect>, output: Rect) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for rect in rects.iter().map(|rect| rect.intersection(&output)) {
        if rect.is_empty() {
            continue;
        }
        let mut merged = rect;
        // Merging may make it overlap one it did not before.
        while let Some(index) = out.iter().position(|other| other.overlaps(&merged)) {
            merged = merged.union(&out.swap_remove(index));
        }
        out.push(merged);
    }
    if out.len() > MAX_RECTS {
        let around = out.iter().fold(Rect::default(), |around, rect| around.union(rect));
        return vec![around];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(id: u64, rect: Rect, commits: u64) -> Drawn {
        (Element::Surface { surface: id, rect }, commits)
    }

    fn solid(rect: Rect, colour: u32) -> Drawn {
        (Element::Solid { rect, colour }, 0)
    }

    const OUTPUT: Rect = Rect::new(0, 0, 1000, 1000);

    /// Commits that do not say what they damaged.
    fn whole_surfaces(_: u64, _: u64) -> Damaged {
        None
    }

    #[test]
    fn damage_far_past_the_surfaces_edge_is_cut_to_the_surface() {
        // A window away from the output's corner, at its own size and
        // stretched: the client's rectangle reaches `i32::MAX`.
        for (size, a) in [
            ((200, 100), Rect::new(100, 100, 200, 100)),
            ((200, 100), Rect::new(100, 100, 300, 150)),
        ] {
            for damaged in [
                Rect::new(0, 0, i32::MAX, i32::MAX),
                Rect::new(-5, -5, i32::MAX, i32::MAX),
                Rect::new(i32::MIN + 300, i32::MIN + 300, i32::MAX, i32::MAX),
            ] {
                let since = move |_: u64, _: u64| Some((vec![damaged], size));
                let damage = changed(&[surface(1, a, 4)], &[surface(1, a, 5)], &since);
                assert_eq!(simplify(damage, OUTPUT), [a], "{damaged:?} at {a:?}: all of the window");
            }
            // One wholly past the far corner damages nothing.
            let since = move |_: u64, _: u64| Some((vec![Rect::new(i32::MAX, i32::MAX, i32::MAX, i32::MAX)], size));
            let damage = changed(&[surface(1, a, 4)], &[surface(1, a, 5)], &since);
            assert_eq!(simplify(damage, OUTPUT), [], "past the corner");
        }
    }

    #[test]
    fn a_commit_that_says_what_it_damaged_damages_that_alone() {
        let a = Rect::new(100, 100, 200, 100);
        let since = |surface: u64, commits: u64| {
            assert_eq!((surface, commits), (1, 4));
            Some((vec![Rect::new(10, 20, 5, 5), Rect::new(190, 90, 50, 50)], (200, 100)))
        };
        let damage = changed(&[surface(1, a, 4)], &[surface(1, a, 6)], &since);
        assert_eq!(
            simplify(damage, OUTPUT),
            [Rect::new(110, 120, 5, 5), Rect::new(290, 190, 10, 10)],
            "inside the window, and cut to it"
        );
    }

    #[test]
    fn damage_on_a_scaled_output_covers_the_pixels_it_falls_on() {
        // A 200x100 surface drawn into 300x150 pixels, at a scale of 1.5.
        let rect = Rect::new(30, 30, 300, 150);
        let since = |_: u64, _: u64| Some((vec![Rect::new(1, 1, 1, 1)], (200, 100)));
        let damage = changed(&[surface(1, rect, 1)], &[surface(1, rect, 2)], &since);
        assert_eq!(simplify(damage, OUTPUT), [Rect::new(31, 31, 2, 2)]);
    }

    #[test]
    fn a_window_that_commits_as_it_is_raised_damages_where_it_overlaps() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        let nothing = |_: u64, _: u64| Some((Vec::new(), (100, 100)));
        let old = [surface(1, a, 1), surface(2, b, 1)];
        let new = [surface(2, b, 1), surface(1, a, 2)];
        assert_eq!(simplify(changed(&old, &new, &nothing), OUTPUT), [a.union(&b)]);
    }

    #[test]
    fn nothing_changed_is_no_damage() {
        let frame = [
            surface(1, Rect::new(0, 0, 10, 10), 3),
            solid(Rect::new(20, 20, 5, 5), 7),
        ];
        assert!(changed(&frame, &frame, &whole_surfaces).is_empty());
    }

    #[test]
    fn a_commit_damages_where_the_surface_is() {
        let a = Rect::new(10, 10, 100, 50);
        let b = Rect::new(300, 300, 100, 50);
        let old = [surface(1, a, 1), surface(2, b, 1)];
        let new = [surface(1, a, 1), surface(2, b, 2)];
        assert_eq!(simplify(changed(&old, &new, &whole_surfaces), OUTPUT), [b]);
    }

    #[test]
    fn a_move_damages_where_it_was_and_where_it_is() {
        let from = Rect::new(10, 10, 100, 50);
        let to = Rect::new(500, 10, 100, 50);
        let damage = simplify(
            changed(&[surface(1, from, 1)], &[surface(1, to, 1)], &whole_surfaces),
            OUTPUT,
        );
        assert_eq!(damage.len(), 2);
        assert!(damage.contains(&from) && damage.contains(&to));
    }

    #[test]
    fn a_frame_that_changes_colour_is_damaged() {
        let rect = Rect::new(8, 8, 2, 100);
        let damage = changed(&[solid(rect, 1)], &[solid(rect, 2)], &whole_surfaces);
        assert_eq!(simplify(damage, OUTPUT), [rect]);
    }

    #[test]
    fn a_raised_window_damages_what_changed_places() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        let old = [surface(1, a, 1), surface(2, b, 1)];
        let new = [surface(2, b, 1), surface(1, a, 1)];
        assert_eq!(simplify(changed(&old, &new, &whole_surfaces), OUTPUT), [a.union(&b)]);
    }

    #[test]
    fn damage_stays_inside_the_output_and_overlaps_merge() {
        let damage = simplify(
            vec![
                Rect::new(-10, -10, 20, 20),
                Rect::new(5, 5, 10, 10),
                Rect::new(990, 990, 50, 50),
            ],
            OUTPUT,
        );
        assert_eq!(damage, [Rect::new(0, 0, 15, 15), Rect::new(990, 990, 10, 10)]);
    }

    #[test]
    fn many_rectangles_become_one() {
        let rects = (0..40).map(|index| Rect::new(index * 20, 0, 10, 10)).collect();
        assert_eq!(simplify(rects, OUTPUT), [Rect::new(0, 0, 790, 10)]);
    }
}
