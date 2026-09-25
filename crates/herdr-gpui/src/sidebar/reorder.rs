//! Reordering workspaces by dragging their rows. Holding a press on a row, or
//! moving it a few pixels, lifts the row; releasing it over a gap sends
//! `workspace.move_block`. The daemon owns the order, so the list changes when
//! the next snapshot arrives, the same for every attached client.
//!
//! A top-level row carries its whole worktree group, collapsed children
//! included, and lands between other top-level rows. A linked worktree only
//! moves among its own siblings: the group is decided by its repository, not
//! by where it is dropped.

use crate::HerdrWindow;
use gpui::{Context, Pixels, Point, Task};
use herdr_client::{Method, protocol::ClientShellWorkspace};
use std::time::Duration;

use super::workspaces::workspace_entries;

/// How long a press rests before its row lifts.
pub(crate) const LIFT_DELAY: Duration = Duration::from_millis(250);
/// How far a press may travel before it lifts without waiting.
const LIFT_DISTANCE: f32 = 4.;

/// A press on a workspace row that may become a reorder.
pub(crate) struct WorkspaceDrag {
    boot: String,
    pub(super) workspace: String,
    origin: Point<Pixels>,
    pub(super) pointer: Point<Pixels>,
    pub(super) lifted: bool,
    /// The gap under the pointer, `None` over the carried unit's own place.
    pub(super) target: Option<Target>,
    /// Lifts the row once the press has rested; dropping the drag cancels it.
    _lift: Task<()>,
}

impl WorkspaceDrag {
    /// How far the lifted row has followed the pointer.
    pub(super) fn offset(&self) -> Pixels {
        self.pointer.y - self.origin.y
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) slot: usize,
    request: MoveBlock,
}

#[cfg(test)]
impl Target {
    pub(super) fn params(&self) -> serde_json::Value {
        self.request.params()
    }
}

/// A `workspace.move_block` request: `workspace_ids` land, in that order,
/// before `before`, or at the end of the list without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MoveBlock {
    workspace_ids: Vec<String>,
    before: Option<String>,
}

impl MoveBlock {
    fn params(&self) -> serde_json::Value {
        serde_json::json!({
            "workspace_ids": self.workspace_ids,
            "before_workspace_id": self.before,
        })
    }
}

/// The units a drag reorders, in display order, and the one it carries. Each
/// unit lists workspace indices in the order the sidebar shows them.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    units: Vec<Vec<usize>>,
    dragged: usize,
    /// Whether the units are one group's children rather than top-level rows.
    children: bool,
}

impl Plan {
    /// `None` when the pressed row has nowhere else to go.
    pub(super) fn new(workspaces: &[ClientShellWorkspace], workspace: &str) -> Option<Self> {
        let pressed = workspaces
            .iter()
            .position(|w| w.workspace_id == workspace)?;
        let mut blocks: Vec<Vec<(usize, bool)>> = Vec::new();
        for (index, child) in workspace_entries(workspaces) {
            match blocks.last_mut() {
                Some(block) if child => block.push((index, child)),
                _ => blocks.push(vec![(index, child)]),
            }
        }
        let block = blocks
            .iter()
            .position(|block| block.iter().any(|&(index, _)| index == pressed))?;
        let plan = if blocks[block][0].0 == pressed {
            Self {
                units: blocks
                    .iter()
                    .map(|block| block.iter().map(|&(index, _)| index).collect())
                    .collect(),
                dragged: block,
                children: false,
            }
        } else {
            let children: Vec<_> = blocks[block][1..].iter().map(|&(i, _)| vec![i]).collect();
            Self {
                dragged: children.iter().position(|unit| unit[0] == pressed)?,
                units: children,
                children: true,
            }
        };
        (plan.units.len() > 1).then_some(plan)
    }

    pub(super) fn len(&self) -> usize {
        self.units.len()
    }

    pub(super) fn dragged(&self) -> usize {
        self.dragged
    }

