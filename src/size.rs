//! Reconciling the three size units one layout passes through.
//!
//! The layout tree stores ratios, GTK allocates in pixels, and a terminal
//! reports its size in character cells. All three meet here so that the
//! conversion lives in one place instead of being spread across every pane.

/// A terminal's cell metrics, as reported by VTE.
///
/// The default of zero is deliberate: before VTE reports a size, "unknown" and
/// "zero-sized" behave identically, since both make a cell conversion unsafe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellSize {
    pub width: i32,
    pub height: i32,
}

impl CellSize {
    pub fn is_usable(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

/// A terminal's size expressed both in cells and in pixels.
///
/// Keeping the pixel allocation alongside the cell count lets a pane detect
/// that it was resized without another signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub columns: i64,
    pub rows: i64,
    pub pixel_width: i32,
    pub pixel_height: i32,
}

/// Convert a pixel allocation into a cell grid.
///
/// Clamps to at least 1x1: a pane dragged smaller than a single cell has no
/// valid cell count, and reporting zero would make programs divide by zero or
/// draw nothing at all.
pub fn cells_for(pixel_width: i32, pixel_height: i32, cell: CellSize) -> (i64, i64) {
    if !cell.is_usable() {
        return (1, 1);
    }
    let columns = (pixel_width / cell.width).max(1) as i64;
    let rows = (pixel_height / cell.height).max(1) as i64;
    (columns, rows)
}

/// The size a terminal should report for a given allocation.
pub fn terminal_size(pixel_width: i32, pixel_height: i32, cell: CellSize) -> TermSize {
    let (columns, rows) = cells_for(pixel_width, pixel_height, cell);
    TermSize {
        columns,
        rows,
        pixel_width,
        pixel_height,
    }
}

/// The pixel box a pane occupies within a layout of the given size.
///
/// Computed from the tree's ratios rather than read from a widget: a widget's
/// allocation is unavailable between being reparented and the next allocation
/// pass, which is exactly the moment a split asks for the pane's shape.
pub fn pane_box(
    node: &crate::tree::Node,
    width: i32,
    height: i32,
    target: crate::tree::PaneId,
) -> Option<(i32, i32)> {
    if width <= 0 || height <= 0 {
        return None;
    }
    match node {
        crate::tree::Node::Pane { id } => (*id == target).then_some((width, height)),
        crate::tree::Node::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let ratio = ratio.clamp(0.0, 1.0);
            let total = match orientation {
                crate::tree::Orientation::Vertical => width,
                crate::tree::Orientation::Horizontal => height,
            };
            // The divider takes one pixel out of what the children share.
            let usable = (total - 1).max(0);
            let first_size = (usable as f64 * ratio).round() as i32;
            let second_size = usable - first_size;
            match orientation {
                crate::tree::Orientation::Vertical => {
                    let (fw, sw) = (first_size, second_size);
                    pane_box(first, fw, height, target)
                        .or_else(|| pane_box(second, sw, height, target))
                }
                crate::tree::Orientation::Horizontal => {
                    let (fh, sh) = (first_size, second_size);
                    pane_box(first, width, fh, target)
                        .or_else(|| pane_box(second, width, sh, target))
                }
            }
        }
    }
}

/// The absolute rectangle a pane occupies within a layout of the given size.
///
/// Position as well as size, because a test that drives the pointer has to know
/// where a pane actually is on screen, not only how big it is.
pub fn pane_rect(
    node: &crate::tree::Node,
    width: i32,
    height: i32,
    target: crate::tree::PaneId,
) -> Option<(i32, i32, i32, i32)> {
    fn walk(
        node: &crate::tree::Node,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        target: crate::tree::PaneId,
    ) -> Option<(i32, i32, i32, i32)> {
        match node {
            crate::tree::Node::Pane { id } => (*id == target).then_some((x, y, w, h)),
            crate::tree::Node::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                let ratio = ratio.clamp(0.0, 1.0);
                let total = match orientation {
                    crate::tree::Orientation::Vertical => w,
                    crate::tree::Orientation::Horizontal => h,
                };
                let usable = (total - 1).max(0);
                let first_size = (usable as f64 * ratio).round() as i32;
                let second_size = usable - first_size;
                match orientation {
                    crate::tree::Orientation::Vertical => walk(first, x, y, first_size, h, target)
                        .or_else(|| walk(second, x + first_size + 1, y, second_size, h, target)),
                    crate::tree::Orientation::Horizontal => {
                        walk(first, x, y, w, first_size, target)
                            .or_else(|| walk(second, x, y + first_size + 1, w, second_size, target))
                    }
                }
            }
        }
    }
    if width <= 0 || height <= 0 {
        return None;
    }
    walk(node, 0, 0, width, height, target)
}

