//! Bootstrap lane planning for the retained history graph.
//! Assign compact causal lanes and shared Git routing columns before live edits.

use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Default)]
struct SessionGitSpines {
    edge_lanes: HashMap<(String, String), usize>,
    shared_parents: HashSet<String>,
    lane_count: usize,
}

/// The established Git-style lane plan, without allocating per-row geometry.
/// Retained views apply this plan when their first batch of nodes arrives.
#[derive(Debug)]
pub(super) struct LanePlan {
    /// Node lanes, including Git's reserved leftmost lane.
    pub(super) nodes: HashMap<String, usize>,
    /// Exact session-to-Git edges sharing a routing spine.
    pub(super) spines: HashMap<(String, String), usize>,
    /// Number of routing lanes between Git and operation lanes.
    pub(super) spine_count: usize,
}

/// Plan compact causal lanes and shared Git anchors using the Activity rules.
#[must_use]
pub(super) fn plan_lanes<S: std::hash::BuildHasher>(
    nodes: &[String],
    parents: &HashMap<String, Vec<String>, S>,
    is_git: &impl Fn(&str) -> bool,
) -> LanePlan {
    let row_of = nodes
        .iter()
        .enumerate()
        .map(|(row, key)| (key.clone(), row))
        .collect();
    let spines = compute_session_git_spines(nodes, &row_of, parents, is_git);
    let lanes = compute_lane_map_reuse(
        nodes,
        &|key| parents.get(key).cloned().unwrap_or_default(),
        is_git,
        spines.lane_count,
        &spines.shared_parents,
    );
    LanePlan {
        nodes: lanes,
        spines: spines.edge_lanes,
        spine_count: spines.lane_count,
    }
}

/// Assign edge-only routing lanes to exact session→Git anchors.
///
/// Two or more sessions based on one commit share one spine. A commit with one
/// session child keeps a direct edge, avoiding a visually synthetic junction.
/// Different shared commit spines are interval-colored so a lane is reused only
/// after its previous spine has ended; overlapping targets therefore never
/// appear connected. Absolute spine lanes begin at one because Git owns lane
/// zero.
fn compute_session_git_spines<S: std::hash::BuildHasher>(
    nodes: &[String],
    row_of: &HashMap<String, usize>,
    parents: &HashMap<String, Vec<String>, S>,
    is_git: &impl Fn(&str) -> bool,
) -> SessionGitSpines {
    #[derive(Debug)]
    struct Group {
        parent: String,
        first_child_row: usize,
        parent_row: usize,
        children: Vec<String>,
    }

    let mut grouped: BTreeMap<String, Group> = BTreeMap::new();
    for (child_row, child) in nodes.iter().enumerate() {
        if is_git(child) {
            continue;
        }
        let node_parents = parents.get(child).map_or(&[][..], Vec::as_slice);
        for parent in node_parents {
            if !is_git(parent) {
                continue;
            }
            let Some(parent_row) = row_of.get(parent).copied() else {
                continue;
            };
            if parent_row <= child_row {
                continue;
            }
            let _: &mut Group = grouped
                .entry(parent.clone())
                .and_modify(|group| {
                    group.first_child_row = group.first_child_row.min(child_row);
                    group.children.push(child.clone());
                })
                .or_insert_with(|| Group {
                    parent: parent.clone(),
                    first_child_row: child_row,
                    parent_row,
                    children: vec![child.clone()],
                });
        }
    }

    let mut groups: Vec<Group> = grouped.into_values().collect();
    groups.sort_by(|left, right| {
        (left.first_child_row, left.parent_row, &left.parent).cmp(&(
            right.first_child_row,
            right.parent_row,
            &right.parent,
        ))
    });

    let mut lane_end_rows: Vec<usize> = Vec::new();
    let mut edge_lanes = HashMap::new();
    let mut shared_parents = HashSet::new();
    for group in groups {
        if group.children.len() < 2 {
            continue;
        }
        let slot = lane_end_rows
            .iter()
            .position(|end_row| *end_row < group.first_child_row)
            .unwrap_or(lane_end_rows.len());
        if let Some(end_row) = lane_end_rows.get_mut(slot) {
            *end_row = group.parent_row;
        } else {
            lane_end_rows.push(group.parent_row);
        }
        let spine_lane = slot.saturating_add(1);
        let _: bool = shared_parents.insert(group.parent.clone());
        for child in group.children {
            let _: Option<usize> = edge_lanes.insert((child, group.parent.clone()), spine_lane);
        }
    }

    SessionGitSpines {
        edge_lanes,
        shared_parents,
        lane_count: lane_end_rows.len(),
    }
}

