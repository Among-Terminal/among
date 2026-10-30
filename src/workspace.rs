//! Workspace lifecycle: a list of named pane trees, one of them active.
//!
//! Every workspace keeps its terminals alive for as long as it exists, so this
//! module only ever holds model state; widget construction lives in
//! [`crate::pane`] and is driven from [`crate::app`].

use crate::tree::{self, Node, PaneId};

/// A flat palette used to give each workspace a colour.
///
/// The values are indices, not colours: which colour a workspace gets is model
/// behaviour that has to be testable, while how a colour is drawn belongs to the
/// stylesheet. Keeping them apart means the assignment can be verified without a
/// display.
///
/// Eight entries, which is enough for the workspaces a person keeps open at once
/// and small enough that the colours stay distinguishable.
pub const PALETTE_SIZE: usize = 8;

/// One pane tree plus the terminals attached to it.
///
/// `nodes` is the layout; the terminal map is keyed by [`PaneId`] so that the
/// tree can stay free of widget and process types.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// minted once from the globally unique birth-pane id, never recalculated
    /// from surviving leaves or list position. session snapshots do not save it.
    pub reference: u64,
    pub name: String,
    /// Index into the palette; stable for the workspace's lifetime.
    pub colour: usize,
    pub nodes: Node,
    pub focused: PaneId,
    /// Working directory of each pane, `None` while unknown.
    ///
    /// A plain shell does not report a directory until it changes one, so
    /// "unknown" is a normal state rather than an error.
    pub cwd: Vec<(PaneId, Option<String>)>,
    /// Pids of running processes, for the exit-confirmation message.
    pub running: usize,
    /// The pane expanded to fill the workspace, if any.
    ///
    /// Held as model state rather than a widget flag so that the layout is
    /// restored exactly as it was, including all its nested ratios.
    pub zoomed: Option<PaneId>,
}

impl Workspace {
    /// A workspace holding exactly one empty pane.
    ///
    /// `colour` is the palette index to use; the caller chooses it, because only
    /// the set of open workspaces knows which colours are already taken.
    pub fn new(name: String, first_pane: PaneId, colour: usize) -> Self {
        Self {
            reference: first_pane.0,
            name,
            colour: colour % PALETTE_SIZE,
            nodes: Node::single_pane(first_pane),
            focused: first_pane,
            cwd: vec![(first_pane, None)],
            running: 1,
            zoomed: None,
        }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        self.nodes.panes()
    }

    /// The directory recorded for a pane, or `None` while it is unknown.
    pub fn cwd_of(&self, pane: PaneId) -> Option<&str> {
        self.cwd
            .iter()
            .find(|(id, _)| *id == pane)
            .and_then(|(_, dir)| dir.as_deref())
    }

    pub fn set_cwd(&mut self, pane: PaneId, dir: Option<String>) {
        match self.cwd.iter_mut().find(|(id, _)| *id == pane) {
            Some(entry) => entry.1 = dir,
            None => self.cwd.push((pane, dir)),
        }
    }

    /// Split the focused pane, returning the new pane's id.
    ///
    /// The orientation comes from the focused pane's current pixel shape, so a
    /// wide pane divides side by side and a tall one into a stack.
    ///
    /// While a pane is expanded the window shows only that pane, so a split is
    /// refused instead of creating a pane the user cannot see and moving focus
    /// to it. The caller restores the layout first.
    pub fn split_focused(&mut self, width: i32, height: i32, new_pane: PaneId) -> Option<PaneId> {
        if self.zoomed.is_some() {
            return None;
        }
        let orientation = tree::auto_orientation(width, height);
        let focused = self.focused;
        self.nodes = self.nodes.split_pane(focused, orientation, new_pane)?;
        // The new terminal starts where the pane it was split from is, so both
        // halves of a split open in the same directory instead of the new one
        // falling back to the directory Among itself started in. The entry is a
        // copy of a string, not a link to the other pane's record: each shell
        // then updates only its own, and a later change in one half cannot move
        // the other.
        let inherited = self.cwd_of(focused).map(str::to_owned);
        self.cwd.push((new_pane, inherited));
        self.running += 1;
        // Focus moves to the newly created pane.
        self.focused = new_pane;
        Some(new_pane)
    }

    /// Adopt a pane that was detached from another workspace.
    ///
    /// The pane is placed after this workspace's last pane in tree order, so a
    /// drop adds it where a split of the last pane would put it. The directory
    /// travels with the pane rather than being inherited from the target: the
    /// terminal is the same terminal and already runs where it was.
    pub fn adopt(
        &mut self,
        pane: PaneId,
        cwd: Option<String>,
        orientation: tree::Orientation,
    ) -> bool {
        let Some(last) = self.nodes.panes().last().copied() else {
            return false;
        };
        let Some(nodes) = self
            .nodes
            .insert_pane(last, orientation, pane, tree::Placement::After)
        else {
            return false;
        };
        self.commit_adopted(nodes, pane, cwd);
        true
    }

    /// Commit an adoption whose tree was prepared by the caller.
    fn commit_adopted(&mut self, nodes: Node, pane: PaneId, cwd: Option<String>) {
        self.nodes = nodes;
        self.cwd.push((pane, cwd));
        self.running += 1;
        self.focused = pane;
        // A drop reveals the new split; leaving the old expansion in place
        // would hide the pane the user just moved here.
        self.zoomed = None;
    }

    /// Close a pane.
    ///
    /// Returns `None` when the pane was the workspace's last one, because the
    /// caller must then close the whole workspace instead of leaving it empty.
    pub fn close(&mut self, pane: PaneId) -> Option<()> {
        let next_focus = self.nodes.closing_focus(pane);
        // Closing the zoomed pane restores the layout rather than leaving a
        // zoom pointing at something that no longer exists.
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        self.nodes = self.nodes.close_pane(pane)?;
        self.cwd.retain(|(id, _)| *id != pane);
        self.running = self.running.saturating_sub(1);
        if self.focused == pane {
            self.focused = next_focus?;
        }
        Some(())
    }

