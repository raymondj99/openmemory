//! Layout helpers for the Graph panel.
//!
//! Two visual modes:
//!
//! * **Adjacency-shell** — focused node centered at the origin,
//!   neighbours grouped by relation kind around it across eight
//!   compass sectors (N, NE, E, SE, S, SW, W, NW). Best for small
//!   neighborhoods (≤ [`LIST_FALLBACK_THRESHOLD`] total nodes).
//! * **Adjacency-list** — neighbours grouped by relation kind,
//!   rendered as a vertical list. Used as the fallback when the
//!   neighbourhood is too dense for the shell view (or when depth > 1).
//!
//! Both modes are produced by the same pure [`prepare`] function;
//! `screens/graph.rs` renders one or the other based on
//! [`PreparedLayout::mode`].
//!
//! Coordinate system for shell mode: nodes are placed in normalised
//! unit-circle space `(x, y) ∈ [-1, 1] × [-1, 1]` with `+y = north`
//! (mathematical convention). The Canvas renderer maps this into the
//! viewport rect, flipping `y` for terminal row coordinates. The
//! focused node is at the origin.

use std::collections::BTreeMap;

/// Above this neighbour count we fall back to adjacency-list mode
/// even at depth 1. Picked so the 8-sector shell stays legible.
pub const LIST_FALLBACK_THRESHOLD: usize = 12;

/// Radius of the neighbour ring in unit-circle coordinates. Slightly
/// less than 1.0 so labels don't clip against the viewport edge.
const SHELL_RADIUS: f64 = 0.78;

/// Half-width of one compass sector in radians (= π/8, i.e. 22.5°).
/// A sector hosts at most 3 nodes spread evenly across this window.
const SECTOR_HALF_WIDTH: f64 = std::f64::consts::FRAC_PI_8;

/// A single neighbour of the focus node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbor {
    pub name: String,
    pub entity_type: String,
    pub relation_kind: String,
    /// `true` when this neighbour is the `to_entity` of the relation,
    /// `false` when it is the `from_entity`. Used to render direction
    /// arrows in the list view and edge direction in the shell view.
    pub outbound: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Shell,
    List,
}

/// One placed node in the shell view.
#[derive(Debug, Clone, PartialEq)]
pub struct NodePosition {
    pub name: String,
    pub entity_type: String,
    pub relation_kind: String,
    pub outbound: bool,
    /// East-positive horizontal coordinate in `[-1, 1]`.
    pub x: f64,
    /// North-positive vertical coordinate in `[-1, 1]`.
    pub y: f64,
    /// Compass sector index (`0 = N`, clockwise). Used by compass-
    /// aware arrow-key navigation to pick the "next" neighbour.
    pub sector: u8,
}

/// One edge in the shell view. v1 always draws from the focused
/// entity at the origin out to the neighbour position.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeSegment {
    pub from_x: f64,
    pub from_y: f64,
    pub to_x: f64,
    pub to_y: f64,
    pub relation_kind: String,
    pub outbound: bool,
}