    /// The unit a workspace belongs to, if this drag reorders it.
    pub(super) fn unit_of(&self, index: usize) -> Option<usize> {
        self.units.iter().position(|unit| unit.contains(&index))
    }

    /// The move that dropping into `slot`, the gap before unit `slot`, makes.
    /// `None` for the two gaps around the carried unit, which move nothing.
    pub(super) fn request(
        &self,
        workspaces: &[ClientShellWorkspace],
        slot: usize,
    ) -> Option<MoveBlock> {
        if slot == self.dragged || slot == self.dragged + 1 || slot > self.units.len() {
            return None;
        }
        let moved = &self.units[self.dragged];
        let id = |index: usize| workspaces[index].workspace_id.clone();
        // A group shows where its earliest member sits, so landing before a
        // unit means landing before that member.
        let before = match self.units.get(slot) {
            Some(unit) => unit.iter().min().copied(),
            // Past the last sibling, a child still has to stay ahead of what
            // follows its group; a top-level unit simply goes last.
            None if self.children => {
                let last = self
                    .units
                    .iter()
                    .enumerate()
                    .filter(|&(unit, _)| unit != self.dragged)
                    .flat_map(|(_, unit)| unit.iter().copied())
                    .max()?;
                (last + 1..workspaces.len()).find(|index| !moved.contains(index))
            }
            None => None,
        };
        Some(MoveBlock {
            workspace_ids: moved.iter().map(|&index| id(index)).collect(),
            before: before.map(id),
        })
    }
}

/// The gap a pointer at `y` points to, given the vertical centers of every
/// unit except the carried one: before the first unit centered below it.
pub(super) fn slot_at(
    centers: impl IntoIterator<Item = (usize, f32)>,
    y: f32,
    len: usize,
) -> usize {
    centers
        .into_iter()
        .find(|&(_, center)| y < center)
        .map_or(len, |(unit, _)| unit)
}

/// Where the drop line goes: the top of the first row of unit `slot`, or the
/// bottom of the last unit's last row. `units` holds each visible row's unit.
pub(super) fn indicator(units: &[Option<usize>], slot: usize, len: usize) -> Option<(usize, Edge)> {
    if slot < len {
        units
            .iter()
            .position(|&unit| unit == Some(slot))
            .map(|row| (row, Edge::Top))
    } else {
        units
            .iter()
            .rposition(|&unit| unit == Some(len - 1))
            .map(|row| (row, Edge::Bottom))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edge {
    Top,
    Bottom,
}

impl HerdrWindow {
    /// Arms a reorder for a left press on a row of the selected endpoint.
    pub(super) fn press_workspace(
        &mut self,
        workspace: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = &self.live.snapshot else {
            return;
        };
        if self.menu.page.is_some()
            || !self.live.status.is_connected()
            || Plan::new(&snapshot.workspaces, workspace).is_none()
        {
            return;
        }
        let lift = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LIFT_DELAY).await;
            let _ = this.update(cx, |this, cx| this.lift_workspace(cx));
        });
        self.workspace_drag = Some(WorkspaceDrag {
            boot: snapshot.boot_id.clone(),
            workspace: workspace.to_owned(),
            origin: position,
            pointer: position,
            lifted: false,
            target: None,
            _lift: lift,
        });
    }

    fn lift_workspace(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = &mut self.workspace_drag
            && !drag.lifted
        {
            drag.lifted = true;
            // The resting pointer is carrying a row, not asking for its menu.
            self.hover = None;
            cx.notify();
        }
    }

    /// Follows the pointer. `target` resolves a lifted drag's gap from the
    /// geometry the sidebar last laid out. Returns whether the move belongs to
    /// the drag, so the terminal and hover never see it.
    pub(super) fn move_workspace_drag(
        &mut self,
        position: Point<Pixels>,
        held: bool,
        target: impl FnOnce(Point<Pixels>) -> Option<Target>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(drag) = &mut self.workspace_drag else {
            return false;
        };
        // A release outside the window never reaches the sidebar.
        if !held {
            let lifted = drag.lifted;
            self.workspace_drag = None;
            cx.notify();
            return lifted;
        }
        drag.pointer = position;
        let moved = position - drag.origin;
        if !drag.lifted && f32::from(moved.x.abs().max(moved.y.abs())) > LIFT_DISTANCE {
            self.lift_workspace(cx);
        }
        let Some(drag) = self.workspace_drag.as_mut().filter(|drag| drag.lifted) else {
            return false;
        };
        drag.target = target(position);
        cx.notify();
        true
    }

    /// Ends the drag on release, sending its move if it was lifted over a gap.
    /// Returns whether it had lifted, in which case the release is not a click.
    pub(super) fn release_workspace_drag(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.workspace_drag.take() else {
            return false;
        };
        if !drag.lifted {
            return false;
        }
        cx.notify();
        let Some(target) = drag.target else {
            return true;
        };
        let connection = &self.endpoints[self.selected_endpoint].connection;
        let (Some(handle), Some(snapshot)) = (&connection.handle, &self.live.snapshot) else {
            return true;
        };
        if snapshot.boot_id != drag.boot {
            return true;
        }
        if let Err(error) = handle.request(
            &drag.boot,
            Method::WorkspaceMoveBlock,
            target.request.params(),
        ) {
            self.local_error = Some(format!("Workspace move not sent: {error}"));
        }
        true
    }

    /// Drops a lifted row where it came from. Returns whether one was lifted.
    pub(crate) fn cancel_workspace_drag(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.workspace_drag.as_ref().is_some_and(|drag| drag.lifted) {
            return false;
        }
        self.workspace_drag = None;
        cx.notify();
        true
    }
}