    /// Rename the workspace.
    ///
    /// An empty or whitespace-only name is refused: an unnamed workspace is
    /// indistinguishable from another unnamed one in the list, which defeats the
    /// point of naming them at all. A refused rename leaves the name as it was.
    pub fn rename(&mut self, name: &str) -> bool {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return false;
        }
        self.name = trimmed.to_string();
        true
    }

    /// Expand the focused pane to fill the workspace, or undo that.
    ///
    /// Returns the pane that is now zoomed, or `None` when the layout is back
    /// to normal.
    pub fn toggle_zoom(&mut self) -> Option<PaneId> {
        let focused = self.focused;
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None => Some(focused),
        };
        self.zoomed
    }

    /// late header activations must never turn a restore into another expansion.
    pub fn restore_expanded(&mut self, pane: PaneId) -> bool {
        if self.zoomed != Some(pane) || !self.nodes.panes().contains(&pane) {
            return false;
        }
        self.zoomed = None;
        self.focused = pane;
        true
    }

    /// Whether this workspace is showing a single zoomed pane.
    pub fn is_zoomed(&self) -> bool {
        self.zoomed.is_some()
    }

    /// The panes actually on screen: one when zoomed, otherwise all of them.
    pub fn visible_panes(&self) -> Vec<PaneId> {
        match self.zoomed {
            Some(id) => vec![id],
            None => self.panes(),
        }
    }

    /// Move focus one step in `direction`, if a neighbour exists.
    ///
    /// An expanded pane is the only pane on screen and therefore the only focus
    /// target. A directional request must not select a sibling hidden behind the
    /// expansion: that would drop the visible pane's focus treatment and grab
    /// keyboard focus for a widget the user cannot see. Returning `false` says
    /// "focus did not move", which is what the caller already does for a
    /// direction with no neighbour.
    pub fn focus_direction(&mut self, direction: tree::Direction) -> bool {
        if self.zoomed.is_some() {
            return false;
        }
        match tree::neighbour_in_direction(&self.nodes, self.focused, direction) {
            Some(next) => {
                self.focused = next;
                true
            }
            None => false,
        }
    }
}

/// Hands out pane ids that are unique across every workspace.
///
/// Ids come from one global counter rather than per-workspace counters so that
/// plugin content and focus can name a pane without qualifying it by
/// workspace.
#[derive(Debug, Default)]
pub struct PaneAllocator {
    next: u64,
}

impl PaneAllocator {
    pub fn after(last: PaneId) -> Self {
        Self { next: last.0 }
    }

    pub fn try_next(&mut self) -> Option<PaneId> {
        self.next = self.next.checked_add(1)?;
        Some(PaneId(self.next))
    }

    #[cfg(test)]
    pub fn next(&mut self) -> PaneId {
        self.try_next().expect("test pane IDs are available")
    }
}

/// The ordered list of workspaces with one of them active.
#[derive(Debug, Clone)]
pub struct WorkspaceSet {
    workspaces: Vec<Workspace>,
    active: usize,
}

/// the same permutation must reach selection and index-keyed plugin summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceMove {
    from: usize,
    to: usize,
}

impl WorkspaceMove {
    pub fn remap(self, index: usize) -> usize {
        if index == self.from {
            self.to
        } else if self.from < index && index <= self.to {
            index - 1
        } else if self.to <= index && index < self.from {
            index + 1
        } else {
            index
        }
    }
}

/// Where a pane landed after moving to another workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspacePaneMove {
    /// Index of the destination workspace after the move.
    pub target: usize,
    /// Whether the source workspace was removed because its last pane left.
    pub source_closed: bool,
}

impl WorkspaceSet {
    pub fn from_restored(workspaces: Vec<Workspace>, active: usize) -> Option<Self> {
        if workspaces.is_empty() || active >= workspaces.len() {
            return None;
        }
        Some(Self { workspaces, active })
    }
    pub fn new(first: Workspace) -> Self {
        Self {
            workspaces: vec![first],
            active: 0,
        }
    }