/// Compute a topological ordering of `nodes` (parents before children).
///
/// Used to assign lanes independently of row order, so time-sorting rows does
/// not fragment a causal chain across lanes. Nodes with no present parents are
/// emitted first; remaining nodes (cycles) are appended in input order.
#[must_use]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::let_underscore_untyped,
    reason = "In-degree counters are bounded by the number of present parents; HashMap insert returns Option which is discarded"
)]
fn topological_order(nodes: &[String], parents_of: &impl Fn(&str) -> Vec<String>) -> Vec<String> {
    use std::collections::VecDeque;
    let present: HashSet<String> = nodes.iter().cloned().collect();
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut indegree: HashMap<String, usize> = HashMap::new();
    for key in nodes {
        let _ = indegree.entry(key.clone()).or_insert(0);
        for parent in parents_of(key) {
            if present.contains(&parent) {
                children_of.entry(parent).or_default().push(key.clone());
                *indegree.entry(key.clone()).or_insert(0) += 1;
            }
        }
    }
    // Seed the BFS queue from `nodes` in input order (not HashMap iteration
    // order, which is process-random): roots are then emitted in a stable,
    // meaningful tie-break (newest-first) and the whole ordering is
    // reproducible across processes.
    let mut queue: VecDeque<String> = nodes
        .iter()
        .filter(|key| indegree.get(*key) == Some(&0))
        .cloned()
        .collect();
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    while let Some(key) = queue.pop_front() {
        order.push(key.clone());
        if let Some(children) = children_of.get(&key) {
            for child in children {
                if let Some(deg) = indegree.get_mut(child) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }
    // Append any remaining (cyclic) nodes in input order.
    let emitted: HashSet<String> = order.iter().cloned().collect();
    for key in nodes {
        if !emitted.contains(key) {
            order.push(key.clone());
        }
    }
    order
}

/// Compute a node-key → lane map over a topological ordering.
///
/// Walks parents-before-children so each node inherits its first parent's lane,
/// keeping a causal chain contiguous on one lane regardless of row order.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    clippy::let_underscore_untyped,
    reason = "Lane indices are bounds-checked; HashMap insert returns Option which is discarded"
)]
fn compute_lane_map(
    topo: &[String],
    parents_of: &impl Fn(&str) -> Vec<String>,
) -> HashMap<String, usize> {
    let mut lane_of: HashMap<String, usize> = HashMap::new();
    // Track which lanes are currently occupied (by a node awaiting its parent).
    let mut active: Vec<Option<String>> = Vec::new();
    // Track, per parent, the lanes already taken by its children. Used to give
    // a fork's sibling branches distinct lanes (branch-out), so they don't all
    // collapse onto the shared parent's column.
    let mut child_lanes: HashMap<String, Vec<usize>> = HashMap::new();
    for key in topo {
        let parents = parents_of(key);
        // Inherit the first parent's lane if it already has one; otherwise take
        // a fresh lane. If the first parent already has another child on that
        // lane (a fork), take a fresh spare lane instead so the branches stay
        // on distinct columns.
        let my_lane = if let Some(&l) = lane_of.get(key) {
            l
        } else if let Some(first_parent) = parents.first() {
            let inherited = *lane_of.get(first_parent).unwrap_or(&0);
            if child_lanes
                .get(first_parent)
                .is_some_and(|ls| ls.contains(&inherited))
            {
                // The parent already has a child on this lane — fork branch.
                let pl = find_spare_lane_str(&active);
                if pl < active.len() {
                    active[pl] = Some(key.clone());
                } else {
                    active.push(Some(key.clone()));
                }
                pl
            } else {
                inherited
            }
        } else {
            let l = active.len();
            active.push(Some(key.clone()));
            let _: Option<usize> = lane_of.insert(key.clone(), l);
            l
        };
        // Record this node's lane.
        let _: Option<usize> = lane_of.insert(key.clone(), my_lane);
        // Record that this node occupies `my_lane` as a child of its first parent.
        if let Some(first_parent) = parents.first() {
            let ls = child_lanes.entry(first_parent.clone()).or_default();
            if !ls.contains(&my_lane) {
                ls.push(my_lane);
            }
        }
        // Secondary parents get fresh lanes (for merges).
        //
        // A secondary parent needs its own lane whenever it does not yet have
        // one, OR when its existing lane collides with this node's own lane
        // (the first parent's lane). The collision case is a fork-then-merge
        // diamond: two branches (B and C) both inherit the same root's lane,
        // so without reassignment they'd share one column and the merge would
        // be invisible. Reassigning the second branch to a fresh spare lane
        // keeps the two incoming edges on distinct columns so the merge jog
        // renders.
        for parent in parents.iter().skip(1) {
            let existing = lane_of.get(parent).copied();
            if existing.is_none() || existing == Some(my_lane) {
                let pl = find_spare_lane_str(&active);
                if pl < active.len() {
                    active[pl] = Some(parent.clone());
                } else {
                    active.push(Some(parent.clone()));
                }
                let _ = lane_of.insert(parent.clone(), pl);
            }
        }
    }
    lane_of
}