/// Whether a resize actually changed the reported terminal size.
///
/// Comparing whole cells rather than raw pixels is deliberate: a divider moving
/// inside one cell is not a resize the terminal should react to, and pushing
/// it would make full-screen programs redraw constantly.
pub fn size_changed(previous: Option<TermSize>, next: TermSize) -> bool {
    match previous {
        None => true,
        Some(prev) => prev.columns != next.columns || prev.rows != next.rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellSize = CellSize {
        width: 9,
        height: 18,
    };

    #[test]
    fn a_pane_box_is_computed_from_the_tree() {
        use crate::tree::{Node, Orientation, PaneId};
        let tree = Node::single_pane(PaneId(1))
            .split_pane(PaneId(1), Orientation::Vertical, PaneId(2))
            .unwrap();
        // 801 usable pixels split evenly, divider excluded.
        assert_eq!(pane_box(&tree, 801, 600, PaneId(1)), Some((400, 600)));
        assert_eq!(pane_box(&tree, 801, 600, PaneId(2)), Some((400, 600)));
    }

    #[test]
    fn a_nested_pane_box_reflects_its_branch() {
        use crate::tree::{Node, Orientation, PaneId};
        let tree = Node::single_pane(PaneId(1))
            .split_pane(PaneId(1), Orientation::Vertical, PaneId(2))
            .unwrap()
            .split_pane(PaneId(2), Orientation::Horizontal, PaneId(3))
            .unwrap();
        // Pane 1 keeps full height; panes 2 and 3 share the right half's height.
        assert_eq!(pane_box(&tree, 801, 601, PaneId(1)), Some((400, 601)));
        assert_eq!(pane_box(&tree, 801, 601, PaneId(2)), Some((400, 300)));
        assert_eq!(pane_box(&tree, 801, 601, PaneId(3)), Some((400, 300)));
    }

    #[test]
    fn a_pane_rect_carries_its_position() {
        use crate::tree::{Node, Orientation, PaneId};
        let tree = Node::single_pane(PaneId(1))
            .split_pane(PaneId(1), Orientation::Vertical, PaneId(2))
            .unwrap();
        assert_eq!(
            pane_rect(&tree, 801, 600, PaneId(1)),
            Some((0, 0, 400, 600))
        );
        // The second pane starts after the first pane and the divider.
        assert_eq!(
            pane_rect(&tree, 801, 600, PaneId(2)),
            Some((401, 0, 400, 600))
        );
    }

    #[test]
    fn a_stacked_pane_rect_carries_a_y_offset() {
        use crate::tree::{Node, Orientation, PaneId};
        let tree = Node::single_pane(PaneId(1))
            .split_pane(PaneId(1), Orientation::Horizontal, PaneId(2))
            .unwrap();
        assert_eq!(
            pane_rect(&tree, 800, 601, PaneId(2)),
            Some((0, 301, 800, 300))
        );
    }

    #[test]
    fn an_unknown_pane_has_no_box() {
        use crate::tree::{Node, PaneId};
        let tree = Node::single_pane(PaneId(1));
        assert_eq!(pane_box(&tree, 800, 600, PaneId(9)), None);
    }

    #[test]
    fn pixels_convert_to_whole_cells() {
        assert_eq!(cells_for(720, 360, CELL), (80, 20));
        assert_eq!(cells_for(721, 359, CELL), (80, 19));
    }

    #[test]
    fn an_allocation_smaller_than_one_cell_clamps_to_one() {
        assert_eq!(cells_for(3, 5, CELL), (1, 1));
        assert_eq!(cells_for(0, 0, CELL), (1, 1));
        assert_eq!(cells_for(-10, -10, CELL), (1, 1));
    }

    #[test]
    fn unusable_cell_metrics_do_not_divide_by_zero() {
        let degenerate = CellSize {
            width: 0,
            height: 0,
        };
        assert_eq!(cells_for(720, 360, degenerate), (1, 1));
    }

    #[test]
    fn terminal_size_keeps_the_pixel_allocation() {
        let size = terminal_size(720, 360, CELL);
        assert_eq!(size.columns, 80);
        assert_eq!(size.rows, 20);
        assert_eq!(size.pixel_width, 720);
        assert_eq!(size.pixel_height, 360);
    }

    #[test]
    fn sub_cell_pixel_changes_are_not_reported_as_a_resize() {
        let a = terminal_size(720, 360, CELL);
        let b = terminal_size(724, 362, CELL);
        assert!(!size_changed(Some(a), b));
    }

    #[test]
    fn crossing_a_cell_boundary_is_reported_as_a_resize() {
        let a = terminal_size(720, 360, CELL);
        let b = terminal_size(729, 360, CELL);
        assert!(size_changed(Some(a), b));
    }

    #[test]
    fn the_first_size_is_always_a_resize() {
        let a = terminal_size(720, 360, CELL);
        assert!(size_changed(None, a));
    }
}