    /// The palette index a new workspace should take.
    ///
    /// The colour used by the fewest open workspaces, ties broken by palette
    /// order. A rotating counter drifts out of step as workspaces come and go and
    /// can hand out a colour that is already on screen while an unused one sits
    /// idle; least-use keeps the workspaces visually distinct for as long as the
    /// palette has room, and once it does not it reuses the least represented
    /// colour rather than failing.
    pub fn next_colour(&self) -> usize {
        let mut counts = [0usize; PALETTE_SIZE];
        for workspace in &self.workspaces {
            counts[workspace.colour % PALETTE_SIZE] += 1;
        }
        let mut best = 0;
        for index in 1..PALETTE_SIZE {
            if counts[index] < counts[best] {
                best = index;
            }
        }
        best
    }

    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    /// Mutable access to the workspace at an index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Workspace> {
        self.workspaces.get_mut(index)
    }

    /// Mutable access to the whole list, for edits that address a workspace by
    /// its position.
    pub fn workspaces_mut(&mut self) -> &mut Vec<Workspace> {
        &mut self.workspaces
    }

    /// Apply a name typed into an in-place editor to the workspace at an index.
    ///
    /// `opened` is the name the editor was opened with. A workspace that no longer
    /// carries it is not the one being renamed: closing a workspace moves every row
    /// after it, so an index that named this workspace when the editor opened can
    /// name a different one by the time the name is applied.
    pub fn rename_at(&mut self, index: usize, opened: &str, typed: &str) -> bool {
        match self.workspaces.get_mut(index) {
            Some(workspace) if workspace.name == opened => workspace.rename(typed),
            _ => false,
        }
    }

    /// Append a workspace and make it active.
    pub fn push_active(&mut self, workspace: Workspace) -> usize {
        self.workspaces.push(workspace);
        self.active = self.workspaces.len() - 1;
        self.active
    }

    /// `gap` addresses the original sequence, before removal: zero is before the
    /// first entry and `len` is after the last. both gaps beside the source are
    /// no-ops, so a drop need not special-case upward and downward movement.
    pub fn reorder(&mut self, source: usize, gap: usize) -> Option<WorkspaceMove> {
        if source >= self.len() || gap > self.len() {
            return None;
        }
        let destination = if gap > source { gap - 1 } else { gap };
        if source == destination {
            return None;
        }
        let movement = WorkspaceMove {
            from: source,
            to: destination,
        };
        let workspace = self.workspaces.remove(source);
        self.workspaces.insert(destination, workspace);
        self.active = movement.remap(self.active);
        Some(movement)
    }

    /// Move a pane to another workspace, activating the destination.
    ///
    /// Workspaces are addressed by reference rather than by position so a drag
    /// cannot land in a different project after the sidebar changed underneath
    /// it. The pane keeps its identity and directory; the destination adopts it
    /// after its last pane in tree order. If the pane was the source's only
    /// pane, the source workspace is removed rather than left empty.
    pub fn move_pane_between(
        &mut self,
        source: u64,
        target: u64,
        pane: PaneId,
        orientation: tree::Orientation,
    ) -> Option<WorkspacePaneMove> {
        if source == target {
            return None;
        }
        let source_index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.reference == source)?;
        let target_index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.reference == target)?;
        if source_index == target_index {
            return None;
        }

        let source_panes = self.workspaces[source_index].panes();
        if !source_panes.contains(&pane) {
            return None;
        }
        let target_panes = self.workspaces[target_index].panes();
        if target_panes.contains(&pane) || target_panes.last().is_none() {
            return None;
        }
        let moved_cwd = self.workspaces[source_index]
            .cwd_of(pane)
            .map(str::to_owned);
        let source_closed = source_panes.len() == 1;
        // Adopt first. If the source mutation cannot complete, the target can
        // be restored from this snapshot without losing the pane.
        let target_before = self.workspaces[target_index].clone();
        if !self.workspaces[target_index].adopt(pane, moved_cwd, orientation) {
            return None;
        }

        let source_result = if source_closed {
            self.remove(source_index).map(|_| ())
        } else {
            self.workspaces[source_index].close(pane)
        };
        if source_result.is_none() {
            if let Some(target_index) = self
                .workspaces
                .iter()
                .position(|workspace| workspace.reference == target)
            {
                self.workspaces[target_index] = target_before;
            }
            return None;
        }

        // Removing the source may shift the target's position, so resolve it by
        // reference again instead of carrying an index across the removal.
        let target_index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.reference == target)?;
        self.active = target_index;
        Some(WorkspacePaneMove {
            target: target_index,
            source_closed,
        })
    }

    /// Activate a workspace by its position in the list.
    ///
    /// Out-of-range positions are ignored, so a digit beyond the workspace
    /// count does nothing.
    pub fn activate(&mut self, index: usize) -> bool {
        if index >= self.workspaces.len() {
            return false;
        }
        self.active = index;
        true
    }

    pub fn activate_next(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        self.active = (self.active + 1) % self.workspaces.len();
    }

    pub fn activate_previous(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        self.active = (self.active + self.workspaces.len() - 1) % self.workspaces.len();
    }

    /// Remove the workspace at `index`, keeping `active` pointing at a real one.
    ///
    /// Returns the removed workspace. Refuses to remove the last remaining
    /// workspace so the application can never end up with nothing to show.
    pub fn remove(&mut self, index: usize) -> Option<Workspace> {
        if self.workspaces.len() <= 1 || index >= self.workspaces.len() {
            return None;
        }
        let removed = self.workspaces.remove(index);
        if self.active >= self.workspaces.len() {
            self.active = self.workspaces.len() - 1;
        } else if self.active > index {
            self.active -= 1;
        }
        Some(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{Direction, Orientation};

    fn workspace() -> Workspace {
        Workspace::new("one".into(), PaneId(1), 0)
    }

    #[test]
    fn a_new_workspace_has_a_colour_from_the_palette() {
        let ws = workspace();
        assert!(ws.colour < PALETTE_SIZE);
    }

    #[test]
    fn workspace_references_survive_birth_pane_closure_reorder_and_index_reuse() {
        let mut set = three();
        let reference = set.workspaces()[1].reference;
        let born = set.workspaces()[1].focused;
        set.get_mut(1)
            .unwrap()
            .split_focused(600, 300, PaneId(40))
            .unwrap();
        set.get_mut(1).unwrap().close(born).unwrap();
        assert_eq!(set.workspaces()[1].reference, reference);
        set.workspaces_mut().swap(0, 1);
        assert_eq!(set.workspaces()[0].reference, reference);
        let closed = set.remove(0).unwrap();
        assert_eq!(closed.reference, reference);
        assert!(set
            .workspaces()
            .iter()
            .all(|workspace| workspace.reference != reference));
        let replacement = Workspace::new("replacement".into(), PaneId(41), 0);
        assert_ne!(replacement.reference, reference);
        set.push_active(replacement);
        assert!(set
            .workspaces()
            .iter()
            .all(|workspace| workspace.reference != reference));
    }

    #[test]
    fn adopting_a_pane_appends_it_after_the_last_pane_and_keeps_its_directory() {
        let mut target = workspace();
        target
            .split_focused(600, 300, PaneId(2))
            .expect("the focused pane is split");
        target.set_cwd(PaneId(2), Some("/target".into()));
        target.toggle_zoom();
        assert_eq!(target.zoomed, Some(PaneId(2)));

        assert!(target.adopt(PaneId(3), Some("/moved".into()), Orientation::Vertical));
        assert_eq!(target.panes(), vec![PaneId(1), PaneId(2), PaneId(3)]);
        assert_eq!(target.focused, PaneId(3));
        assert_eq!(target.cwd_of(PaneId(2)), Some("/target"));
        assert_eq!(target.cwd_of(PaneId(3)), Some("/moved"));
        assert_eq!(target.running, 3);
        assert_eq!(target.zoomed, None);
    }

    #[test]
    fn adopting_refuses_a_pane_already_in_the_workspace() {
        let mut target = workspace();
        assert!(!target.adopt(PaneId(1), None, Orientation::Vertical));
        assert_eq!(target.panes(), vec![PaneId(1)]);
        assert_eq!(target.focused, PaneId(1));
        assert_eq!(target.running, 1);
        assert_eq!(target.cwd.len(), 1);
    }

    #[test]
    fn adopting_a_pane_with_unknown_directory_keeps_the_entry_unknown() {
        let mut target = workspace();
        assert!(target.adopt(PaneId(2), None, Orientation::Horizontal));
        assert_eq!(target.cwd_of(PaneId(2)), None);
        assert!(target.cwd.iter().any(|(id, _)| *id == PaneId(2)));
    }

    #[test]
    fn moving_a_pane_between_workspaces_adopts_it_and_reclaims_the_source_space() {
        let mut set = three();
        set.get_mut(0)
            .expect("first workspace")
            .split_focused(600, 300, PaneId(4))
            .expect("source split");
        set.get_mut(1)
            .expect("second workspace")
            .split_focused(600, 300, PaneId(5))
            .expect("target split");
        set.get_mut(0)
            .expect("first workspace")
            .set_cwd(PaneId(4), Some("/moved".into()));
        set.get_mut(1)
            .expect("second workspace")
            .set_cwd(PaneId(5), Some("/target".into()));
        set.get_mut(0).expect("first workspace").toggle_zoom();
        set.get_mut(1).expect("second workspace").toggle_zoom();
        set.activate(0);

        let source = set.workspaces()[0].reference;
        let target = set.workspaces()[1].reference;
        let before_names = names(&set);
        let before_colours: Vec<_> = set
            .workspaces()
            .iter()
            .map(|workspace| workspace.colour)
            .collect();

        let movement = set
            .move_pane_between(source, target, PaneId(4), Orientation::Vertical)
            .expect("the pane moves");
        assert_eq!(
            movement,
            WorkspacePaneMove {
                target: 1,
                source_closed: false
            }
        );
        assert_eq!(set.active_index(), 1);
        assert_eq!(set.workspaces()[0].panes(), vec![PaneId(1)]);
        assert_eq!(set.workspaces()[0].focused, PaneId(1));
        assert_eq!(set.workspaces()[0].zoomed, None);
        assert_eq!(
            set.workspaces()[1].panes(),
            vec![PaneId(2), PaneId(5), PaneId(4)]
        );
        assert_eq!(set.workspaces()[1].focused, PaneId(4));
        assert_eq!(set.workspaces()[1].zoomed, None);
        assert_eq!(set.workspaces()[1].cwd_of(PaneId(4)), Some("/moved"));
        assert_eq!(set.workspaces()[1].cwd_of(PaneId(5)), Some("/target"));
        assert_eq!(set.workspaces()[0].running, 1);
        assert_eq!(set.workspaces()[1].running, 3);
        assert_eq!(names(&set), before_names);
        assert_eq!(
            set.workspaces()
                .iter()
                .map(|workspace| workspace.colour)
                .collect::<Vec<_>>(),
            before_colours
        );
    }

    #[test]
    fn moving_an_unfocused_pane_leaves_the_source_focus_alone() {
        let mut set = three();
        set.get_mut(0)
            .expect("first workspace")
            .split_focused(600, 300, PaneId(4))
            .expect("source split");
        // The new pane is focused; move the older pane instead.
        let source = set.workspaces()[0].reference;
        let target = set.workspaces()[1].reference;
        set.move_pane_between(source, target, PaneId(1), Orientation::Vertical)
            .expect("the pane moves");
        assert_eq!(set.workspaces()[0].focused, PaneId(4));
        assert_eq!(set.workspaces()[0].panes(), vec![PaneId(4)]);
        assert_eq!(set.workspaces()[1].panes(), vec![PaneId(2), PaneId(1)]);
    }

    #[test]
    fn moving_a_workspaces_last_pane_removes_the_source_and_activates_the_target() {
        let mut set = three();
        set.activate(1);
        let source = set.workspaces()[1].reference;
        let target = set.workspaces()[2].reference;
        let movement = set
            .move_pane_between(source, target, PaneId(2), Orientation::Vertical)
            .expect("the pane moves");
        assert_eq!(
            movement,
            WorkspacePaneMove {
                target: 1,
                source_closed: true
            }
        );
        assert_eq!(set.len(), 2);
        assert_eq!(names(&set), ["first", "third"]);
        assert_eq!(set.active_index(), 1);
        assert_eq!(set.workspaces()[1].panes(), vec![PaneId(3), PaneId(2)]);
        assert_eq!(set.workspaces()[1].focused, PaneId(2));
        assert_eq!(set.workspaces()[0].panes(), vec![PaneId(1)]);
        assert_eq!(set.workspaces()[0].colour, 0);
    }

    #[test]
    fn moving_a_pane_into_an_earlier_workspace_keeps_its_index() {
        let mut set = three();
        set.activate(2);
        let source = set.workspaces()[2].reference;
        let target = set.workspaces()[0].reference;
        let movement = set
            .move_pane_between(source, target, PaneId(3), Orientation::Horizontal)
            .expect("the pane moves");
        assert_eq!(movement.target, 0);
        assert!(movement.source_closed);
        assert_eq!(set.active_index(), 0);
        assert_eq!(names(&set), ["first", "second"]);
        assert_eq!(set.workspaces()[0].panes(), vec![PaneId(1), PaneId(3)]);
        assert_eq!(set.workspaces()[1].panes(), vec![PaneId(2)]);
    }

    #[test]
    fn invalid_cross_workspace_moves_leave_the_set_untouched() {
        let mut set = three();
        let first = set.workspaces()[0].reference;
        let second = set.workspaces()[1].reference;
        let before = format!("{set:?}");
        assert!(set
            .move_pane_between(first, first, PaneId(1), Orientation::Vertical)
            .is_none());
        assert!(set
            .move_pane_between(first, 999, PaneId(1), Orientation::Vertical)
            .is_none());
        assert!(set
            .move_pane_between(first, second, PaneId(999), Orientation::Vertical)
            .is_none());
        assert!(set
            .move_pane_between(first, second, PaneId(2), Orientation::Vertical)
            .is_none());
        assert_eq!(format!("{set:?}"), before);
    }

    /// A set of three workspaces named `first`, `second`, `third`.
    fn three() -> WorkspaceSet {
        let mut alloc = PaneAllocator::default();
        let mut set = WorkspaceSet::new(Workspace::new("first".into(), alloc.next(), 0));
        for name in ["second", "third"] {
            let colour = set.next_colour();
            set.push_active(Workspace::new(name.into(), alloc.next(), colour));
        }
        set
    }

    fn names(set: &WorkspaceSet) -> Vec<String> {
        set.workspaces()
            .iter()
            .map(|workspace| workspace.name.clone())
            .collect()
    }

    #[test]
    fn reorder_inserts_in_both_directions_rather_than_swapping() {
        let mut set = three();
        let moved = set.reorder(2, 0).unwrap();
        assert_eq!(names(&set), ["third", "first", "second"]);
        assert_eq!(
            (0..3).map(|index| moved.remap(index)).collect::<Vec<_>>(),
            [1, 2, 0]
        );
        assert_eq!(set.active_index(), 0);
        let moved = set.reorder(0, 3).unwrap();
        assert_eq!(names(&set), ["first", "second", "third"]);
        assert_eq!(
            (0..3).map(|index| moved.remap(index)).collect::<Vec<_>>(),
            [2, 0, 1]
        );
        assert_eq!(set.active_index(), 2);
    }

    #[test]
    fn every_reorder_keeps_the_active_workspace_and_other_relative_positions() {
        for active in 0..3 {
            for source in 0..3 {
                for gap in 0..=3 {
                    let mut set = three();
                    set.activate(active);
                    let active_reference = set.active().reference;
                    let before: Vec<_> = set.workspaces().iter().map(|w| w.reference).collect();
                    let movement = set.reorder(source, gap);
                    assert_eq!(set.active().reference, active_reference);
                    let unchanged: Vec<_> = set
                        .workspaces()
                        .iter()
                        .filter(|w| w.reference != before[source])
                        .map(|w| w.reference)
                        .collect();
                    assert_eq!(
                        unchanged,
                        before
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| *index != source)
                            .map(|(_, id)| *id)
                            .collect::<Vec<_>>()
                    );
                    if let Some(movement) = movement {
                        for (old, reference) in before.iter().enumerate() {
                            assert_eq!(set.workspaces()[movement.remap(old)].reference, *reference);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_and_adjacent_reorder_gaps_leave_the_model_unchanged() {
        let mut set = three();
        let before = format!("{set:?}");
        for (source, gap) in [
            (0, 0),
            (0, 1),
            (1, 1),
            (1, 2),
            (2, 2),
            (2, 3),
            (3, 0),
            (usize::MAX, 1),
            (0, 4),
            (1, usize::MAX),
        ] {
            assert_eq!(set.reorder(source, gap), None);
            assert_eq!(format!("{set:?}"), before);
        }
        let mut only = WorkspaceSet::new(workspace());
        assert_eq!(only.reorder(0, 0), None);
        assert_eq!(only.reorder(0, 1), None);
        assert_eq!(only.len(), 1);
    }

    #[test]
    fn reordering_equal_looking_entries_preserves_every_workspace_field() {
        let mut set = three();
        for (index, workspace) in set.workspaces_mut().iter_mut().enumerate() {
            workspace.name = "same project".into();
            workspace.colour = 0;
            workspace.set_cwd(workspace.focused, Some(format!("/project/{index}")));
            workspace.split_focused(600, 300, PaneId(10 + index as u64));
            workspace.split_focused(300, 600, PaneId(20 + index as u64));
            workspace.toggle_zoom();
        }
        let before: Vec<_> = set
            .workspaces()
            .iter()
            .map(|workspace| (workspace.reference, format!("{workspace:?}")))
            .collect();
        for (source, gap) in [(0, 3), (2, 0), (1, 3), (2, 1)] {
            set.reorder(source, gap).unwrap();
            for workspace in set.workspaces() {
                let original = before
                    .iter()
                    .find(|(reference, _)| *reference == workspace.reference)
                    .unwrap();
                assert_eq!(format!("{workspace:?}"), original.1);
            }
        }
    }

    #[test]
    fn navigation_append_and_removal_follow_the_reordered_sequence() {
        let mut set = three();
        set.reorder(2, 0).unwrap();
        assert!(set.activate(1));
        assert_eq!(set.active().name, "first");
        set.activate_next();
        assert_eq!(set.active().name, "second");
        set.activate_next();
        assert_eq!(set.active().name, "third");
        set.activate_previous();
        assert_eq!(set.active().name, "second");
        set.push_active(Workspace::new("fourth".into(), PaneId(50), 3));
        assert_eq!(names(&set), ["third", "first", "second", "fourth"]);
        assert_eq!(set.active().name, "fourth");
        set.remove(1).unwrap();
        assert_eq!(names(&set), ["third", "second", "fourth"]);
        assert_eq!(set.active().name, "fourth");
    }

    #[test]
    fn a_rename_reaches_the_workspace_it_was_opened_for() {
        let mut set = three();
        assert!(set.rename_at(1, "second", "renamed"));
        assert_eq!(names(&set), ["first", "renamed", "third"]);
    }

    #[test]
    fn a_rename_is_refused_when_the_workspace_at_that_index_is_another_one() {
        let mut set = three();
        // An editor opened on `second` at index 1, while `first` is closed: the
        // editor's index now names `third`, and the typed name must not land there.
        set.remove(0);
        assert!(!set.rename_at(1, "second", "renamed"));
        assert_eq!(names(&set), ["second", "third"]);
    }

    #[test]
    fn a_rename_is_refused_when_there_is_no_workspace_left_at_that_index() {
        let mut set = three();
        assert!(!set.rename_at(7, "second", "renamed"));
        assert_eq!(names(&set), ["first", "second", "third"]);
    }

    #[test]
    fn a_rename_cannot_be_applied_twice() {
        let mut set = three();
        assert!(set.rename_at(1, "second", "renamed"));
        // The editor is gone; anything that still holds the name it was opened with
        // is stale, and must not rename the workspace that took this one's place.
        assert!(!set.rename_at(1, "second", "again"));
        assert_eq!(names(&set), ["first", "renamed", "third"]);
    }

    #[test]
    fn workspaces_up_to_the_palette_size_all_differ() {
        let mut alloc = PaneAllocator::default();
        let mut set = WorkspaceSet::new(Workspace::new("first".into(), alloc.next(), 0));
        for index in 1..PALETTE_SIZE {
            let colour = set.next_colour();
            set.push_active(Workspace::new(format!("w{index}"), alloc.next(), colour));
        }
        let mut colours: Vec<usize> = set.workspaces().iter().map(|w| w.colour).collect();
        colours.sort();
        colours.dedup();
        assert_eq!(colours.len(), PALETTE_SIZE, "colours must all differ");
    }

    #[test]
    fn past_the_palette_size_a_workspace_still_gets_a_colour() {
        let mut alloc = PaneAllocator::default();
        let mut set = WorkspaceSet::new(Workspace::new("first".into(), alloc.next(), 0));
        for index in 1..PALETTE_SIZE + 4 {
            let colour = set.next_colour();
            set.push_active(Workspace::new(format!("w{index}"), alloc.next(), colour));
        }
        // Every workspace is coloured, and none is left without one.
        assert!(set.workspaces().iter().all(|w| w.colour < PALETTE_SIZE));
    }

    #[test]
    fn a_reused_colour_is_the_least_represented() {
        let mut alloc = PaneAllocator::default();
        let mut set = WorkspaceSet::new(Workspace::new("first".into(), alloc.next(), 0));
        for index in 1..PALETTE_SIZE + 8 {
            let colour = set.next_colour();
            set.push_active(Workspace::new(format!("w{index}"), alloc.next(), colour));
        }
        let mut counts = [0usize; PALETTE_SIZE];
        for workspace in set.workspaces() {
            counts[workspace.colour] += 1;
        }
        let spread = counts.iter().max().unwrap() - counts.iter().min().unwrap();
        // Least-use keeps the counts within one of each other.
        assert!(spread <= 1, "colour usage is uneven: {counts:?}");
    }

    #[test]
    fn closing_a_workspace_frees_its_colour() {
        let mut alloc = PaneAllocator::default();
        let mut set = WorkspaceSet::new(Workspace::new("first".into(), alloc.next(), 0));
        let second = set.next_colour();
        set.push_active(Workspace::new("second".into(), alloc.next(), second));
        // With one workspace per colour used, the freed colour becomes the least
        // represented and is offered again.
        set.remove(1).unwrap();
        assert_eq!(set.next_colour(), second);
    }

    #[test]
    fn renaming_leaves_the_colour_alone() {
        let mut ws = workspace();
        let before = ws.colour;
        assert!(ws.rename("renamed"));
        assert_eq!(ws.colour, before);
    }

    #[test]
    fn renaming_takes_a_valid_name() {
        let mut ws = workspace();
        assert!(ws.rename("  my project  "));
        assert_eq!(ws.name, "my project");
    }

    #[test]
    fn renaming_refuses_an_empty_or_blank_name() {
        let mut ws = workspace();
        assert!(!ws.rename(""));
        assert!(!ws.rename("   "));
        assert_eq!(ws.name, "one", "a refused rename must keep the old name");
    }

    #[test]
    fn new_workspace_has_one_pane_with_unknown_cwd() {
        let ws = workspace();
        assert_eq!(ws.panes(), vec![PaneId(1)]);
        assert_eq!(ws.focused, PaneId(1));
        assert_eq!(ws.cwd_of(PaneId(1)), None);
    }

    #[test]
    fn split_uses_pane_shape_and_moves_focus_to_the_new_pane() {
        let mut ws = workspace();
        let new = ws.split_focused(400, 200, PaneId(2)).unwrap();
        assert_eq!(new, PaneId(2));
        assert_eq!(ws.focused, PaneId(2));
        assert!(matches!(
            ws.nodes,
            Node::Split {
                orientation: Orientation::Vertical,
                ..
            }
        ));
    }

    #[test]
    fn split_of_a_tall_pane_stacks() {
        let mut ws = workspace();
        ws.split_focused(200, 400, PaneId(2));
        assert!(matches!(
            ws.nodes,
            Node::Split {
                orientation: Orientation::Horizontal,
                ..
            }
        ));
    }

    #[test]
    fn cwd_is_tracked_per_pane_and_cleared_on_close() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.set_cwd(PaneId(2), Some("/tmp".into()));
        assert_eq!(ws.cwd_of(PaneId(2)), Some("/tmp"));
        assert_eq!(ws.cwd_of(PaneId(1)), None);

        ws.close(PaneId(2)).unwrap();
        assert_eq!(ws.cwd_of(PaneId(2)), None);
    }

    #[test]
    fn a_split_hands_the_origin_panes_directory_to_the_new_pane() {
        let mut ws = workspace();
        ws.set_cwd(PaneId(1), Some("/projects/among".into()));
        let new = ws.split_focused(400, 200, PaneId(2)).unwrap();
        assert_eq!(new, PaneId(2));
        assert_eq!(
            ws.cwd_of(PaneId(2)),
            Some("/projects/among"),
            "the new terminal is started in the directory of the pane it was split from"
        );
    }

    #[test]
    fn a_split_from_a_pane_without_a_directory_records_none() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        assert_eq!(
            ws.cwd_of(PaneId(2)),
            None,
            "an unknown origin leaves the new pane unknown, so the start directory still applies"
        );
    }

    #[test]
    fn a_split_copies_the_directory_rather_than_linking_it() {
        let mut ws = workspace();
        ws.set_cwd(PaneId(1), Some("/one".into()));
        ws.split_focused(400, 200, PaneId(2));
        assert_eq!(ws.focused, PaneId(2), "focus follows the split");

        ws.set_cwd(PaneId(1), Some("/two".into()));
        assert_eq!(ws.cwd_of(PaneId(1)), Some("/two"));
        assert_eq!(
            ws.cwd_of(PaneId(2)),
            Some("/one"),
            "a later change in the origin pane must not reach the pane split from it"
        );
    }

    #[test]
    fn closing_moves_focus_to_a_pane_that_still_exists() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.split_focused(400, 400, PaneId(3));
        assert_eq!(ws.focused, PaneId(3));

        ws.close(PaneId(3)).unwrap();
        assert!(ws.panes().contains(&ws.focused));
        assert_eq!(ws.focused, PaneId(2));
    }

    #[test]
    fn nested_closes_focus_the_sibling_not_the_first_workspace_pane() {
        for (width, height) in [(400, 200), (200, 400)] {
            for closed in [PaneId(2), PaneId(3)] {
                let mut ws = workspace();
                ws.split_focused(width, height, PaneId(2));
                ws.split_focused(height, width, PaneId(3));
                ws.focused = closed;
                ws.close(closed).unwrap();
                assert_eq!(
                    ws.focused,
                    if closed == PaneId(2) {
                        PaneId(3)
                    } else {
                        PaneId(2)
                    }
                );
            }
        }
    }

    #[test]
    fn closing_unfocused_leaf_preserves_focus_and_subtree_survivor_is_local() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.split_focused(200, 400, PaneId(3));
        ws.focused = PaneId(1);
        ws.close(PaneId(3)).unwrap();
        assert_eq!(ws.focused, PaneId(1));

        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.split_focused(200, 400, PaneId(3));
        ws.focused = PaneId(1);
        ws.close(PaneId(1)).unwrap();
        assert_eq!(ws.focused, PaneId(2));
    }

    #[test]
    fn closing_the_only_pane_is_refused() {
        let mut ws = workspace();
        assert!(ws.close(PaneId(1)).is_none());
        assert_eq!(ws.panes(), vec![PaneId(1)]);
    }

    #[test]
    fn zoom_shows_only_the_focused_pane_and_restores_afterwards() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.focused = PaneId(2);
        assert_eq!(ws.visible_panes(), vec![PaneId(1), PaneId(2)]);

        assert_eq!(ws.toggle_zoom(), Some(PaneId(2)));
        assert_eq!(ws.visible_panes(), vec![PaneId(2)]);
        assert!(ws.is_zoomed());

        // Toggling back restores every pane, because the tree was untouched.
        assert_eq!(ws.toggle_zoom(), None);
        assert_eq!(ws.visible_panes(), vec![PaneId(1), PaneId(2)]);
        assert!(!ws.is_zoomed());
    }

    #[test]
    fn zoom_expands_whichever_pane_is_focused_at_the_time() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));

        // Zooming pane 2, then un-zooming, then zooming pane 1 gives pane 1.
        ws.focused = PaneId(2);
        assert_eq!(ws.toggle_zoom(), Some(PaneId(2)));
        assert_eq!(ws.toggle_zoom(), None);
        ws.focused = PaneId(1);
        assert_eq!(ws.toggle_zoom(), Some(PaneId(1)));
        assert_eq!(ws.visible_panes(), vec![PaneId(1)]);
    }

    #[test]
    fn restore_only_preserves_nested_layout_and_rejects_wrong_or_repeated_targets() {
        let mut ws = workspace();
        ws.split_focused(600, 300, PaneId(2));
        ws.split_focused(250, 500, PaneId(3));
        let layout = ws.nodes.clone();
        let directories = ws.cwd.clone();
        assert!(!ws.restore_expanded(PaneId(3)));
        ws.toggle_zoom();
        assert!(!ws.restore_expanded(PaneId(2)));
        assert_eq!(ws.zoomed, Some(PaneId(3)));
        assert!(ws.restore_expanded(PaneId(3)));
        assert_eq!(ws.nodes, layout);
        assert_eq!(ws.cwd, directories);
        assert_eq!(ws.focused, PaneId(3));
        assert_eq!(ws.running, 3);
        assert!(!ws.restore_expanded(PaneId(3)));
        assert!(!ws.is_zoomed());
    }

    #[test]
    fn closed_or_other_workspace_targets_cannot_restore_a_different_pane() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.toggle_zoom();
        ws.close(PaneId(2)).unwrap();
        assert!(!ws.restore_expanded(PaneId(2)));
        ws.toggle_zoom();
        assert!(!ws.restore_expanded(PaneId(20)));
        assert_eq!(ws.zoomed, Some(PaneId(1)));
    }

    #[test]
    fn closing_the_zoomed_pane_leaves_the_layout_unzoomed() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.toggle_zoom();
        assert!(ws.is_zoomed());
        ws.close(PaneId(2)).unwrap();
        assert!(!ws.is_zoomed());
        assert_eq!(ws.visible_panes(), vec![PaneId(1)]);
    }

    #[test]
    fn focus_moves_to_a_real_neighbour() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.focused = PaneId(1);
        assert!(ws.focus_direction(Direction::Right));
        assert_eq!(ws.focused, PaneId(2));
    }

    #[test]
    fn focus_without_a_neighbour_keeps_the_current_pane() {
        let mut ws = workspace();
        assert!(!ws.focus_direction(Direction::Right));
        assert_eq!(ws.focused, PaneId(1));
    }

    #[test]
    fn directional_focus_is_held_by_the_expanded_pane_in_every_direction() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.focused = PaneId(2);
        assert_eq!(ws.toggle_zoom(), Some(PaneId(2)));
        let layout = ws.nodes.clone();

        // Left and Right do have neighbours in the stored tree, so a suppression
        // that only covered missing neighbours would fail here.
        for direction in [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ] {
            assert!(!ws.focus_direction(direction));
            assert_eq!(ws.focused, PaneId(2));
            assert_eq!(ws.zoomed, Some(PaneId(2)));
            assert_eq!(ws.visible_panes(), vec![PaneId(2)]);
            assert_eq!(ws.nodes, layout);
        }
    }

    #[test]
    fn directional_focus_resumes_after_restore() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.focused = PaneId(2);
        ws.toggle_zoom();
        assert!(!ws.focus_direction(Direction::Left));
        assert_eq!(ws.focused, PaneId(2));

        assert_eq!(ws.toggle_zoom(), None);
        assert!(ws.focus_direction(Direction::Left));
        assert_eq!(ws.focused, PaneId(1));
    }

    #[test]
    fn a_hidden_pane_exiting_leaves_the_expanded_pane_focused() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.focused = PaneId(2);
        ws.toggle_zoom();

        // Pane 1 is behind the expansion; its process ending must not move
        // focus off the visible pane or end the expansion.
        assert_eq!(ws.close(PaneId(1)), Some(()));
        assert_eq!(ws.zoomed, Some(PaneId(2)));
        assert_eq!(ws.focused, PaneId(2));
        assert_eq!(ws.visible_panes(), vec![PaneId(2)]);
    }

    #[test]
    fn split_is_refused_while_a_pane_is_expanded() {
        let mut ws = workspace();
        ws.split_focused(400, 200, PaneId(2));
        ws.focused = PaneId(2);
        ws.toggle_zoom();
        let layout = ws.nodes.clone();
        let directories = ws.cwd.clone();

        assert!(ws.split_focused(400, 200, PaneId(3)).is_none());
        assert_eq!(ws.panes(), vec![PaneId(1), PaneId(2)]);
        assert_eq!(ws.nodes, layout);
        assert_eq!(ws.cwd, directories);
        assert_eq!(ws.running, 2);
        assert_eq!(ws.focused, PaneId(2));
        assert_eq!(ws.zoomed, Some(PaneId(2)));

        // Restoring makes the same request possible again.
        assert_eq!(ws.toggle_zoom(), None);
        assert_eq!(ws.split_focused(400, 200, PaneId(3)), Some(PaneId(3)));
        assert_eq!(ws.focused, PaneId(3));
        assert!(ws.panes().contains(&PaneId(3)));
    }

    fn set_of_three() -> WorkspaceSet {
        let mut set = WorkspaceSet::new(Workspace::new("one".into(), PaneId(1), 0));
        let mut alloc = PaneAllocator::default();
        for name in ["two", "three"] {
            let id = alloc.next();
            let colour = set.next_colour();
            set.push_active(Workspace::new(name.into(), id, colour));
        }
        set
    }

    #[test]
    fn new_set_starts_with_one_active_workspace() {
        let set = WorkspaceSet::new(Workspace::new("one".into(), PaneId(1), 0));
        assert_eq!(set.len(), 1);
        assert_eq!(set.active_index(), 0);
    }

    #[test]
    fn pushing_makes_the_new_workspace_active() {
        let set = set_of_three();
        assert_eq!(set.len(), 3);
        assert_eq!(set.active_index(), 2);
        assert_eq!(set.active().name, "three");
    }

    #[test]
    fn activate_ignores_out_of_range_positions() {
        let mut set = set_of_three();
        assert!(set.activate(1));
        assert_eq!(set.active_index(), 1);
        assert!(!set.activate(3));
        assert!(!set.activate(99));
        assert_eq!(set.active_index(), 1);
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let mut set = set_of_three();
        assert_eq!(set.active_index(), 2);
        // Wrapping forward from the last workspace returns to the first.
        set.activate_next();
        assert_eq!(set.active_index(), 0);
        // Wrapping backward from the first returns to the last.
        set.activate_previous();
        assert_eq!(set.active_index(), 2);
        // A plain step is not a wrap.
        set.activate_previous();
        assert_eq!(set.active_index(), 1);
    }

    #[test]
    fn removing_the_active_workspace_keeps_a_valid_active_index() {
        let mut set = set_of_three();
        set.activate(2);
        set.remove(2).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.active_index() < set.len());
    }

    #[test]
    fn removing_an_earlier_workspace_shifts_the_active_index_back() {
        let mut set = set_of_three();
        set.activate(2);
        set.remove(0).unwrap();
        assert_eq!(set.active_index(), 1);
        assert_eq!(set.active().name, "three");
    }

    #[test]
    fn removing_the_last_workspace_is_refused() {
        let mut set = WorkspaceSet::new(Workspace::new("only".into(), PaneId(1), 0));
        assert!(set.remove(0).is_none());
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn allocator_hands_out_unique_ids() {
        let mut alloc = PaneAllocator::default();
        let ids: Vec<PaneId> = (0..5).map(|_| alloc.next()).collect();
        for pair in ids.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
    }
}