/// Find a spare lane index for string-keyed nodes.
fn find_spare_lane_str(active: &[Option<String>]) -> usize {
    if let Some(i) = active.iter().position(Option::is_none) {
        return i;
    }
    active.len()
}

// ---------------------------------------------------------------------------
// Lane reuse across disconnected chains
// ---------------------------------------------------------------------------

/// Compute a node-key → lane map with **freed-lane reuse** across disconnected
/// chains.
///
/// Unlike [`compute_lane_map`], which gives every root node its own permanent
/// fresh lane, this assigns lanes so two disconnected chains whose display-row
/// ranges do *not* overlap share one base lane instead of consuming separate
/// permanent ones. This keeps long histories readable when many sequential,
/// non-overlapping sessions would otherwise each claim their own column.
///
/// Each component first receives its normal compact local layout. Those local
/// lanes are then translated into the first global lane block where their exact
/// rendered geometry fits. Occupancy is tracked as row intervals per lane (node
/// dots, vertical runs, and transition rows), rather than reserving a component's
/// full bounding rectangle. A disconnected session can therefore reuse an
/// operation lane inside a long component's Git-only gap while active fork lanes
/// remain protected.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    clippy::let_underscore_untyped,
    reason = "Lane indices are bounds-checked against the active vector length"
)]
fn compute_lane_map_reuse(
    nodes_newest_first: &[String],
    parents_of: &impl Fn(&str) -> Vec<String>,
    is_git: &impl Fn(&str) -> bool,
    session_git_spine_count: usize,
    shared_session_git_parents: &HashSet<String>,
) -> HashMap<String, usize> {
    use std::collections::VecDeque;

    // --- Phase 0/1: connected components over undirected edges ------------------
    // Build undirected adjacency so we can flood-fill components regardless of
    // edge direction.
    let mut adj_undirected: HashMap<String, Vec<String>> = HashMap::new();
    for key in nodes_newest_first {
        let _: &mut Vec<String> = adj_undirected.entry(key.clone()).or_default();
        for parent in parents_of(key) {
            let _: &mut Vec<String> = adj_undirected.entry(parent.clone()).or_default();
            if let Some(neighbors) = adj_undirected.get_mut(&parent) {
                neighbors.push(key.clone());
            }
            let _: &mut Vec<String> = adj_undirected.entry(key.clone()).or_default();
            if let Some(neighbors) = adj_undirected.get_mut(key) {
                neighbors.push(parent.clone());
            }
        }
    }

    // Row index per key within `nodes_newest_first`.
    let mut row_of_key: HashMap<String, usize> = HashMap::with_capacity(nodes_newest_first.len());
    for (i, k) in nodes_newest_first.iter().enumerate() {
        let _ = row_of_key.insert(k.clone(), i);
    }

    // Flood-fill components; record each component's [start,end] row span where
    // start = smallest row index (= newest member), end = largest (= oldest).
    // Also record whether each component contains any git node, so git commits
    // can be pinned to the leftmost lane (0).
    let mut comp_id_of_key: HashMap<String, usize> = HashMap::new();
    let mut comp_start_end: Vec<(usize, usize)> = Vec::new(); // per comp id -> span
    let mut comp_is_git: Vec<bool> = Vec::new(); // per comp id -> contains a git node
    let mut seen_keys: HashSet<String> = HashSet::with_capacity(nodes_newest_first.len());
    for seed in nodes_newest_first {
        if seen_keys.contains(seed) {
            continue;
        }
        let _: bool = seen_keys.insert(seed.clone());
        let mut queue_local: VecDeque<String> = VecDeque::from([seed.clone()]);
        let mut members_start_end = (
            *row_of_key.get(seed).unwrap_or(&usize::MAX),
            *row_of_key.get(seed).unwrap_or(&usize::MAX),
        );
        let mut any_git = is_git(seed);
        while let Some(k) = queue_local.pop_front() {
            members_start_end = fold_span(
                members_start_end,
                *row_of_key.get(&k).unwrap_or(&usize::MAX),
            );
            if is_git(&k) {
                any_git = true;
            }
            let _ = comp_id_of_key.insert(k.clone(), comp_start_end.len());
            if let Some(neighbors) = adj_undirected.get(&k) {
                for nbr in neighbors {
                    if !seen_keys.contains(nbr) {
                        let _: bool = seen_keys.insert(nbr.clone());
                        queue_local.push_back(nbr.clone());
                    }
                }
            }
        }
        comp_start_end.push(members_start_end);
        comp_is_git.push(any_git);
    }

    // --- Phase 2: compute each component's compact local geometry -----------------
    // Local geometry must be known BEFORE global interval coloring: a component
    // with an active fork occupies more than its base lane. Coloring only bases
    // lets a later disconnected component collide with that still-live branch.
    let git_present = comp_is_git.iter().any(|&g| g);
    let mut members_by_component: Vec<Vec<String>> = vec![Vec::new(); comp_start_end.len()];
    for key in nodes_newest_first {
        if let Some(cid) = comp_id_of_key.get(key).copied() {
            members_by_component[cid].push(key.clone());
        }
    }

    let mut local_lanes_by_component: Vec<HashMap<String, usize>> =
        Vec::with_capacity(comp_start_end.len());
    let mut op_lane_rank_by_component: Vec<HashMap<usize, usize>> =
        Vec::with_capacity(comp_start_end.len());
    let mut operation_usage_by_component: Vec<Vec<Vec<RowInterval>>> =
        Vec::with_capacity(comp_start_end.len());
    for members in &members_by_component {
        let local = if members.is_empty() {
            HashMap::new()
        } else {
            let topo = topological_order(members, parents_of);
            let uncompacted = compute_lane_map(&topo, parents_of);
            let domain_separated = separate_git_and_operation_lanes(members, &uncompacted, is_git);
            // `compute_lane_map` never frees a lane, so sequential branches
            // inside one component would each keep a permanent column. Compact
            // only disjoint geometry; overlapping branches remain distinct.
            compact_component_lanes(
                members,
                &domain_separated,
                parents_of,
                is_git,
                ComponentGeometry {
                    row_of_key: &row_of_key,
                    shared_session_git_parents,
                },
            )
        };

        // Git is globally remapped to lane 0. Rank only the operation lanes so
        // every component's operation block is dense even when a local lane was
        // occupied solely by Git.
        let mut op_local_lanes: Vec<usize> = members
            .iter()
            .filter(|key| !is_git(key))
            .filter_map(|key| local.get(key).copied())
            .collect();
        op_local_lanes.sort_unstable();
        op_local_lanes.dedup();
        let mut rank_by_lane = HashMap::with_capacity(op_local_lanes.len());
        for (rank, lane) in op_local_lanes.iter().copied().enumerate() {
            let _: Option<usize> = rank_by_lane.insert(lane, rank);
        }
        let operation_usage = component_operation_lane_usage(
            members,
            ComponentLocalLayout {
                lanes: &local,
                operation_rank_by_lane: &rank_by_lane,
            },
            parents_of,
            is_git,
            ComponentGeometry {
                row_of_key: &row_of_key,
                shared_session_git_parents,
            },
        );
        operation_usage_by_component.push(operation_usage);
        op_lane_rank_by_component.push(rank_by_lane);
        local_lanes_by_component.push(local);
    }

    // --- Phase 3: geometry-aware greedy lane-block placement ----------------------
    // Preserve every component's compact local lane ordering, but reserve only
    // the rows where each translated lane has real geometry. This lets a small
    // disconnected component fit into a Git-only gap inside a much longer
    // component without colliding with live forks or merge runs.
    let mut comp_ids_sorted_by_start: Vec<usize> = comp_start_end
        .iter()
        .enumerate()
        .map(|(id, _)| id)
        .collect();
    comp_ids_sorted_by_start.sort_by_key(|&id| comp_start_end[id]);

    // Global operation-lane occupancy in final lane space. Lane 0 stays reserved
    // for Git; exact session-base routing spines occupy the following lanes and
    // are likewise never considered operation-lane candidates.
    let minimum_op_lane = usize::from(git_present).saturating_add(session_git_spine_count);
    let mut global_lane_usage: Vec<Vec<RowInterval>> = vec![Vec::new(); minimum_op_lane];
    let mut comp_base_lane: Vec<usize> = vec![minimum_op_lane; comp_start_end.len()];

    for &cid in &comp_ids_sorted_by_start {
        let operation_usage = &operation_usage_by_component[cid];
        if operation_usage.is_empty() {
            continue;
        }

        let mut base = minimum_op_lane;
        while !component_usage_fits(&global_lane_usage, operation_usage, base) {
            base = base.saturating_add(1);
        }
        comp_base_lane[cid] = base;
        reserve_component_usage(&mut global_lane_usage, operation_usage, base);
    }

    // --- Phase 4: map compact local lanes into their allocated global blocks -------
    let mut lane_of: HashMap<String, usize> = HashMap::with_capacity(nodes_newest_first.len());
    for (cid, members) in members_by_component.iter().enumerate() {
        let local = &local_lanes_by_component[cid];
        let rank_by_lane = &op_lane_rank_by_component[cid];
        let base = comp_base_lane[cid];
        for key in members {
            if is_git(key) {
                let _ = lane_of.insert(key.clone(), 0);
            } else {
                let local_lane = *local.get(key).unwrap_or(&0);
                let rank = *rank_by_lane.get(&local_lane).unwrap_or(&0);
                let _ = lane_of.insert(key.clone(), base.saturating_add(rank));
            }
        }
    }

    lane_of
}

