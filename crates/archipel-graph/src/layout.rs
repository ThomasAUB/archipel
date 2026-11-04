//! Lane layout: assigns each row a column and a color and lists the edges to draw.
//!
//! The algorithm walks rows in display order (children before parents) and
//! keeps the *active lanes*: one slot per column, holding the row the lane is
//! heading to (a parent not yet reached) and its color.
//!
//! For each row:
//! 1. every lane heading to this row ends here ([`EdgeKind::Incoming`] edges);
//!    the commit sits in the leftmost of them, or in the leftmost free column
//!    (a branch tip) when none;
//! 2. other lanes pass by ([`EdgeKind::Through`]);
//! 3. the commit's lane continues toward its first parent, and each further
//!    parent joins the leftmost lane already heading to it or opens a new lane
//!    in the first free column right of the commit ([`EdgeKind::Outgoing`]).
//!
//! Columns never shift: a freed column is reused by the next new lane, so
//! pass-through lanes are always vertical. The state between two rows is small
//! ([`LaneState`]); cloning it every N rows gives the checkpoints from which any
//! window is replayed. The layout is a pure function of the parent links, so it
//! is deterministic and a replay from a checkpoint equals a full layout.

/// A parent link of a row, as stored in the topology cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParentLink {
    /// The parent is the commit at this row (always after the child).
    Row(u32),
    /// The parent is not part of the walk (shallow clone, missing object).
    Outside,
}

/// What part of a row an edge covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeKind {
    /// A lane passing by: from the top of `from` to the bottom of `to` (`from == to`).
    Through,
    /// A lane ending at this commit: from the top of column `from` to the commit node (column `to`).
    Incoming,
    /// A lane leaving this commit toward a parent: from the commit node (column `from`)
    /// to the bottom of column `to`. These, with [`EdgeKind::Through`], are the
    /// edges between this row and the next.
    Outgoing,
}

/// One line segment of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Edge {
    /// Segment kind.
    pub kind: EdgeKind,
    /// Start column.
    pub from: u32,
    /// End column.
    pub to: u32,
    /// Color index of the lane drawn (the UI maps it modulo its palette).
    pub color: u32,
}

/// Layout of one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowLayout {
    /// Column of the commit node.
    pub column: u32,
    /// Color index of the commit's lane.
    pub color: u32,
    /// Segments to draw, sorted by kind, then columns.
    pub edges: Vec<Edge>,
    /// Number of columns used by this row (before or after the commit).
    pub width: u32,
    /// Whether some parents are outside the walk (drawn as a stub, if at all).
    pub hidden_parents: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lane {
    /// Row the lane is heading to.
    target: u32,
    color: u32,
}

/// Active lanes between two rows: the state saved in a checkpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneState {
    lanes: Vec<Option<Lane>>,
    next_color: u32,
}

