//! Damage: what changed on an output from one frame to the next, so that a
//! renderer draws only that.
//!
//! A frame's damage comes from its scene beside the last one drawn. A
//! surface that moved, appeared, went, or committed damages where it was and
//! where it is; so does a frame that changed colour. Elements that changed
//! places in the order damage both places. An output that changed size or
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
                changed(last, &drawn)
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

/// Where two frames' elements differ.
fn changed(old: &[Drawn], new: &[Drawn]) -> Vec<Rect> {
    let mut damage = Vec::new();
    // What is not exactly as it was: where it is, and where it was.
    for drawn in new.iter().filter(|drawn| !old.contains(drawn)) {
        damage.push(rect_of(&drawn.0));
    }
    for drawn in old.iter().filter(|drawn| !new.contains(drawn)) {
        damage.push(rect_of(&drawn.0));
    }
    // What is on both but in another place in the order.
    let kept_old = old.iter().filter(|drawn| new.contains(drawn));
    let kept_new = new.iter().filter(|drawn| old.contains(drawn));
    for (before, after) in kept_old.zip(kept_new) {
        if before != after {
            damage.push(rect_of(&before.0));
            damage.push(rect_of(&after.0));
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

    #[test]
    fn nothing_changed_is_no_damage() {
        let frame = [
            surface(1, Rect::new(0, 0, 10, 10), 3),
            solid(Rect::new(20, 20, 5, 5), 7),
        ];
        assert!(changed(&frame, &frame).is_empty());
    }

    #[test]
    fn a_commit_damages_where_the_surface_is() {
        let a = Rect::new(10, 10, 100, 50);
        let b = Rect::new(300, 300, 100, 50);
        let old = [surface(1, a, 1), surface(2, b, 1)];
        let new = [surface(1, a, 1), surface(2, b, 2)];
        assert_eq!(simplify(changed(&old, &new), OUTPUT), [b]);
    }

    #[test]
    fn a_move_damages_where_it_was_and_where_it_is() {
        let from = Rect::new(10, 10, 100, 50);
        let to = Rect::new(500, 10, 100, 50);
        let damage = simplify(changed(&[surface(1, from, 1)], &[surface(1, to, 1)]), OUTPUT);
        assert_eq!(damage.len(), 2);
        assert!(damage.contains(&from) && damage.contains(&to));
    }

    #[test]
    fn a_frame_that_changes_colour_is_damaged() {
        let rect = Rect::new(8, 8, 2, 100);
        let damage = changed(&[solid(rect, 1)], &[solid(rect, 2)]);
        assert_eq!(simplify(damage, OUTPUT), [rect]);
    }

    #[test]
    fn a_raised_window_damages_what_changed_places() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 100, 100);
        let old = [surface(1, a, 1), surface(2, b, 1)];
        let new = [surface(2, b, 1), surface(1, a, 1)];
        assert_eq!(simplify(changed(&old, &new), OUTPUT), [a.union(&b)]);
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