type RowInterval = (usize, usize);

#[derive(Debug, Clone, Copy)]
struct LaneRun {
    lane: usize,
    rows: RowInterval,
}

#[derive(Debug, Clone, Copy)]
struct LaneTransition {
    row: usize,
    first_lane: usize,
    last_lane: usize,
}

#[derive(Debug, Clone, Copy)]
struct EdgeLaneUsage {
    runs: [Option<LaneRun>; 2],
    transition: Option<LaneTransition>,
}

#[derive(Debug, Clone, Copy)]
struct ComponentLocalLayout<'a> {
    lanes: &'a HashMap<String, usize>,
    operation_rank_by_lane: &'a HashMap<usize, usize>,
}

#[derive(Debug, Clone, Copy)]
struct ComponentGeometry<'a> {
    row_of_key: &'a HashMap<String, usize>,
    shared_session_git_parents: &'a HashSet<String>,
}

/// Describe the lanes and rows touched by one downward edge.
fn edge_lane_usage(
    child_row: usize,
    child_lane: usize,
    parent_row: usize,
    parent_lane: usize,
    parent_anchored: bool,
) -> EdgeLaneUsage {
    let transition = (child_lane != parent_lane).then(|| LaneTransition {
        row: if parent_anchored {
            parent_row
        } else if parent_row == child_row.saturating_add(1) {
            child_row
        } else {
            parent_row.saturating_sub(1)
        },
        first_lane: child_lane.min(parent_lane),
        last_lane: child_lane.max(parent_lane),
    });

    if child_lane == parent_lane || parent_anchored {
        return EdgeLaneUsage {
            runs: [
                Some(LaneRun {
                    lane: child_lane,
                    rows: (child_row, parent_row),
                }),
                None,
            ],
            transition,
        };
    }
    if parent_row == child_row.saturating_add(1) {
        return EdgeLaneUsage {
            runs: [
                Some(LaneRun {
                    lane: parent_lane,
                    rows: (child_row, parent_row),
                }),
                None,
            ],
            transition,
        };
    }

    let jog_row = parent_row.saturating_sub(1);
    EdgeLaneUsage {
        runs: [
            Some(LaneRun {
                lane: child_lane,
                rows: (child_row, jog_row),
            }),
            Some(LaneRun {
                lane: parent_lane,
                rows: (jog_row, parent_row),
            }),
        ],
        transition,
    }
}

