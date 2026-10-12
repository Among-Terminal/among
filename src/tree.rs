//! The pane layout tree: plain data, no GTK types.
//!
//! Keeping the tree free of widget types is what lets layout persistence be
//! added later without restructuring, and lets the tree be tested headlessly.
//! Widgets are produced by [`crate::pane`] from this data, not stored in it.

/// Which way a split divides its two children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// Children side by side, separated by a vertical divider.
    ///
    /// Named for the divider, matching GTK's `PanedOrientation::Vertical`.
    Vertical,
    /// Children stacked, separated by a horizontal divider.
    ///
    /// Named for the divider, matching GTK's `PanedOrientation::Horizontal`.
    Horizontal,
}

/// Identifies a pane. Stable for the pane's lifetime, so plugin content and
/// focus can refer to a pane across layout changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u64);

/// A node in the layout tree.
///
/// Leaves hold a pane; inner nodes hold two children and the proportion of
/// space given to the first. The tree is a plain value: layout operations are
/// functions that return a new tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// A leaf: one pane with one terminal inside it.
    Pane { id: PaneId },
    /// An inner node dividing `first` and `second`.
    Split {
        orientation: Orientation,
        /// Fraction of the available space given to `first`, in `0.0..=1.0`.
        ///
        /// Stored as a ratio rather than a pixel position because ratios
        /// survive window resizes and are what layout persistence needs.
        ratio: f64,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    /// A tree containing exactly one pane.
    pub fn single_pane(id: PaneId) -> Self {
        Node::Pane { id }
    }

    /// The id of the first pane in tree order.
    #[cfg(test)]
    pub fn first_pane(&self) -> PaneId {
        match self {
            Node::Pane { id } => *id,
            Node::Split { first, .. } => first.first_pane(),
        }
    }

    /// Every pane in the tree, left to right and top to bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane { id } => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }

    /// Number of panes in the tree.
    pub fn len(&self) -> usize {
        self.panes().len()
    }

    /// Always false: a tree root is at minimum a single pane.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Replace the pane `target` with a split of it and a new pane.
    ///
    /// The new pane becomes the second child and receives focus afterwards;
    /// the caller decides focus. The ratio starts even.
    pub fn split_pane(
        &self,
        target: PaneId,
        orientation: Orientation,
        new_pane: PaneId,
    ) -> Option<Node> {
        let mut next = self.clone();
        if next.replace_pane_with_split(target, orientation, new_pane) {
            Some(next)
        } else {
            None
        }
    }

    fn replace_pane_with_split(
        &mut self,
        target: PaneId,
        orientation: Orientation,
        new_pane: PaneId,
    ) -> bool {
        match self {
            Node::Pane { id } if *id == target => {
                *self = Node::Split {
                    orientation,
                    ratio: 0.5,
                    first: Box::new(Node::Pane { id: target }),
                    second: Box::new(Node::Pane { id: new_pane }),
                };
                true
            }
            Node::Pane { .. } => false,
            Node::Split { first, second, .. } => {
                first.replace_pane_with_split(target, orientation, new_pane)
                    || second.replace_pane_with_split(target, orientation, new_pane)
            }
        }
    }

    /// Move a pane to sit beside a target, dividing the target's area.
    ///
    /// The moved pane keeps its identity — and therefore its terminal and any
    /// content scoped to it — because it is detached and re-inserted rather than
    /// copied. The target's area is divided evenly, exactly as a split created
    /// there by hand would be.
    pub fn move_pane(
        &self,
        moved: PaneId,
        target: PaneId,
        orientation: Orientation,
        placement: Placement,
    ) -> Option<Node> {
        if moved == target {
            return None;
        }
        let panes = self.panes();
        if !panes.contains(&moved) || !panes.contains(&target) {
            return None;
        }
        // Detaching uses the same collapse rules as closing, so a split that is
        // left with one child becomes that child rather than an empty divider.
        let without = self.close_pane(moved)?;
        let mut next = without;
        if next.insert_beside(target, orientation, moved, placement) {
            Some(next)
        } else {
            None
        }
    }

    /// Replace the leaf `target` with a split placing `other` beside it.
    fn insert_beside(
        &mut self,
        target: PaneId,
        orientation: Orientation,
        other: PaneId,
        placement: Placement,
    ) -> bool {
        match self {
            Node::Pane { id } if *id == target => {
                let other_leaf = Node::Pane { id: other };
                let target_leaf = Node::Pane { id: target };
                let (first, second) = match placement {
                    Placement::Before => (other_leaf, target_leaf),
                    Placement::After => (target_leaf, other_leaf),
                };
                *self = Node::Split {
                    orientation,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            Node::Pane { .. } => false,
            Node::Split { first, second, .. } => {
                first.insert_beside(target, orientation, other, placement)
                    || second.insert_beside(target, orientation, other, placement)
            }
        }
    }

    /// Remove a pane, reconnecting its sibling so no empty gap remains.
    ///
    /// Returns `None` when the pane is not present, or when it is the only pane
    /// in the tree: removing the last pane would leave nothing to render, and
    /// the caller escalates to closing the workspace instead.
    pub fn close_pane(&self, target: PaneId) -> Option<Node> {
        if self.len() <= 1 || !self.panes().contains(&target) {
            return None;
        }
        let mut next = self.clone();
        if next.detach_pane(target) {
            Some(next)
        } else {
            None
        }
    }

    /// Choose within the branch that will reclaim this leaf's space, before it
    /// disappears. Tree order breaks ties only inside that sibling subtree.
    pub fn closing_focus(&self, target: PaneId) -> Option<PaneId> {
        match self {
            Node::Pane { .. } => None,
            Node::Split { first, second, .. } => {
                if matches!(first.as_ref(), Node::Pane { id } if *id == target) {
                    second.panes().first().copied()
                } else if matches!(second.as_ref(), Node::Pane { id } if *id == target) {
                    first.panes().first().copied()
                } else {
                    first
                        .closing_focus(target)
                        .or_else(|| second.closing_focus(target))
                }
            }
        }
    }

    /// Remove a pane and replace its parent split with the surviving sibling.
    ///
    /// A split whose only remaining content is a single child collapses to
    /// that child, so no empty divider is ever left behind.
    fn detach_pane(&mut self, target: PaneId) -> bool {
        match self.without_pane(target) {
            Some(replacement) => {
                *self = replacement;
                true
            }
            None => false,
        }
    }

    /// The subtree with `target` removed, or `None` if `target` is absent.
    ///
    /// Splits collapse automatically: a split with one surviving child becomes
    /// that child, which is what removes the now-meaningless divider.
    fn without_pane(&self, target: PaneId) -> Option<Node> {
        match self {
            Node::Pane { id } => (*id != target).then(|| self.clone()),
            Node::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                // A leaf cannot tell "removed, now empty" apart from "absent",
                // so membership is resolved here before recursing.
                let in_first = first.panes().contains(&target);
                let in_second = !in_first && second.panes().contains(&target);
                if !in_first && !in_second {
                    return None;
                }

                let new_first: Box<Node> = if in_first {
                    match first.without_pane(target) {
                        Some(node) => Box::new(node),
                        // The first child held only the target pane, so this
                        // split collapses onto the second child.
                        None => return Some(*second.clone()),
                    }
                } else {
                    first.clone()
                };

                let new_second: Box<Node> = if in_second {
                    match second.without_pane(target) {
                        Some(node) => Box::new(node),
                        // Symmetrically, collapse onto the surviving first child.
                        None => return Some(*new_first),
                    }
                } else {
                    second.clone()
                };

                Some(Node::Split {
                    orientation: *orientation,
                    ratio: *ratio,
                    first: new_first,
                    second: new_second,
                })
            }
        }
    }

    /// Update the ratio of the split that directly contains `pane`.
    #[cfg(test)]
    pub fn set_ratio_for_pane(&mut self, pane: PaneId, ratio: f64) -> bool {
        let ratio = ratio.clamp(0.05, 0.95);
        match self {
            Node::Pane { .. } => false,
            Node::Split {
                ratio: r,
                first,
                second,
                ..
            } => {
                if matches!(**first, Node::Pane { id } if id == pane) {
                    *r = ratio;
                    true
                } else if matches!(**second, Node::Pane { id } if id == pane) {
                    *r = 1.0 - ratio;
                    true
                } else {
                    first.set_ratio_for_pane(pane, ratio) || second.set_ratio_for_pane(pane, ratio)
                }
            }
        }
    }

    /// Set the ratio of the split at a path, where the path selects the first
    /// child as `0` and the second as `1`.
    ///
    /// A divider drag arrives as a widget with no identity of its own, so the
    /// widget tree carries the path and the ratio is written back by path. That
    /// keeps the tree authoritative without tagging widgets.
    pub fn set_ratio_at(&mut self, path: &[usize], ratio: f64) -> bool {
        let ratio = ratio.clamp(0.05, 0.95);
        match path.first() {
            None => match self {
                Node::Split { ratio: r, .. } => {
                    *r = ratio;
                    true
                }
                Node::Pane { .. } => false,
            },
            Some(&index) => match self {
                Node::Split { first, second, .. } => {
                    let child = if index == 0 { first } else { second };
                    child.set_ratio_at(&path[1..], ratio)
                }
                Node::Pane { .. } => false,
            },
        }
    }

    /// Read the ratio of the split at a path.
    /// The orientation of the split at `path`, or `None` when the path does not
    /// name a split.
    pub fn orientation_at(&self, path: &[usize]) -> Option<Orientation> {
        match (self, path.first()) {
            (Node::Split { orientation, .. }, None) => Some(*orientation),
            (Node::Split { first, second, .. }, Some(&index)) => {
                let child = if index == 0 { first } else { second };
                child.orientation_at(&path[1..])
            }
            (Node::Pane { .. }, _) => None,
        }
    }

    pub fn ratio_at(&self, path: &[usize]) -> Option<f64> {
        match path.first() {
            None => match self {
                Node::Split { ratio, .. } => Some(*ratio),
                Node::Pane { .. } => None,
            },
            Some(&index) => match self {
                Node::Split { first, second, .. } => {
                    let child = if index == 0 { first } else { second };
                    child.ratio_at(&path[1..])
                }
                Node::Pane { .. } => None,
            },
        }
    }

    /// Apply a function to every split in the tree.
    ///
    /// Takes a function reference rather than a generic `FnMut` bound: a
    /// generic closure parameter would monomorphise once per recursion level
    /// and blow the recursion limit.
    #[cfg(test)]
    pub fn for_each_split_mut(&mut self, f: &mut dyn FnMut(&mut Orientation, &mut f64)) {
        match self {
            Node::Pane { .. } => {}
            Node::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                f(orientation, ratio);
                first.for_each_split_mut(f);
                second.for_each_split_mut(f);
            }
        }
    }
}