/// Resolves the pointer to a gap, from each unit's rows and the move for every
/// gap, all prepared by the render the pointer is over.
pub(super) fn resolve(
    rows: &[(usize, usize)],
    requests: &[Option<MoveBlock>],
    dragged: usize,
    scroll: &gpui::ScrollHandle,
    y: Pixels,
) -> Option<Target> {
    let len = requests.len().checked_sub(1)?;
    let offset = scroll.offset().y;
    let mut spans: Vec<Option<(Pixels, Pixels)>> = vec![None; len];
    for &(unit, row) in rows {
        let Some(bounds) = scroll.bounds_for_item(row) else {
            continue;
        };
        let (top, bottom) = (bounds.top() + offset, bounds.bottom() + offset);
        let span = &mut spans[unit];
        *span = Some(span.map_or((top, bottom), |(t, b)| (t.min(top), b.max(bottom))));
    }
    let centers = spans.iter().enumerate().filter_map(|(unit, span)| {
        let (top, bottom) = (*span)?;
        (unit != dragged).then(|| (unit, f32::from(top + bottom) / 2.))
    });
    let slot = slot_at(centers, f32::from(y), len);
    let request = requests.get(slot)?.clone()?;
    Some(Target { slot, request })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use herdr_client::protocol::ClientShellWorktree;

    fn workspace(id: &str, tree: Option<(&str, bool)>) -> ClientShellWorkspace {
        ClientShellWorkspace {
            workspace_id: id.into(),
            active_tab_id: String::new(),
            new_workspace_cwd: String::new(),
            number: 0,
            label: id.into(),
            custom_label: false,
            branch: None,
            git_ahead_behind: None,
            tokens: Vec::new(),
            worktree: tree.map(|(key, linked)| ClientShellWorktree {
                key: key.into(),
                label: key.into(),
                is_linked_worktree: linked,
            }),
            focused: false,
            agent_status: herdr_client::protocol::AgentStatus::Idle,
        }
    }

    /// `a` heads a group whose child `a2` sits after `b` in daemon order.
    fn list() -> Vec<ClientShellWorkspace> {
        vec![
            workspace("a", Some(("repo-a", false))),
            workspace("a1", Some(("repo-a", true))),
            workspace("b", None),
            workspace("a2", Some(("repo-a", true))),
            workspace("c", None),
        ]
    }

    fn request(ids: &[&str], before: Option<&str>) -> Option<MoveBlock> {
        Some(MoveBlock {
            workspace_ids: ids.iter().map(|&id| id.into()).collect(),
            before: before.map(Into::into),
        })
    }

    #[test]
    fn a_top_level_row_carries_its_whole_group() {
        let list = list();
        let plan = Plan::new(&list, "a").unwrap();
        assert_eq!(plan.units, [vec![0, 1, 3], vec![2], vec![4]]);
        assert_eq!(plan.dragged, 0);
        // The gaps around the group itself move nothing.
        assert_eq!(plan.request(&list, 0), None);
        assert_eq!(plan.request(&list, 1), None);
        assert_eq!(
            plan.request(&list, 2),
            request(&["a", "a1", "a2"], Some("c"))
        );
        assert_eq!(plan.request(&list, 3), request(&["a", "a1", "a2"], None));
        assert_eq!(plan.request(&list, 4), None);
    }

    #[test]
    fn a_standalone_row_lands_before_a_group_s_earliest_member() {
        let mut list = list();
        // The group shows where `a1` sits, ahead of its own main checkout.
        list.swap(0, 1);
        let plan = Plan::new(&list, "c").unwrap();
        assert_eq!(plan.units, [vec![1, 0, 3], vec![2], vec![4]]);
        assert_eq!(plan.request(&list, 0), request(&["c"], Some("a1")));
    }

    #[test]
    fn a_child_moves_only_among_its_siblings() {
        let list = list();
        let plan = Plan::new(&list, "a1").unwrap();
        assert!(plan.children);
        assert_eq!(plan.units, [vec![1], vec![3]]);
        assert_eq!(plan.unit_of(2), None);
        assert_eq!(plan.request(&list, 0), None);
        assert_eq!(plan.request(&list, 1), None);
        // Last among siblings stays ahead of what follows the group.
        assert_eq!(plan.request(&list, 2), request(&["a1"], Some("c")));
        let plan = Plan::new(&list, "a2").unwrap();
        assert_eq!(plan.request(&list, 0), request(&["a2"], Some("a1")));
    }

    #[test]
    fn a_last_child_moved_to_the_end_of_the_list_needs_no_anchor() {
        let list = vec![
            workspace("a", Some(("repo-a", false))),
            workspace("a1", Some(("repo-a", true))),
            workspace("a2", Some(("repo-a", true))),
        ];
        let plan = Plan::new(&list, "a1").unwrap();
        assert_eq!(plan.request(&list, 2), request(&["a1"], None));
    }

    #[test]
    fn rows_with_nowhere_to_go_do_not_arm() {
        let list = vec![
            workspace("a", Some(("repo-a", false))),
            workspace("a1", Some(("repo-a", true))),
        ];
        assert_eq!(Plan::new(&list, "a"), None);
        assert_eq!(Plan::new(&list, "a1"), None);
        assert_eq!(Plan::new(&list, "missing"), None);
    }

    #[test]
    fn the_pointer_picks_the_gap_before_the_next_center() {
        let centers = [(0, 10.), (2, 50.), (3, 70.)];
        assert_eq!(slot_at(centers, 0., 4), 0);
        assert_eq!(slot_at(centers, 30., 4), 2);
        assert_eq!(slot_at(centers, 60., 4), 3);
        assert_eq!(slot_at(centers, 90., 4), 4);
    }

    #[test]
    fn the_drop_line_marks_the_first_or_last_row_of_a_unit() {
        let rows = [None, Some(0), Some(0), Some(1), Some(1)];
        assert_eq!(indicator(&rows, 0, 2), Some((1, Edge::Top)));
        assert_eq!(indicator(&rows, 1, 2), Some((3, Edge::Top)));
        assert_eq!(indicator(&rows, 2, 2), Some((4, Edge::Bottom)));
        assert_eq!(indicator(&[None], 0, 2), None);
    }
}