/// Compute exact rendered row intervals for each dense operation-lane rank in
/// one connected component. Git occupies canonical lane 0 while operation ranks
/// start at 1, allowing Git↔operation transition rows to reserve every operation
/// lane crossed inside the component's translated block.
fn component_operation_lane_usage(
    members: &[String],
    local: ComponentLocalLayout<'_>,
    parents_of: &impl Fn(&str) -> Vec<String>,
    is_git: &impl Fn(&str) -> bool,
    geometry: ComponentGeometry<'_>,
) -> Vec<Vec<RowInterval>> {
    let mut usage: Vec<Vec<RowInterval>> = vec![Vec::new(); local.operation_rank_by_lane.len()];
    let op_offset = usize::from(members.iter().any(|key| is_git(key)));
    let child_counts = count_children(members, parents_of);

    for key in members {
        let Some(child_row) = geometry.row_of_key.get(key).copied() else {
            continue;
        };
        let child_lane = canonical_component_lane(key, local, is_git, op_offset);
        record_canonical_run(
            &mut usage,
            op_offset,
            LaneRun {
                lane: child_lane,
                rows: (child_row, child_row),
            },
        );

        for parent in parents_of(key) {
            let Some(parent_row) = geometry.row_of_key.get(&parent).copied() else {
                continue;
            };
            if parent_row <= child_row {
                continue;
            }
            // This shared edge is routed on a dedicated spine outside the
            // operation-lane block. A lone session→Git edge remains direct and
            // therefore contributes its ordinary lane usage below.
            if !is_git(key)
                && is_git(&parent)
                && geometry
                    .shared_session_git_parents
                    .contains(parent.as_str())
            {
                continue;
            }
            let parent_lane = canonical_component_lane(&parent, local, is_git, op_offset);
            let parent_anchored = child_counts.get(&parent).copied().unwrap_or(0) > 1;
            let edge = edge_lane_usage(
                child_row,
                child_lane,
                parent_row,
                parent_lane,
                parent_anchored,
            );
            for run in edge.runs.into_iter().flatten() {
                record_canonical_run(&mut usage, op_offset, run);
            }
            if let Some(transition) = edge.transition {
                record_canonical_transition(&mut usage, op_offset, transition);
            }
        }
    }

    for intervals in &mut usage {
        merge_row_intervals(intervals);
    }
    usage
}