/// Which side of a target a moved pane is inserted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Before,
    After,
}

/// Which part of a pane a drop landed on.
///
/// A drop names an edge rather than a raw position, because the result is a
/// split along that edge and nothing else about the pointer matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropSide {
    Left,
    Right,
    Top,
    Bottom,
}

impl DropSide {
    /// The orientation that puts the dropped pane on this side.
    pub fn orientation(self) -> Orientation {
        match self {
            DropSide::Left | DropSide::Right => Orientation::Vertical,
            DropSide::Top | DropSide::Bottom => Orientation::Horizontal,
        }
    }

    /// Whether the dropped pane goes before the target in tree order.
    pub fn placement(self) -> Placement {
        match self {
            DropSide::Left | DropSide::Top => Placement::Before,
            DropSide::Right | DropSide::Bottom => Placement::After,
        }
    }
}

/// Decide which edge of a pane a pointer position is nearest.
///
/// Distances are compared as fractions of the pane's extent, not in pixels.
/// Comparing pixels would make the answer depend on the pane's shape: in a wide
/// pane the centre is closer to the top or bottom edge in pixels, so a drop in
/// the middle would split the wrong way. Normalised distances put the zone
/// boundaries on the pane's diagonals, which is what the interaction should
/// feel like.
///
/// A position exactly on a diagonal is a tie. Ties resolve to the vertical
/// split, because most panes are wider than they are tall and side-by-side is
/// the division a user reaches for by default.
pub fn drop_side(x: i32, y: i32, width: i32, height: i32) -> DropSide {
    if width <= 0 || height <= 0 {
        return DropSide::Right;
    }
    let fx = (x as f64 / width as f64).clamp(0.0, 1.0);
    let fy = (y as f64 / height as f64).clamp(0.0, 1.0);
    // Distance to the nearest vertical edge and to the nearest horizontal edge,
    // each as a fraction of the pane.
    let to_vertical = fx.min(1.0 - fx);
    let to_horizontal = fy.min(1.0 - fy);

    if to_vertical <= to_horizontal {
        if fx < 0.5 {
            DropSide::Left
        } else {
            DropSide::Right
        }
    } else if fy < 0.5 {
        DropSide::Top
    } else {
        DropSide::Bottom
    }
}