/// Converts a column index to the public `u32` representation.
fn col(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

impl LaneState {
    /// The state before the first row.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of columns currently in use (including free holes).
    #[must_use]
    pub fn width(&self) -> usize {
        self.lanes.len()
    }

    /// Heap and inline bytes held by this state.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.lanes.capacity() * std::mem::size_of::<Option<Lane>>()
    }

    /// Lays out `row`, whose parents are `parents` (first parent first), and
    /// advances the state to the next row.
    pub fn step(&mut self, row: u32, parents: &[ParentLink]) -> RowLayout {
        let mut edges = Vec::new();
        let mut layout = self.advance_with(row, parents, &mut |e| edges.push(e));
        edges.sort_unstable();
        layout.edges = edges;
        layout
    }

    /// Like [`LaneState::step`] without collecting edges (checkpoint pass);
    /// returns the row width.
    pub fn advance(&mut self, row: u32, parents: &[ParentLink]) -> u32 {
        self.advance_with(row, parents, &mut |_| {}).width
    }

    fn advance_with(
        &mut self,
        row: u32,
        parents: &[ParentLink],
        edges: &mut impl FnMut(Edge),
    ) -> RowLayout {
        let width_before = self.lanes.len();

        // 1. Lanes ending here, lanes passing by.
        let mut column = None;
        for (c, slot) in self.lanes.iter().enumerate() {
            let Some(lane) = slot else { continue };
            if lane.target == row {
                let commit_col = *column.get_or_insert(c);
                edges(Edge {
                    kind: EdgeKind::Incoming,
                    from: col(c),
                    to: col(commit_col),
                    color: lane.color,
                });
            } else {
                edges(Edge {
                    kind: EdgeKind::Through,
                    from: col(c),
                    to: col(c),
                    color: lane.color,
                });
            }
        }
        let (column, color) = if let Some(c) = column {
            let color = self.lanes[c].map_or(0, |l| l.color);
            // Merged-in lanes end at this commit.
            for slot in &mut self.lanes[c + 1..] {
                if slot.is_some_and(|l| l.target == row) {
                    *slot = None;
                }
            }
            (c, color)
        } else {
            let c = self.free_slot(0);
            (c, self.fresh_color())
        };

        // 2. The commit's own lane follows the first parent.
        let mut hidden_parents = false;
        match parents.first() {
            Some(ParentLink::Row(p)) => {
                self.lanes[column] = Some(Lane { target: *p, color });
                edges(Edge {
                    kind: EdgeKind::Outgoing,
                    from: col(column),
                    to: col(column),
                    color,
                });
            }
            Some(ParentLink::Outside) => {
                hidden_parents = true;
                self.lanes[column] = None;
            }
            None => self.lanes[column] = None,
        }

        // 3. Further parents join an existing lane or open a new one.
        for (i, parent) in parents.iter().enumerate().skip(1) {
            let ParentLink::Row(p) = *parent else {
                hidden_parents = true;
                continue;
            };
            if parents[..i].contains(parent) {
                continue;
            }
            let existing = self
                .lanes
                .iter()
                .position(|slot| slot.is_some_and(|l| l.target == p));
            let (to, lane_color) = if let Some(c) = existing {
                (c, self.lanes[c].map_or(0, |l| l.color))
            } else {
                let c = self.free_slot(column + 1);
                let lane_color = self.fresh_color();
                self.lanes[c] = Some(Lane {
                    target: p,
                    color: lane_color,
                });
                (c, lane_color)
            };
            edges(Edge {
                kind: EdgeKind::Outgoing,
                from: col(column),
                to: col(to),
                color: lane_color,
            });
        }

        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        RowLayout {
            column: col(column),
            color,
            edges: Vec::new(),
            width: col(width_before.max(self.lanes.len()).max(column + 1)),
            hidden_parents,
        }
    }

    /// First free column at or after `from`, growing the lane vector if needed.
    fn free_slot(&mut self, from: usize) -> usize {
        if let Some(offset) = self.lanes.iter().skip(from).position(Option::is_none) {
            return from + offset;
        }
        if self.lanes.len() < from {
            self.lanes.resize(from, None);
        }
        self.lanes.push(None);
        self.lanes.len() - 1
    }

    fn fresh_color(&mut self) -> u32 {
        let color = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ParentLink::{Outside, Row};

    fn layout(graph: &[&[ParentLink]]) -> Vec<RowLayout> {
        let mut state = LaneState::new();
        graph
            .iter()
            .zip(0u32..)
            .map(|(parents, row)| state.step(row, parents))
            .collect()
    }

    fn e(kind: EdgeKind, from: u32, to: u32) -> (EdgeKind, u32, u32) {
        (kind, from, to)
    }

    fn shape(r: &RowLayout) -> Vec<(EdgeKind, u32, u32)> {
        r.edges.iter().map(|x| (x.kind, x.from, x.to)).collect()
    }

    use EdgeKind::{Incoming, Outgoing, Through};

    #[test]
    fn linear_history_stays_in_column_zero() {
        let rows = layout(&[&[Row(1)], &[Row(2)], &[]]);
        assert!(rows.iter().all(|r| r.column == 0 && r.color == 0));
        assert_eq!(shape(&rows[0]), vec![e(Outgoing, 0, 0)]);
        assert_eq!(shape(&rows[1]), vec![e(Incoming, 0, 0), e(Outgoing, 0, 0)]);
        assert_eq!(shape(&rows[2]), vec![e(Incoming, 0, 0)]);
        assert!(rows.iter().all(|r| r.width == 1));
    }

    #[test]
    fn branch_and_merge() {
        // 0: merge M (parents 1, 2)
        // 1: main A  (parent 3)
        // 2: feature F (parent 3)
        // 3: base B
        let rows = layout(&[&[Row(1), Row(2)], &[Row(3)], &[Row(3)], &[]]);
        assert_eq!(rows[0].column, 0);
        assert_eq!(shape(&rows[0]), vec![e(Outgoing, 0, 0), e(Outgoing, 0, 1)]);
        assert_eq!(rows[1].column, 0);
        assert_eq!(
            shape(&rows[1]),
            vec![e(Through, 1, 1), e(Incoming, 0, 0), e(Outgoing, 0, 0)]
        );
        assert_eq!(rows[2].column, 1);
        assert_ne!(rows[2].color, rows[1].color);
        assert_eq!(
            shape(&rows[2]),
            vec![e(Through, 0, 0), e(Incoming, 1, 1), e(Outgoing, 1, 1)]
        );
        // Both lanes converge on the base.
        assert_eq!(rows[3].column, 0);
        assert_eq!(shape(&rows[3]), vec![e(Incoming, 0, 0), e(Incoming, 1, 0)]);
        assert_eq!(rows[3].width, 2);
    }

    #[test]
    fn octopus_merge_opens_one_lane_per_parent() {
        let rows = layout(&[
            &[Row(1), Row(2), Row(3)],
            &[Row(4)],
            &[Row(4)],
            &[Row(4)],
            &[],
        ]);
        assert_eq!(
            shape(&rows[0]),
            vec![e(Outgoing, 0, 0), e(Outgoing, 0, 1), e(Outgoing, 0, 2)]
        );
        assert_eq!(rows[0].width, 3);
        assert_eq!([rows[1].column, rows[2].column, rows[3].column], [0, 1, 2]);
        assert_eq!(
            shape(&rows[4]),
            vec![e(Incoming, 0, 0), e(Incoming, 1, 0), e(Incoming, 2, 0)]
        );
        let colors: std::collections::HashSet<u32> = rows[..4].iter().map(|r| r.color).collect();
        assert_eq!(colors.len(), 3);
    }

    #[test]
    fn multiple_roots_free_their_columns() {
        // Two unrelated histories: 0 -> 2, 1 -> 3 (roots 2 and 3).
        let rows = layout(&[&[Row(2)], &[Row(3)], &[], &[]]);
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[1].column, 1);
        assert_eq!(rows[2].column, 0);
        assert_eq!(shape(&rows[2]), vec![e(Through, 1, 1), e(Incoming, 0, 0)]);
        assert_eq!(rows[3].column, 1);
        assert_eq!(shape(&rows[3]), vec![e(Incoming, 1, 1)]);
        // A new tip after both roots reuses column 0.
        let mut state = LaneState::new();
        state.step(0, &[]);
        assert_eq!(state.step(1, &[]).column, 0);
        assert_eq!(state.width(), 0);
    }

    #[test]
    fn crossing_lanes_pass_through_vertically() {
        // 0: tip X (parent 3) col 0
        // 1: merge M (parents 2, 3) col 1; parent 3 already has lane 0 → edge 1→0
        // 2: Y (parent 3) col 1
        // 3: base
        let rows = layout(&[&[Row(3)], &[Row(2), Row(3)], &[Row(3)], &[]]);
        assert_eq!(rows[1].column, 1);
        assert_eq!(
            shape(&rows[1]),
            vec![e(Through, 0, 0), e(Outgoing, 1, 0), e(Outgoing, 1, 1)]
        );
        // The merge edge takes the color of the lane it joins.
        let joined = rows[1].edges.iter().find(|x| x.to == 0).map(|x| x.color);
        assert_eq!(joined, Some(rows[0].color));
        assert_eq!(rows[3].column, 0);
        assert_eq!(shape(&rows[3]), vec![e(Incoming, 0, 0), e(Incoming, 1, 0)]);

        // A lane opened right of the commit crosses an existing lane.
        // 0: A (parent 3) col 0; 1: B (parent 4) col 1; 2: M (parents 3, 4) col 2
        let rows = layout(&[&[Row(3)], &[Row(4)], &[Row(3), Row(4)], &[], &[]]);
        assert_eq!(rows[2].column, 2);
        assert_eq!(
            shape(&rows[2]),
            vec![
                e(Through, 0, 0),
                e(Through, 1, 1),
                e(Outgoing, 2, 1),
                e(Outgoing, 2, 2)
            ]
        );
    }

    #[test]
    fn outside_parents_end_the_lane() {
        let rows = layout(&[&[Row(1), Outside], &[Outside]]);
        assert!(rows[0].hidden_parents);
        assert_eq!(shape(&rows[0]), vec![e(Outgoing, 0, 0)]);
        assert!(rows[1].hidden_parents);
        assert_eq!(shape(&rows[1]), vec![e(Incoming, 0, 0)]);
    }

    #[test]
    fn duplicate_parents_draw_one_edge() {
        let rows = layout(&[&[Row(1), Row(1)], &[]]);
        assert_eq!(shape(&rows[0]), vec![e(Outgoing, 0, 0)]);
    }

    #[test]
    fn replay_from_saved_state_matches() {
        let graph: &[&[ParentLink]] = &[
            &[Row(2)],
            &[Row(3), Row(4)],
            &[Row(5)],
            &[Row(5)],
            &[Row(5)],
            &[],
        ];
        let full = layout(graph);
        let mut state = LaneState::new();
        let mut saved = None;
        for (row, parents) in (0u32..).zip(graph) {
            if row == 3 {
                saved = Some(state.clone());
            }
            state.step(row, parents);
        }
        let mut state = saved.unwrap_or_default();
        for (row, parents) in (3u32..).zip(&graph[3..]) {
            assert_eq!(state.step(row, parents), full[row as usize]);
        }
    }
}
