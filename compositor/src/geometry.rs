//! Rectangles and regions in surface coordinates.

/// A rectangle: its top-left corner and its size. A rectangle with no area
/// holds no point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Rect { x, y, width, height }
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        !self.is_empty()
            && x >= self.x
            && y >= self.y
            && i64::from(x) < i64::from(self.x) + i64::from(self.width)
            && i64::from(y) < i64::from(self.y) + i64::from(self.height)
    }

    /// The smallest rectangle holding both, or the other one if this is
    /// empty.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = (i64::from(self.x) + i64::from(self.width)).max(i64::from(other.x) + i64::from(other.width));
        let bottom = (i64::from(self.y) + i64::from(self.height)).max(i64::from(other.y) + i64::from(other.height));
        Rect::new(
            left,
            top,
            (right - i64::from(left)).min(i64::from(i32::MAX)) as i32,
            (bottom - i64::from(top)).min(i64::from(i32::MAX)) as i32,
        )
    }

    /// The part both hold, which is empty where they do not meet.
    pub fn intersection(&self, other: &Rect) -> Rect {
        let left = i64::from(self.x).max(i64::from(other.x));
        let top = i64::from(self.y).max(i64::from(other.y));
        let right = (i64::from(self.x) + i64::from(self.width)).min(i64::from(other.x) + i64::from(other.width));
        let bottom = (i64::from(self.y) + i64::from(self.height)).min(i64::from(other.y) + i64::from(other.height));
        if self.is_empty() || other.is_empty() || right <= left || bottom <= top {
            return Rect::default();
        }
        Rect::new(left as i32, top as i32, (right - left) as i32, (bottom - top) as i32)
    }

    /// Whether they share any point.
    pub fn overlaps(&self, other: &Rect) -> bool {
        !self.intersection(other).is_empty()
    }

    pub fn translated(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(
            self.x.saturating_add(dx),
            self.y.saturating_add(dy),
            self.width,
            self.height,
        )
    }
}

/// The most rectangles one region may be built from.
pub const MAX_REGION_OPERATIONS: usize = 256;

/// A region: rectangles added and subtracted, in order. A point is in the
/// region if the last operation whose rectangle holds it was an addition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region {
    operations: Vec<(bool, Rect)>,
}

impl Region {
    /// Adds or subtracts a rectangle. Returns false, and changes nothing, if
    /// the region already holds [`MAX_REGION_OPERATIONS`] of them.
    pub fn apply(&mut self, add: bool, rect: Rect) -> bool {
        if self.operations.len() >= MAX_REGION_OPERATIONS {
            return false;
        }
        if !rect.is_empty() {
            self.operations.push((add, rect));
        }
        true
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.operations
            .iter()
            .rev()
            .find(|(_, rect)| rect.contains(x, y))
            .is_some_and(|(add, _)| *add)
    }

    /// The region that holds every point of a rectangle.
    pub fn of(rect: Rect) -> Self {
        let mut region = Region::default();
        region.apply(true, rect);
        region
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_holds_its_own_points_and_no_others() {
        let rect = Rect::new(10, 20, 3, 2);
        assert!(rect.contains(10, 20) && rect.contains(12, 21));
        assert!(!rect.contains(13, 20) && !rect.contains(10, 22) && !rect.contains(9, 20));
        assert!(!Rect::new(0, 0, 0, 5).contains(0, 0));
        // No overflow at the edges of the coordinate space.
        assert!(Rect::new(i32::MAX - 1, 0, i32::MAX, 1).contains(i32::MAX, 0));
    }

    #[test]
    fn a_later_subtraction_wins_over_an_earlier_addition() {
        let mut region = Region::of(Rect::new(0, 0, 100, 100));
        region.apply(false, Rect::new(40, 40, 20, 20));
        assert!(region.contains(10, 10));
        assert!(!region.contains(50, 50));
        region.apply(true, Rect::new(45, 45, 5, 5));
        assert!(region.contains(47, 47));
    }

    #[test]
    fn a_region_has_a_bounded_number_of_operations() {
        let mut region = Region::default();
        for i in 0..MAX_REGION_OPERATIONS {
            assert!(region.apply(true, Rect::new(i as i32, 0, 1, 1)));
        }
        assert!(!region.apply(true, Rect::new(0, 0, 1, 1)));
    }

    #[test]
    fn a_union_covers_both() {
        let union = Rect::new(0, 0, 10, 10).union(&Rect::new(20, 5, 10, 10));
        assert_eq!(union, Rect::new(0, 0, 30, 15));
        assert_eq!(Rect::default().union(&Rect::new(1, 2, 3, 4)), Rect::new(1, 2, 3, 4));
    }
}