/// A direction for moving focus between panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    /// The axis the direction moves along.
    fn axis(self) -> Axis {
        match self {
            Direction::Up | Direction::Down => Axis::Vertical,
            Direction::Left | Direction::Right => Axis::Horizontal,
        }
    }

    /// Sign along the axis: negative is up/left, positive is down/right.
    fn sign(self) -> f64 {
        match self {
            Direction::Up | Direction::Left => -1.0,
            Direction::Down | Direction::Right => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// A pane's position in the layout, for directional focus lookup.
#[derive(Debug, Clone, Copy)]
struct Position {
    pane: PaneId,
    horizontal: (f64, f64),
    vertical: (f64, f64),
}

/// Compute geometric ranges for every pane in the tree.
///
/// Ranges are in `0.0..=1.0` of the root's extent, which makes comparison
/// independent of actual pixel sizes.
fn positions(node: &Node) -> Vec<Position> {
    fn walk(node: &Node, h: (f64, f64), v: (f64, f64), out: &mut Vec<Position>) {
        match node {
            Node::Pane { id } => out.push(Position {
                pane: *id,
                horizontal: h,
                vertical: v,
            }),
            Node::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                let (lo, hi, axis) = match orientation {
                    Orientation::Vertical => (h.0, h.1, Axis::Horizontal),
                    Orientation::Horizontal => (v.0, v.1, Axis::Vertical),
                };
                let mid = lo + (hi - lo) * ratio;
                let (first_range, second_range) = match axis {
                    Axis::Horizontal => ((lo, mid), (mid, hi)),
                    Axis::Vertical => ((lo, mid), (mid, hi)),
                };
                match orientation {
                    Orientation::Vertical => {
                        walk(first, first_range, v, out);
                        walk(second, second_range, v, out);
                    }
                    Orientation::Horizontal => {
                        walk(first, h, first_range, out);
                        walk(second, h, second_range, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(node, (0.0, 1.0), (0.0, 1.0), &mut out);
    out
}

/// Find the nearest pane in `direction` from `from`.
///
/// A pane qualifies when it lies beyond `from` on the movement axis and
/// overlaps `from` on the other axis. Among candidates, the closest edge wins;
/// ties are broken by perpendicular distance so the choice is deterministic.
pub fn neighbour_in_direction(node: &Node, from: PaneId, direction: Direction) -> Option<PaneId> {
    let all = positions(node);
    let origin = all.iter().find(|p| p.pane == from)?;

    let (axis, sign) = (direction.axis(), direction.sign());
    let (origin_range, other_range) = match axis {
        Axis::Horizontal => (origin.horizontal, origin.vertical),
        Axis::Vertical => (origin.vertical, origin.horizontal),
    };

    let origin_edge = if sign > 0.0 {
        origin_range.1
    } else {
        origin_range.0
    };

    let mut best: Option<(f64, f64, PaneId)> = None;
    for candidate in &all {
        if candidate.pane == from {
            continue;
        }
        let (cand_range, cand_other) = match axis {
            Axis::Horizontal => (candidate.horizontal, candidate.vertical),
            Axis::Vertical => (candidate.vertical, candidate.horizontal),
        };

        let beyond = if sign > 0.0 {
            cand_range.0 >= origin_range.1 - 1e-9
        } else {
            cand_range.1 <= origin_range.0 + 1e-9
        };
        let overlaps = cand_other.0 < other_range.1 - 1e-9 && cand_other.1 > other_range.0 + 1e-9;
        if !beyond || !overlaps {
            continue;
        }

        let edge = if sign > 0.0 {
            cand_range.0
        } else {
            cand_range.1
        };
        let along = (edge - origin_edge).abs();
        let overlap = (cand_other.1.min(other_range.1) - cand_other.0.max(other_range.0)).abs();
        let perp = if overlap > 1e-9 {
            (cand_other.0 - other_range.0)
                .abs()
                .min((other_range.1 - cand_other.1).abs())
        } else {
            overlap
        };

        let better = match best {
            None => true,
            Some((best_along, best_perp, _)) => {
                along < best_along - 1e-9
                    || ((along - best_along).abs() <= 1e-9 && perp < best_perp - 1e-9)
            }
        };
        if better {
            best = Some((along, perp, candidate.pane));
        }
    }
    best.map(|(_, _, pane)| pane)
}

/// Choose a split orientation from a pane's shape.
///
/// A pane at least as wide as it is tall is divided side by side; a taller pane
/// is stacked. Comparing the two extents directly (rather than their ratio)
/// matches the intuition that a wide area divides better across.
pub fn auto_orientation(width: i32, height: i32) -> Orientation {
    if width >= height {
        Orientation::Vertical
    } else {
        Orientation::Horizontal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> PaneId {
        PaneId(n)
    }

    #[test]
    fn single_pane_tree_has_one_pane() {
        let tree = Node::single_pane(id(1));
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.panes(), vec![id(1)]);
        assert_eq!(tree.first_pane(), id(1));
        assert!(!tree.is_empty());
    }

    #[test]
    fn split_replaces_only_the_target_pane() {
        let tree = Node::single_pane(id(1));
        let next = tree
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();

        assert_eq!(next.len(), 2);
        assert_eq!(next.panes(), vec![id(1), id(2)]);
        match next {
            Node::Split {
                ratio,
                orientation,
                first,
                second,
            } => {
                assert_eq!(ratio, 0.5);
                assert_eq!(orientation, Orientation::Vertical);
                assert_eq!(*first, Node::Pane { id: id(1) });
                assert_eq!(*second, Node::Pane { id: id(2) });
            }
            other => panic!("expected a split at the root, got {other:?}"),
        }
    }

    #[test]
    fn split_of_missing_pane_is_rejected() {
        let tree = Node::single_pane(id(1));
        assert!(tree
            .split_pane(id(99), Orientation::Vertical, id(2))
            .is_none());
    }

    #[test]
    fn nested_split_preserves_untouched_panes() {
        // (1 2) then split pane 2 -> ((1 (2 3)))
        let tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        let nested = tree
            .split_pane(id(2), Orientation::Horizontal, id(3))
            .unwrap();

        assert_eq!(nested.len(), 3);
        assert_eq!(nested.panes(), vec![id(1), id(2), id(3)]);

        // The original outer split keeps its orientation and ratio.
        match &nested {
            Node::Split {
                orientation,
                ratio,
                second,
                ..
            } => {
                assert_eq!(*orientation, Orientation::Vertical);
                assert_eq!(*ratio, 0.5);
                match **second {
                    Node::Split { orientation, .. } => {
                        assert_eq!(orientation, Orientation::Horizontal)
                    }
                    ref other => panic!("expected nested split, got {other:?}"),
                }
            }
            other => panic!("expected outer split, got {other:?}"),
        }
    }

    #[test]
    fn closing_a_leaf_promotes_its_sibling() {
        let tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        let closed = tree.close_pane(id(2)).unwrap();

        assert_eq!(closed, Node::Pane { id: id(1) });
    }

    #[test]
    fn closing_middle_of_three_joins_the_others() {
        // ((1 (2 3)) -> ((1 3)) is not produced; instead (1 (2 3)) closing 2 -> (1 3)
        let tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        let tree = tree
            .split_pane(id(2), Orientation::Horizontal, id(3))
            .unwrap();
        let closed = tree.close_pane(id(2)).unwrap();

        assert_eq!(closed.panes(), vec![id(1), id(3)]);
        // A single divider remains between the two survivors.
        match closed {
            Node::Split { ratio, .. } => assert_eq!(ratio, 0.5),
            other => panic!("expected a rejoined split, got {other:?}"),
        }
    }

    #[test]
    fn closing_first_of_three_joins_the_others() {
        let tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        let tree = tree
            .split_pane(id(2), Orientation::Vertical, id(3))
            .unwrap();
        let closed = tree.close_pane(id(1)).unwrap();

        assert_eq!(closed.panes(), vec![id(2), id(3)]);
        assert_eq!(closed.len(), 2);
    }

    #[test]
    fn closing_last_pane_is_rejected() {
        let tree = Node::single_pane(id(1));
        assert!(tree.close_pane(id(1)).is_none());
    }

    #[test]
    fn closing_missing_pane_is_rejected() {
        let tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        assert!(tree.close_pane(id(99)).is_none());
    }

    #[test]
    fn auto_orientation_picks_the_larger_axis() {
        assert_eq!(auto_orientation(400, 200), Orientation::Vertical);
        assert_eq!(auto_orientation(200, 400), Orientation::Horizontal);
        assert_eq!(auto_orientation(300, 300), Orientation::Vertical);
    }

    #[test]
    fn set_ratio_targets_the_containing_split() {
        let mut tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        assert!(tree.set_ratio_for_pane(id(2), 0.3));
        match &tree {
            Node::Split { ratio, .. } => assert!((ratio - 0.7).abs() < 1e-9),
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn set_ratio_is_clamped_away_from_degenerate_values() {
        let mut tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        tree.set_ratio_for_pane(id(1), 0.0);
        match &tree {
            Node::Split { ratio, .. } => assert!(*ratio >= 0.05),
            other => panic!("expected split, got {other:?}"),
        }
    }

    fn three_across() -> Node {
        // (1 2) then split pane 2 horizontally -> (1 | (2 / 3))
        Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap()
            .split_pane(id(2), Orientation::Horizontal, id(3))
            .unwrap()
    }

    #[test]
    fn direction_finds_the_adjacent_pane() {
        let tree = three_across();
        assert_eq!(
            neighbour_in_direction(&tree, id(1), Direction::Right),
            Some(id(2))
        );
        assert_eq!(
            neighbour_in_direction(&tree, id(2), Direction::Down),
            Some(id(3))
        );
        assert_eq!(neighbour_in_direction(&tree, id(2), Direction::Up), None);
    }

    #[test]
    fn direction_without_neighbour_returns_none() {
        let tree = three_across();
        // Nothing above pane 1, nothing below pane 3, nothing above pane 2.
        assert_eq!(neighbour_in_direction(&tree, id(1), Direction::Up), None);
        assert_eq!(neighbour_in_direction(&tree, id(3), Direction::Down), None);
        assert_eq!(neighbour_in_direction(&tree, id(2), Direction::Up), None);
    }

    #[test]
    fn direction_uses_the_overlapping_pane_not_just_the_aligned_one() {
        // Layout is 1 | (2 / 3), so pane 1 spans the full height and therefore
        // overlaps pane 3 vertically. Moving left from pane 3 must land on pane
        // 1 even though pane 3 is not directly beside it in the same row.
        let tree = three_across();
        assert_eq!(
            neighbour_in_direction(&tree, id(3), Direction::Left),
            Some(id(1))
        );
        // Pane 2 sits directly above pane 3, so Up from 3 finds it.
        assert_eq!(
            neighbour_in_direction(&tree, id(3), Direction::Up),
            Some(id(2))
        );
    }

    #[test]
    fn direction_ignores_the_pane_itself() {
        let tree = three_across();
        assert_ne!(
            neighbour_in_direction(&tree, id(2), Direction::Left),
            Some(id(2))
        );
    }

    #[test]
    fn every_pane_has_a_neighbour_in_some_direction() {
        let tree = three_across();
        for pane in tree.panes() {
            let any = [
                Direction::Up,
                Direction::Down,
                Direction::Left,
                Direction::Right,
            ]
            .into_iter()
            .any(|d| neighbour_in_direction(&tree, pane, d).is_some());
            assert!(any, "pane {pane:?} has no neighbour in any direction");
        }
    }

    #[test]
    fn set_ratio_at_reaches_a_nested_split_by_path() {
        let mut tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap()
            .split_pane(id(2), Orientation::Vertical, id(3))
            .unwrap();

        // Path [1] is the second child (the nested split).
        assert_eq!(tree.ratio_at(&[1]), Some(0.5));
        assert!(tree.set_ratio_at(&[1], 0.3));
        assert!((tree.ratio_at(&[1]).unwrap() - 0.3).abs() < 1e-9);
        // The outer split is untouched.
        assert!((tree.ratio_at(&[]).unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn set_ratio_at_rejects_a_path_into_a_leaf() {
        let mut tree = Node::single_pane(id(1));
        assert!(!tree.set_ratio_at(&[0], 0.5));
        assert_eq!(tree.ratio_at(&[0]), None);
    }

    #[test]
    fn set_ratio_at_clamps() {
        let mut tree = Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap();
        tree.set_ratio_at(&[], 0.99);
        assert!(tree.ratio_at(&[]).unwrap() <= 0.95);
    }

    // --- moving a pane (tasks 1.1 - 1.7) ---

    fn two_panes() -> Node {
        Node::single_pane(id(1))
            .split_pane(id(1), Orientation::Vertical, id(2))
            .unwrap()
    }

    fn three_panes() -> Node {
        two_panes()
            .split_pane(id(2), Orientation::Vertical, id(3))
            .unwrap()
    }

    #[test]
    fn moving_a_pane_presents_it_once_and_closes_its_old_position() {
        // Start (1 | (2 | 3)). Moving 3 beside 1 detaches it — collapsing the
        // nested split so 2 stands alone — and reinserts it next to 1, giving
        // ((1 3) | 2). Every pane is present exactly once, including the one
        // the moved pane left behind.
        let tree = three_panes();
        let moved = tree
            .move_pane(id(3), id(1), Orientation::Vertical, Placement::After)
            .unwrap();
        assert_eq!(moved.panes(), vec![id(1), id(3), id(2)]);
        for pane in [id(1), id(2), id(3)] {
            assert_eq!(
                moved.panes().iter().filter(|p| **p == pane).count(),
                1,
                "{pane:?} appears more than once"
            );
        }
    }

    #[test]
    fn moving_keeps_the_pane_count() {
        let tree = three_panes();
        let before = tree.len();
        let moved = tree
            .move_pane(id(3), id(1), Orientation::Horizontal, Placement::Before)
            .unwrap();
        assert_eq!(moved.len(), before);
    }

    #[test]
    fn placement_decides_which_side_the_pane_lands_on() {
        let tree = two_panes();

        let after = tree
            .move_pane(id(2), id(1), Orientation::Vertical, Placement::After)
            .unwrap();
        assert_eq!(after.panes(), vec![id(1), id(2)]);

        let before = tree
            .move_pane(id(2), id(1), Orientation::Vertical, Placement::Before)
            .unwrap();
        assert_eq!(before.panes(), vec![id(2), id(1)]);
    }

    #[test]
    fn placement_matches_the_drop_side() {
        assert_eq!(DropSide::Left.placement(), Placement::Before);
        assert_eq!(DropSide::Right.placement(), Placement::After);
        assert_eq!(DropSide::Top.placement(), Placement::Before);
        assert_eq!(DropSide::Bottom.placement(), Placement::After);
        assert_eq!(DropSide::Left.orientation(), Orientation::Vertical);
        assert_eq!(DropSide::Bottom.orientation(), Orientation::Horizontal);
    }

    #[test]
    fn moving_into_a_nested_layout_leaves_outsiders_alone() {
        // (1 | (2 / 3)) : move 3 onto 1's right. The nested split collapses and
        // pane 2 keeps its own orientation and ratio.
        let tree = two_panes()
            .split_pane(id(2), Orientation::Horizontal, id(3))
            .unwrap();
        let moved = tree
            .move_pane(id(3), id(1), Orientation::Vertical, Placement::After)
            .unwrap();
        assert_eq!(moved.panes(), vec![id(1), id(3), id(2)]);
        // Pane 2 is untouched: it is still a leaf at full height of its branch.
        match &moved {
            Node::Split {
                orientation, ratio, ..
            } => {
                assert_eq!(*orientation, Orientation::Vertical);
                assert_eq!(*ratio, 0.5);
            }
            other => panic!("expected a split, got {other:?}"),
        }
    }

    #[test]
    fn moving_a_pane_onto_itself_is_refused() {
        let tree = three_panes();
        assert!(tree
            .move_pane(id(2), id(2), Orientation::Vertical, Placement::After)
            .is_none());
        assert_eq!(tree.panes(), vec![id(1), id(2), id(3)]);
    }

    #[test]
    fn moving_in_a_single_pane_tree_is_refused() {
        let tree = Node::single_pane(id(1));
        assert!(tree
            .move_pane(id(1), id(1), Orientation::Vertical, Placement::After)
            .is_none());
        assert_eq!(tree.panes(), vec![id(1)]);
    }

    #[test]
    fn moving_an_unknown_pane_or_onto_an_unknown_target_is_refused() {
        let tree = two_panes();
        assert!(tree
            .move_pane(id(9), id(1), Orientation::Vertical, Placement::After)
            .is_none());
        assert!(tree
            .move_pane(id(2), id(9), Orientation::Vertical, Placement::After)
            .is_none());
    }

    #[test]
    fn a_moved_pane_keeps_its_id() {
        let tree = three_panes();
        let moved = tree
            .move_pane(id(2), id(1), Orientation::Horizontal, Placement::After)
            .unwrap();
        assert!(moved.panes().contains(&id(2)));
    }

    // --- drop zone geometry (task 3.1) ---

    #[test]
    fn drop_sides_follow_the_nearest_edge() {
        // 400x400: the diagonals split the pane into four triangles.
        assert_eq!(drop_side(40, 200, 400, 400), DropSide::Left);
        assert_eq!(drop_side(360, 200, 400, 400), DropSide::Right);
        assert_eq!(drop_side(200, 40, 400, 400), DropSide::Top);
        assert_eq!(drop_side(200, 360, 400, 400), DropSide::Bottom);
    }

    #[test]
    fn a_wide_pane_does_not_flip_its_zones() {
        // The centre of a wide pane must still resolve to a side, not to top or
        // bottom, which is what comparing raw pixels would produce.
        // On the centre row, a position left of centre is a left drop and one
        // right of centre is a right drop; neither becomes top or bottom.
        assert_eq!(drop_side(400, 100, 1200, 200), DropSide::Left);
        assert_eq!(drop_side(800, 100, 1200, 200), DropSide::Right);
        // Near the top edge, the top zone wins even in a wide pane.
        assert_eq!(drop_side(600, 5, 1200, 200), DropSide::Top);
    }

    #[test]
    fn the_centre_resolves_to_an_edge_rather_than_nothing() {
        // A tie resolves to the vertical split; the important part is that it
        // resolves at all, because a centre drop must not destroy the target.
        let side = drop_side(200, 200, 400, 400);
        assert!(matches!(side, DropSide::Left | DropSide::Right));
    }

    #[test]
    fn a_degenerate_pane_still_yields_a_side() {
        assert_eq!(drop_side(0, 0, 0, 0), DropSide::Right);
        assert_eq!(drop_side(0, 0, 0, 100), DropSide::Right);
    }

    #[test]
    fn positions_outside_the_pane_are_clamped() {
        assert_eq!(drop_side(-50, 200, 400, 400), DropSide::Left);
        assert_eq!(drop_side(9999, 200, 400, 400), DropSide::Right);
    }

    #[test]
    fn every_position_yields_some_side() {
        for x in (0..400).step_by(37) {
            for y in (0..400).step_by(41) {
                let _ = drop_side(x, y, 400, 400);
            }
        }
    }

    #[test]
    fn for_each_split_visits_every_inner_node() {
        let tree = three_across();
        let mut count = 0;
        let mut tree = tree;
        tree.for_each_split_mut(&mut |_, _| count += 1);
        assert_eq!(count, 2);
    }
}