/// Prepared layout produced by [`prepare`]. List-mode callers read
/// `groups`; shell-mode callers read `positions` + `edges` (and may
/// still read `groups` for the legend / fallback render).
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedLayout {
    pub focus: String,
    pub mode: Mode,
    pub groups: Vec<RelationGroup>,
    pub total_neighbors: usize,
    /// Populated only when `mode == Mode::Shell`. Same order as the
    /// flat traversal of `groups` so a selection index maps cleanly
    /// across both views.
    pub positions: Vec<NodePosition>,
    /// Populated only when `mode == Mode::Shell`. One per neighbour.
    pub edges: Vec<EdgeSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationGroup {
    pub relation_kind: String,
    pub neighbors: Vec<Neighbor>,
}

/// Choose a layout mode and group neighbours by relation kind.
///
/// * `focus` — name of the centered entity (used in render only).
/// * `neighbors` — full neighbour list.
/// * `depth` — 1 or 2. Depth 2 always renders in list mode.
///
/// Total function: no input combination panics or returns `None`. An
/// empty neighbour slice produces an empty `groups` Vec and a list-
/// mode result so the renderer always has one stable path.
pub fn prepare(focus: &str, neighbors: &[Neighbor], depth: u8) -> PreparedLayout {
    let mode = if depth >= 2 || neighbors.len() > LIST_FALLBACK_THRESHOLD || neighbors.is_empty() {
        Mode::List
    } else {
        Mode::Shell
    };

    // Group by relation kind. BTreeMap gives deterministic order
    // (alphabetical), so the layout doesn't shuffle between refreshes.
    let mut by_kind: BTreeMap<String, Vec<Neighbor>> = BTreeMap::new();
    for n in neighbors {
        by_kind
            .entry(n.relation_kind.clone())
            .or_default()
            .push(n.clone());
    }
    for v in by_kind.values_mut() {
        v.sort_by(|a, b| a.name.cmp(&b.name));
    }
    let groups: Vec<RelationGroup> = by_kind
        .into_iter()
        .map(|(relation_kind, neighbors)| RelationGroup {
            relation_kind,
            neighbors,
        })
        .collect();

    let (positions, edges) = if mode == Mode::Shell {
        place_shell(&groups)
    } else {
        (Vec::new(), Vec::new())
    };

    PreparedLayout {
        focus: focus.to_string(),
        mode,
        groups,
        total_neighbors: neighbors.len(),
        positions,
        edges,
    }
}

/// Map each `RelationGroup` to a compass sector and place its
/// neighbours along an arc inside that sector. Returns one
/// [`NodePosition`] per neighbour (in the same flat traversal order
/// as `groups`) and one [`EdgeSegment`] per neighbour (from origin).
///
/// Sector assignment rule: groups are assigned round-robin to
/// sectors starting at North (sector 0) and going clockwise. Two
/// groups can share a sector if there are more than 8 groups; in
/// practice we hit the [`LIST_FALLBACK_THRESHOLD`] before that
/// matters.
fn place_shell(groups: &[RelationGroup]) -> (Vec<NodePosition>, Vec<EdgeSegment>) {
    let total: usize = groups.iter().map(|g| g.neighbors.len()).sum();
    let mut positions: Vec<NodePosition> = Vec::with_capacity(total);
    let mut edges: Vec<EdgeSegment> = Vec::with_capacity(total);

    for (group_idx, group) in groups.iter().enumerate() {
        let sector = (group_idx % 8) as u8;
        let sector_center = sector_angle(sector);
        let k = group.neighbors.len();
        for (i, n) in group.neighbors.iter().enumerate() {
            let theta = within_sector_angle(sector_center, i, k);
            let x = SHELL_RADIUS * theta.cos();
            let y = SHELL_RADIUS * theta.sin();
            positions.push(NodePosition {
                name: n.name.clone(),
                entity_type: n.entity_type.clone(),
                relation_kind: n.relation_kind.clone(),
                outbound: n.outbound,
                x,
                y,
                sector,
            });
            edges.push(EdgeSegment {
                from_x: 0.0,
                from_y: 0.0,
                to_x: x,
                to_y: y,
                relation_kind: n.relation_kind.clone(),
                outbound: n.outbound,
            });
        }
    }

    (positions, edges)
}

/// Centre-of-sector angle in radians. Sector 0 = North (+y axis).
/// Going clockwise: NE, E, SE, S, SW, W, NW.
fn sector_angle(sector: u8) -> f64 {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
    // North = π/2 in maths convention. Clockwise step = -π/4.
    FRAC_PI_2 - f64::from(sector) * FRAC_PI_4
}

/// Angle of the `i`-th node inside a sector of `count` total nodes.
/// Distributes nodes evenly across the sector's ±22.5° window so the
/// edges fan out symmetrically. For `count == 1` we sit at the
/// sector centre.
fn within_sector_angle(sector_center: f64, i: usize, count: usize) -> f64 {
    if count <= 1 {
        return sector_center;
    }
    // Step between adjacent nodes: total span / (count + 1) so we
    // get equal margins at both edges of the sector window.
    let span = 2.0 * SECTOR_HALF_WIDTH;
    let step = span / (count as f64 + 1.0);
    let offset = (i as f64 + 1.0) * step - SECTOR_HALF_WIDTH;
    sector_center + offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(name: &str, kind: &str, outbound: bool) -> Neighbor {
        Neighbor {
            name: name.into(),
            entity_type: "concept".into(),
            relation_kind: kind.into(),
            outbound,
        }
    }

    #[test]
    fn empty_neighborhood_produces_empty_groups() {
        let p = prepare("focus", &[], 1);
        assert!(p.groups.is_empty());
        assert!(p.positions.is_empty());
        assert!(p.edges.is_empty());
        assert_eq!(p.total_neighbors, 0);
        assert_eq!(p.mode, Mode::List);
    }

    #[test]
    fn shell_mode_until_threshold_then_list() {
        let small: Vec<Neighbor> = (0..LIST_FALLBACK_THRESHOLD)
            .map(|i| n(&format!("n{i}"), "uses", true))
            .collect();
        assert_eq!(prepare("f", &small, 1).mode, Mode::Shell);

        let oversize: Vec<Neighbor> = (0..=LIST_FALLBACK_THRESHOLD)
            .map(|i| n(&format!("n{i}"), "uses", true))
            .collect();
        assert_eq!(prepare("f", &oversize, 1).mode, Mode::List);
    }

    #[test]
    fn depth_two_always_lists() {
        let one = vec![n("a", "uses", true)];
        assert_eq!(prepare("f", &one, 2).mode, Mode::List);
    }

    #[test]
    fn neighbors_are_grouped_by_relation_kind() {
        let neighbors = vec![
            n("alpha", "uses", true),
            n("beta", "maintains", true),
            n("gamma", "uses", false),
        ];
        let p = prepare("f", &neighbors, 1);
        assert_eq!(p.groups.len(), 2);
        let kinds: Vec<&str> = p.groups.iter().map(|g| g.relation_kind.as_str()).collect();
        assert_eq!(kinds, vec!["maintains", "uses"]);
        let uses = p.groups.iter().find(|g| g.relation_kind == "uses").unwrap();
        assert_eq!(uses.neighbors.len(), 2);
    }

    #[test]
    fn total_neighbors_matches_group_sum_for_random_shapes() {
        let cases = [
            vec![],
            vec![n("a", "k", true)],
            vec![
                n("a", "x", true),
                n("b", "x", true),
                n("c", "y", false),
                n("d", "z", true),
                n("e", "x", true),
            ],
        ];
        for case in &cases {
            let p = prepare("f", case, 1);
            let sum: usize = p.groups.iter().map(|g| g.neighbors.len()).sum();
            assert_eq!(sum, p.total_neighbors, "drift on input {case:?}");
            assert_eq!(sum, case.len());
        }
    }

    #[test]
    fn no_neighbor_is_dropped_or_duplicated() {
        let neighbors: Vec<Neighbor> = (0..10)
            .map(|i| n(&format!("n{i}"), if i % 3 == 0 { "uses" } else { "knows" }, i % 2 == 0))
            .collect();
        let p = prepare("f", &neighbors, 1);
        let mut flat: Vec<&str> = p
            .groups
            .iter()
            .flat_map(|g| g.neighbors.iter().map(|n| n.name.as_str()))
            .collect();
        flat.sort_unstable();
        let mut expected: Vec<&str> = neighbors.iter().map(|n| n.name.as_str()).collect();
        expected.sort_unstable();
        assert_eq!(flat, expected);
    }

    // ───── shell-layout-specific invariants ─────

    #[test]
    fn shell_layout_emits_one_position_and_one_edge_per_neighbor() {
        let neighbors = vec![
            n("a", "uses", true),
            n("b", "uses", true),
            n("c", "maintains", false),
        ];
        let p = prepare("f", &neighbors, 1);
        assert_eq!(p.mode, Mode::Shell);
        assert_eq!(p.positions.len(), neighbors.len());
        assert_eq!(p.edges.len(), neighbors.len());
    }

    #[test]
    fn shell_positions_stay_inside_unit_circle() {
        let neighbors: Vec<Neighbor> = (0..LIST_FALLBACK_THRESHOLD)
            .map(|i| n(&format!("n{i}"), &format!("k{}", i % 4), i % 2 == 0))
            .collect();
        let p = prepare("f", &neighbors, 1);
        for pos in &p.positions {
            let r = (pos.x * pos.x + pos.y * pos.y).sqrt();
            assert!(r <= 1.0, "{} fell outside unit circle (r={r})", pos.name);
            // Also assert we actually reached the shell ring (not the
            // origin): the renderer relies on a non-zero radius for
            // edges to be visible.
            assert!(r > 0.1, "{} too close to origin (r={r})", pos.name);
        }
    }

    #[test]
    fn shell_edges_originate_at_origin() {
        let neighbors = vec![n("a", "uses", true), n("b", "maintains", false)];
        let p = prepare("f", &neighbors, 1);
        for e in &p.edges {
            assert_eq!((e.from_x, e.from_y), (0.0, 0.0));
        }
    }

    #[test]
    fn shell_groups_land_in_distinct_sectors_until_eight() {
        // Each relation kind gets its own sector, round-robin from N.
        let neighbors: Vec<Neighbor> = (0..4)
            .map(|i| n(&format!("n{i}"), &format!("kind{i}"), true))
            .collect();
        let p = prepare("f", &neighbors, 1);
        let sectors: Vec<u8> = p.positions.iter().map(|pos| pos.sector).collect();
        // Four distinct relation kinds -> four distinct sectors: 0, 1, 2, 3.
        assert_eq!(sectors, vec![0, 1, 2, 3]);
    }

    #[test]
    fn shell_collocated_group_members_spread_inside_their_sector() {
        // Three neighbours sharing one relation kind should occupy
        // three distinct positions within the same sector.
        let neighbors = vec![
            n("a", "uses", true),
            n("b", "uses", true),
            n("c", "uses", true),
        ];
        let p = prepare("f", &neighbors, 1);
        assert_eq!(p.positions.len(), 3);
        assert!(p.positions.iter().all(|pos| pos.sector == 0));
        // All three at distinct angles -> distinct (x, y).
        let mut xs: Vec<f64> = p.positions.iter().map(|pos| pos.x).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in xs.windows(2) {
            assert!((w[1] - w[0]).abs() > 1e-6, "duplicate x detected: {xs:?}");
        }
    }

    #[test]
    fn sector_zero_is_due_north() {
        // The N sector's centre angle is π/2 (positive y, zero x).
        let neighbors = vec![n("a", "uses", true)];
        let p = prepare("f", &neighbors, 1);
        let pos = &p.positions[0];
        assert_eq!(pos.sector, 0);
        assert!(pos.x.abs() < 1e-9, "expected x ≈ 0 for due-north, got {}", pos.x);
        assert!(pos.y > 0.5, "expected y > 0.5 for due-north, got {}", pos.y);
    }
}