fn canonical_component_lane(
    key: &str,
    local: ComponentLocalLayout<'_>,
    is_git: &impl Fn(&str) -> bool,
    op_offset: usize,
) -> usize {
    if is_git(key) {
        return 0;
    }
    let local_lane = local.lanes.get(key).copied().unwrap_or(0);
    op_offset.saturating_add(
        local
            .operation_rank_by_lane
            .get(&local_lane)
            .copied()
            .unwrap_or(0),
    )
}

fn record_canonical_run(usage: &mut [Vec<RowInterval>], op_offset: usize, run: LaneRun) {
    let Some(rank) = run.lane.checked_sub(op_offset) else {
        return;
    };
    if let Some(intervals) = usage.get_mut(rank) {
        intervals.push(run.rows);
    }
}

fn record_canonical_transition(
    usage: &mut [Vec<RowInterval>],
    op_offset: usize,
    transition: LaneTransition,
) {
    for lane in transition.first_lane..=transition.last_lane {
        record_canonical_run(
            usage,
            op_offset,
            LaneRun {
                lane,
                rows: (transition.row, transition.row),
            },
        );
    }
}

fn count_children(
    members: &[String],
    parents_of: &impl Fn(&str) -> Vec<String>,
) -> HashMap<String, usize> {
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for key in members {
        for parent in parents_of(key) {
            let count = child_counts.entry(parent).or_default();
            *count = count.saturating_add(1);
        }
    }
    child_counts
}

fn component_usage_fits(
    global_usage: &[Vec<RowInterval>],
    component_usage: &[Vec<RowInterval>],
    base: usize,
) -> bool {
    component_usage.iter().enumerate().all(|(rank, intervals)| {
        global_usage
            .get(base.saturating_add(rank))
            .is_none_or(|occupied| !row_interval_lists_overlap(intervals, occupied))
    })
}

fn reserve_component_usage(
    global_usage: &mut Vec<Vec<RowInterval>>,
    component_usage: &[Vec<RowInterval>],
    base: usize,
) {
    let needed = base.saturating_add(component_usage.len());
    global_usage.resize_with(needed, Vec::new);
    for (rank, intervals) in component_usage.iter().enumerate() {
        if let Some(occupied) = global_usage.get_mut(base.saturating_add(rank)) {
            occupied.extend_from_slice(intervals);
            merge_row_intervals(occupied);
        }
    }
}

fn row_interval_lists_overlap(left: &[RowInterval], right: &[RowInterval]) -> bool {
    let mut left_index = 0usize;
    let mut right_index = 0usize;
    while let (Some(&(left_lo, left_hi)), Some(&(right_lo, right_hi))) =
        (left.get(left_index), right.get(right_index))
    {
        if left_lo <= right_hi && right_lo <= left_hi {
            return true;
        }
        if left_hi < right_lo {
            left_index = left_index.saturating_add(1);
        } else {
            right_index = right_index.saturating_add(1);
        }
    }
    false
}

fn merge_row_intervals(intervals: &mut Vec<RowInterval>) {
    intervals.sort_unstable();
    let mut merged: Vec<RowInterval> = Vec::with_capacity(intervals.len());
    for &(lo, hi) in intervals.iter() {
        if let Some(last) = merged.last_mut()
            && lo <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(hi);
            continue;
        }
        merged.push((lo, hi));
    }
    *intervals = merged;
}

/// Separate Git and operation nodes before in-component compaction.
///
/// The basic topology walk lets a component's first operation branch inherit
/// the same temporary lane as its Git parent. Git is remapped to lane zero in
/// the final layout, so retaining that temporary overlap would make the Git
/// history inflate the operation lane's usage span and prevent sequential
/// sessions from reusing it. Canonicalizing domains here gives Git lane zero
/// and densely ranks the original operation lanes from one onward.
fn separate_git_and_operation_lanes(
    members: &[String],
    lane_of: &HashMap<String, usize>,
    is_git: &impl Fn(&str) -> bool,
) -> HashMap<String, usize> {
    if !members.iter().any(|key| is_git(key)) {
        return lane_of.clone();
    }

    let mut operation_lanes: Vec<usize> = members
        .iter()
        .filter(|key| !is_git(key))
        .filter_map(|key| lane_of.get(key).copied())
        .collect();
    operation_lanes.sort_unstable();
    operation_lanes.dedup();
    let operation_rank: HashMap<usize, usize> = operation_lanes
        .into_iter()
        .enumerate()
        .map(|(rank, lane)| (lane, rank.saturating_add(1)))
        .collect();

    members
        .iter()
        .map(|key| {
            let lane = if is_git(key) {
                0
            } else {
                lane_of
                    .get(key)
                    .and_then(|lane| operation_rank.get(lane))
                    .copied()
                    .unwrap_or(1)
            };
            (key.clone(), lane)
        })
        .collect()
}

/// Merge lanes inside ONE component whose rendered row usage never overlaps.
///
/// [`compute_lane_map`] gives every root, fork branch, and colliding merge
/// parent its own fresh lane and never frees them, so sequential branches in a
/// single component — branches joined by explicit Git links, repeated fork
/// diamonds, sequential subagent forks — each claim a permanent column even
/// though their vertical segments occupy disjoint display rows. This pass
/// computes each lane's exact rendered row usage (node dots plus edge runs,
/// mirroring the above/below/transition geometry rules)
/// and collapses a lane into the first earlier lane whose usage-row span is
/// strictly disjoint, so a finished branch's lane becomes reusable.
///
/// The merge check is conservative and exact: two lanes merge only when their
/// usage-row spans are disjoint, so a merged column never carries two different
/// branches' segments at the same row (no crossing lines on one lane). Fork
/// siblings and merge parents whose branches overlap in time stay on distinct
/// lanes. After merging, the surviving lanes are renumbered in ascending
/// survivor order so the compacted local lane ids are contiguous (0..=width-1)
/// instead of leaving a gap at every collapsed lane. When no lane can merge,
/// every lane survives in order, so the assignment is byte-identical to the
/// uncompacted one.
///
/// Deterministic: `members` is the component in canonical newest-first display
/// order, spans are folded from fixed edges, and merging walks lane indices in
/// ascending order — no `HashMap` iteration order leaks into the mapping.
#[expect(
    clippy::indexing_slicing,
    reason = "lane indices are bounded by the component's lane/row counts"
)]
fn compact_component_lanes(
    members: &[String],
    lane_of: &HashMap<String, usize>,
    parents_of: &impl Fn(&str) -> Vec<String>,
    is_git: &impl Fn(&str) -> bool,
    geometry: ComponentGeometry<'_>,
) -> HashMap<String, usize> {
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for key in members {
        for parent in parents_of(key) {
            let count = child_counts.entry(parent).or_default();
            *count = count.saturating_add(1);
        }
    }
    // Per-lane used-row span (lo, hi): initialize one entry per lane in use.
    let mut lane_spans: Vec<(usize, usize)> = Vec::new();
    for key in members {
        let lane = *lane_of.get(key).unwrap_or(&0);
        while lane_spans.len() <= lane {
            lane_spans.push((usize::MAX, 0));
        }
    }
    // Fold node dots and every edge run into the lanes they touch, using the
    // same geometry the retained graph emits (same-lane run, parent-anchored fork
    // run, adjacent merge jog with no source run, or non-adjacent merge jog at
    // parent_row - 1).
    for key in members {
        let my_lane = *lane_of.get(key).unwrap_or(&0);
        let Some(child_row) = geometry.row_of_key.get(key).copied() else {
            continue;
        };
        lane_spans[my_lane] = fold_span(lane_spans[my_lane], child_row);
        for parent in parents_of(key) {
            let Some(parent_row) = geometry.row_of_key.get(&parent).copied() else {
                continue;
            };
            if parent_row <= child_row {
                continue; // not a downward edge
            }
            // Shared session→Git edges leave the operation block at the child
            // row and run on their reserved spine. A lone edge remains direct
            // and keeps its source lane occupied through the Git target row.
            if !is_git(key)
                && is_git(&parent)
                && geometry
                    .shared_session_git_parents
                    .contains(parent.as_str())
            {
                continue;
            }
            let p_lane = *lane_of.get(&parent).unwrap_or(&my_lane);
            let parent_anchored = child_counts.get(&parent).copied().unwrap_or(0) > 1;
            if my_lane == p_lane {
                lane_spans[my_lane] = fold_span(lane_spans[my_lane], parent_row);
            } else if parent_anchored {
                // Parent-anchored fork: the source lane remains live through
                // the parent row's top half; the destination lane is already
                // occupied there by the parent node itself.
                lane_spans[my_lane] = fold_span(lane_spans[my_lane], parent_row);
            } else if parent_row == child_row.saturating_add(1) {
                // Adjacent cross-lane: the jog starts at the child's midpoint,
                // so the source lane carries no run; the destination lane gets
                // the two endpoint halves only.
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], child_row);
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], parent_row);
            } else {
                // Non-adjacent cross-lane: source run down to parent_row - 1,
                // jog, then destination run parent_row - 1..=parent_row.
                lane_spans[my_lane] = fold_span(lane_spans[my_lane], parent_row.saturating_sub(1));
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], parent_row.saturating_sub(1));
                lane_spans[p_lane] = fold_span(lane_spans[p_lane], parent_row);
            }
        }
    }

    // Merge in ascending lane order: a lane collapses into the first earlier
    // lane whose accumulated usage span is strictly disjoint, extending that
    // target's span so later lanes check against everything merged so far.
    let mut remap: Vec<usize> = (0..lane_spans.len()).collect();
    let first_operation_lane = usize::from(members.iter().any(|key| is_git(key)));
    for lane in first_operation_lane.saturating_add(1)..lane_spans.len() {
        let (lo, hi) = lane_spans[lane];
        for earlier in first_operation_lane..lane {
            let target = remap[earlier];
            let (elo, ehi) = lane_spans[target];
            if hi < elo || ehi < lo {
                lane_spans[target] = fold_span(lane_spans[target], lo);
                lane_spans[target] = fold_span(lane_spans[target], hi);
                remap[lane] = target;
                break;
            }
        }
    }

    // Densify the survivors: merging collapses lanes onto earlier targets, so
    // the surviving target ids are ascending but sparse (every collapsed lane
    // leaves a gap). Renumber the survivors in ascending order to 0..=width-1
    // so the compacted local lane ids are contiguous again. This is a pure
    // relabeling — relative lane order, overlap disjointness, and the merged
    // spans are unchanged.
    let mut survivors: Vec<usize> = remap.clone();
    survivors.sort_unstable();
    survivors.dedup();
    let mut dense_of_survivor: HashMap<usize, usize> = HashMap::with_capacity(survivors.len());
    for (dense, survivor) in survivors.iter().copied().enumerate() {
        let _: Option<usize> = dense_of_survivor.insert(survivor, dense);
    }

    members
        .iter()
        .map(|key| {
            let lane = *lane_of.get(key).unwrap_or(&0);
            (
                key.clone(),
                *dense_of_survivor.get(&remap[lane]).unwrap_or(&0),
            )
        })
        .collect()
}

/// Fold a row index into a running `(min, max)` span.
fn fold_span(span: (usize, usize), row: usize) -> (usize, usize) {
    (span.0.min(row), span.1.max(row))
}
