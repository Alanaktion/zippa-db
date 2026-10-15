//! ER diagram model: the tables, their columns, and the foreign keys between
//! them, a deterministic layered layout, and an SVG renderer shared by the
//! on-screen canvas and the export.
//!
//! The layout is Sugiyama-style, left to right, with a referenced table to
//! the left of the tables that reference it:
//!
//! 1. Tables are split into connected components, each laid out on its own
//!    so unrelated clusters never interleave; tables with no relations at
//!    all are packed into a grid of their own.
//! 2. Cycles are broken by reversing the arcs that run backwards in a DFS
//!    order (for the layout only — every edge is still drawn the way the
//!    foreign key points).
//! 3. Tables are given layers in topological order, each in the first layer
//!    past its parents with room under a height budget, so a hub table with
//!    fifty children spills them over several columns instead of one
//!    endless one.
//! 4. An edge spanning several layers gets a dummy slot in each layer it
//!    crosses, so it takes part in the ordering and is routed through the
//!    gaps between tables rather than over them.
//! 5. Each layer is ordered by barycenter sweeps over the column rows the
//!    edges attach to, keeping the order with the fewest crossings seen.
//! 6. Each table is pulled towards the rows it connects to (an isotonic
//!    regression per layer, which keeps the order and the spacing), so
//!    related tables sit level with each other.
//!
//! Every step breaks ties by input order, so the same schema always gives
//! the same picture.

use std::collections::{HashMap, HashSet};

use super::query::QueryResult;

/// One column of a table, as the diagram shows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ErdColumn {
    pub name: String,
    pub data_type: String,
    pub primary_key: bool,
}

/// A table participating in the diagram.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ErdTable {
    /// Schema on engines that have them; `None` for MySQL/SQLite.
    pub schema: Option<String>,
    pub name: String,
    /// In ordinal order. Empty on the ends of an [`ErdRelation`], which only
    /// name the table.
    pub columns: Vec<ErdColumn>,
}

impl ErdTable {
    fn key(&self) -> (String, String) {
        (self.schema.clone().unwrap_or_default(), self.name.clone())
    }

    /// Schema-qualified where there is a schema to qualify with.
    pub fn display_name(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{schema}.{}", self.name),
            None => self.name.clone(),
        }
    }

    fn row_of(&self, column: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == column)
    }
}

/// One foreign-key relation: `from` references `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErdRelation {
    pub from: ErdTable,
    pub to: ErdTable,
    /// The constraint's name, for the SVG title.
    pub name: String,
    /// The referencing columns, in key order.
    pub from_columns: Vec<String>,
    /// The referenced columns, in key order. Empty when SQLite leaves them
    /// implicit, which means the referenced table's primary key.
    pub to_columns: Vec<String>,
}

/// One laid-out table: its box, with (`x`, `y`) the top-left corner in
/// world units.
#[derive(Debug, Clone)]
pub struct ErdNode {
    pub table: ErdTable,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ErdNode {
    /// The vertical center of a column's row, or of the header for `None`.
    pub fn row_y(&self, row: Option<usize>) -> f32 {
        self.y + port_offset(row)
    }
}

/// One drawn relation, by node index.
#[derive(Debug, Clone)]
pub struct ErdEdge {
    pub from: usize,
    pub to: usize,
    pub name: String,
    pub from_columns: Vec<String>,
    pub to_columns: Vec<String>,
    /// The row of the first referencing column in the `from` table, or
    /// `None` (the header) when it is not one the table lists.
    pub from_row: Option<usize>,
    /// The same for the first referenced column in the `to` table.
    pub to_row: Option<usize>,
    /// The route, from the referencing row to the referenced one: the two
    /// end points on the tables' sides with the gaps it passes through in
    /// between. Each pair of points is joined by a curve that leaves and
    /// arrives horizontally.
    pub path: Vec<(f32, f32)>,
}

/// The laid-out diagram: node boxes plus the relations between them.
#[derive(Debug, Clone, Default)]
pub struct ErdDiagram {
    pub nodes: Vec<ErdNode>,
    pub edges: Vec<ErdEdge>,
}

/// Box geometry in world units. The SVG draws with the same numbers the
/// routing attaches edges by.
const HEADER_HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 20.0;
const MIN_NODE_WIDTH: f32 = 160.0;
const MAX_NODE_WIDTH: f32 = 360.0;
const CHAR_WIDTH: f32 = 7.0;
const ROW_PADDING: f32 = 8.0;
const TAG_WIDTH: f32 = 22.0;

/// Spacing: between layers (room for the curves), between tables in a
/// layer, around a dummy slot, and between laid-out blocks.
const LAYER_GAP: f32 = 120.0;
const NODE_GAP: f32 = 32.0;
const DUMMY_HEIGHT: f32 = 10.0;
const DUMMY_GAP: f32 = 6.0;
const BLOCK_GAP: f32 = 120.0;

/// The width-to-height ratio the layering and the packing aim for.
const ASPECT: f32 = 1.6;
const ORDER_SWEEPS: usize = 24;
const PLACE_PASSES: usize = 8;

/// How far right of a table a self-referencing edge loops out.
pub const SELF_LOOP_REACH: f32 = 40.0;

/// Lay out `tables` and their `relations`, deterministically: the same input
/// always produces the same positions. Relations naming a table that is not
/// listed are dropped, as are duplicates (the same columns of the same table
/// referencing the same table; the first name wins).
pub fn layout_diagram(tables: &[ErdTable], relations: &[ErdRelation]) -> ErdDiagram {
    let index: HashMap<(String, String), usize> = tables
        .iter()
        .enumerate()
        .map(|(i, table)| (table.key(), i))
        .collect();

    let mut nodes: Vec<ErdNode> = tables
        .iter()
        .map(|table| ErdNode {
            table: table.clone(),
            x: 0.0,
            y: 0.0,
            width: node_width(table),
            height: node_height(table),
        })
        .collect();
    let mut edges = resolve_edges(tables, &index, relations);

    // Components, in order of their first table.
    let mut roots = UnionFind::new(tables.len());
    for edge in &edges {
        roots.union(edge.from, edge.to);
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<usize, usize> = HashMap::new();
    for i in 0..tables.len() {
        let root = roots.find(i);
        let group = *group_of.entry(root).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[group].push(i);
    }
    let (mut connected, isolated): (Vec<_>, Vec<_>) =
        groups.into_iter().partition(|group| group.len() > 1);
    // Largest first, so the main cluster leads; the sort is stable.
    connected.sort_by_key(|group| std::cmp::Reverse(group.len()));

    let mut blocks: Vec<Block> = connected
        .iter()
        .map(|group| layered(group, &nodes, &edges))
        .collect();
    if !isolated.is_empty() {
        let members: Vec<usize> = isolated.into_iter().flatten().collect();
        let sizes: Vec<(f32, f32)> = members
            .iter()
            .map(|&i| (nodes[i].width, nodes[i].height))
            .collect();
        let (positions, width, height) = shelf_pack(&sizes, NODE_GAP * 2.0);
        blocks.push(Block {
            members,
            positions,
            waypoints: Vec::new(),
            width,
            height,
        });
    }

    let sizes: Vec<(f32, f32)> = blocks.iter().map(|b| (b.width, b.height)).collect();
    let (offsets, _, _) = shelf_pack(&sizes, BLOCK_GAP);
    let mut waypoints: Vec<Vec<(f32, f32)>> = vec![Vec::new(); edges.len()];
    for (block, (ox, oy)) in blocks.into_iter().zip(offsets) {
        for (&node, (x, y)) in block.members.iter().zip(block.positions) {
            nodes[node].x = ox + x;
            nodes[node].y = oy + y;
        }
        for (edge, points) in block.waypoints {
            waypoints[edge] = points.into_iter().map(|(x, y)| (ox + x, oy + y)).collect();
        }
    }

    for (edge, mut points) in edges.iter_mut().zip(waypoints) {
        edge.path = route(edge, &nodes, &mut points);
    }

    center(&mut nodes, &mut edges);
    ErdDiagram { nodes, edges }
}

/// Match each relation to its tables and rows, dropping the ones that miss.
fn resolve_edges(
    tables: &[ErdTable],
    index: &HashMap<(String, String), usize>,
    relations: &[ErdRelation],
) -> Vec<ErdEdge> {
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    for relation in relations {
        let (Some(&from), Some(&to)) = (
            index.get(&relation.from.key()),
            index.get(&relation.to.key()),
        ) else {
            continue;
        };
        if !seen.insert((from, to, relation.from_columns.clone())) {
            continue;
        }
        let to_columns = if relation.to_columns.is_empty() {
            tables[to]
                .columns
                .iter()
                .filter(|column| column.primary_key)
                .map(|column| column.name.clone())
                .collect()
        } else {
            relation.to_columns.clone()
        };
        edges.push(ErdEdge {
            from,
            to,
            name: relation.name.clone(),
            from_row: relation
                .from_columns
                .first()
                .and_then(|column| tables[from].row_of(column)),
            to_row: to_columns
                .first()
                .and_then(|column| tables[to].row_of(column)),
            from_columns: relation.from_columns.clone(),
            to_columns,
            path: Vec::new(),
        });
    }
    edges
}

/// The vertical offset of a row's center from the top of its table.
fn port_offset(row: Option<usize>) -> f32 {
    match row {
        Some(row) => HEADER_HEIGHT + ROW_HEIGHT * row as f32 + ROW_HEIGHT / 2.0,
        None => HEADER_HEIGHT / 2.0,
    }
}

/// Box width from the longest line — the name, or a column's name and type
/// side by side — so narrow tables pack tightly and wide ones do not
/// dominate the layout.
fn node_width(table: &ErdTable) -> f32 {
    let header = 32.0 + 7.6 * table.display_name().chars().count() as f32;
    let rows = table
        .columns
        .iter()
        .map(|column| {
            ROW_PADDING * 2.0
                + TAG_WIDTH
                + CHAR_WIDTH
                    * (column.name.chars().count() + column.data_type.chars().count()) as f32
                + 16.0
        })
        .fold(0.0_f32, f32::max);
    header.max(rows).clamp(MIN_NODE_WIDTH, MAX_NODE_WIDTH)
}

fn node_height(table: &ErdTable) -> f32 {
    HEADER_HEIGHT + ROW_HEIGHT * table.columns.len() as f32
}

/// One laid-out group of tables, in its own coordinates from (0, 0).
struct Block {
    members: Vec<usize>,
    /// The top-left of each member, in `members` order.
    positions: Vec<(f32, f32)>,
    /// The gap points each long edge passes through, left to right.
    waypoints: Vec<(usize, Vec<(f32, f32)>)>,
    width: f32,
    height: f32,
}

struct UnionFind(Vec<usize>);

impl UnionFind {
    fn new(len: usize) -> Self {
        Self((0..len).collect())
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.0[i] != i {
            self.0[i] = self.0[self.0[i]];
            i = self.0[i];
        }
        i
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        // The lower index wins, so a component's root is its first table.
        if a != b {
            let (low, high) = (a.min(b), a.max(b));
            self.0[high] = low;
        }
    }
}

/// One slot in a layer: a table or the gap a long edge passes through.
struct Vertex {
    layer: usize,
    height: f32,
    real: bool,
}

/// An edge piece between two adjacent layers, with the offsets of the rows
/// it attaches to from the top of each vertex.
struct Segment {
    left: usize,
    right: usize,
    left_port: f32,
    right_port: f32,
}

/// Lay out one connected component.
fn layered(group: &[usize], nodes: &[ErdNode], edges: &[ErdEdge]) -> Block {
    let n = group.len();
    let local: HashMap<usize, usize> = group.iter().enumerate().map(|(i, &g)| (g, i)).collect();

    // Arcs run from the referenced table to the referencing one, so a
    // parent lands to the left of its children.
    let arcs: Vec<(usize, usize, usize)> = edges
        .iter()
        .enumerate()
        .filter(|(_, edge)| edge.from != edge.to)
        .filter_map(|(e, edge)| Some((e, *local.get(&edge.to)?, *local.get(&edge.from)?)))
        .collect();

    let pairs: Vec<(usize, usize)> = arcs.iter().map(|&(_, a, b)| (a, b)).collect();
    let order = acyclic_order(n, &pairs);
    let mut position = vec![0; n];
    for (i, &v) in order.iter().enumerate() {
        position[v] = i;
    }
    // Every arc points forward in `order`: the few that ran backwards are
    // reversed, which is what breaks the cycles.
    let dag: Vec<(usize, usize, usize)> = arcs
        .iter()
        .map(|&(e, parent, child)| {
            if position[parent] < position[child] {
                (e, parent, child)
            } else {
                (e, child, parent)
            }
        })
        .collect();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(_, a, b) in &dag {
        succs[a].push(b);
        preds[b].push(a);
    }

    // Layering under a height budget that aims the component at `ASPECT`.
    let heights: Vec<f32> = group.iter().map(|&g| nodes[g].height).collect();
    let stacked: f32 = heights.iter().map(|h| h + NODE_GAP).sum();
    let mean_width = group.iter().map(|&g| nodes[g].width).sum::<f32>() / n as f32 + LAYER_GAP;
    let tallest = heights.iter().copied().fold(0.0, f32::max);
    let budget = (stacked * mean_width / ASPECT).sqrt().max(tallest);

    let mut layer = vec![0usize; n];
    let mut fill: Vec<f32> = Vec::new();
    for &v in &order {
        let lowest = preds[v].iter().map(|&p| layer[p] + 1).max().unwrap_or(0);
        let mut l = lowest;
        loop {
            if l >= fill.len() {
                fill.resize(l + 1, 0.0);
            }
            if fill[l] == 0.0 || fill[l] + heights[v] <= budget {
                break;
            }
            l += 1;
        }
        layer[v] = l;
        fill[l] += heights[v] + NODE_GAP;
    }
    // A table with more children than parents moves right, next to its
    // children, when there is room: a lookup table everything references
    // otherwise sits in the first column with long edges to the far end.
    for &v in order.iter().rev() {
        if succs[v].len() <= preds[v].len() {
            continue;
        }
        let highest = succs[v].iter().map(|&s| layer[s]).min().unwrap_or(0) - 1;
        if let Some(target) = (layer[v] + 1..=highest)
            .rev()
            .find(|&l| fill[l] + heights[v] <= budget)
        {
            fill[layer[v]] -= heights[v] + NODE_GAP;
            fill[target] += heights[v] + NODE_GAP;
            layer[v] = target;
        }
    }

    // Vertices: the tables first (by local index), then the dummy slots.
    let mut vertices: Vec<Vertex> = (0..n)
        .map(|v| Vertex {
            layer: layer[v],
            height: heights[v],
            real: true,
        })
        .collect();
    let mut segments: Vec<Segment> = Vec::new();
    let mut chains: Vec<(usize, Vec<usize>)> = Vec::new();
    for &(e, a, b) in &dag {
        let edge = &edges[e];
        let port = |v: usize| {
            if group[v] == edge.from {
                port_offset(edge.from_row)
            } else {
                port_offset(edge.to_row)
            }
        };
        let mut chain = vec![a];
        for l in layer[a] + 1..layer[b] {
            vertices.push(Vertex {
                layer: l,
                height: DUMMY_HEIGHT,
                real: false,
            });
            chain.push(vertices.len() - 1);
        }
        chain.push(b);
        for pair in chain.windows(2) {
            let port_of = |v: usize| if v < n { port(v) } else { DUMMY_HEIGHT / 2.0 };
            segments.push(Segment {
                left: pair[0],
                right: pair[1],
                left_port: port_of(pair[0]),
                right_port: port_of(pair[1]),
            });
        }
        chains.push((e, chain[1..chain.len() - 1].to_vec()));
    }

    let layer_count = vertices.iter().map(|v| v.layer + 1).max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for &v in &order {
        layers[layer[v]].push(v);
    }
    for v in n..vertices.len() {
        layers[vertices[v].layer].push(v);
    }

    // (neighbour, own port, neighbour's port) on each side.
    let mut left_of: Vec<Vec<(usize, f32, f32)>> = vec![Vec::new(); vertices.len()];
    let mut right_of: Vec<Vec<(usize, f32, f32)>> = vec![Vec::new(); vertices.len()];
    for s in &segments {
        right_of[s.left].push((s.right, s.left_port, s.right_port));
        left_of[s.right].push((s.left, s.right_port, s.left_port));
    }
    // Segments by the layer they leave, for counting crossings.
    let mut between: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for (i, s) in segments.iter().enumerate() {
        between[vertices[s.left].layer].push(i);
    }

    let mut ys = vec![0.0_f32; vertices.len()];
    for list in &layers {
        stack(list, &vertices, &mut ys);
    }

    // Ordering: barycenter sweeps, keeping the best order seen. The sweeps
    // settle into a local optimum that depends on where they start, so they
    // run from the initial order and from its mirror image.
    let mut best = layers.clone();
    let mut fewest = crossings(&between, &segments, &ys);
    let mirrored: Vec<Vec<usize>> = layers
        .iter()
        .map(|list| list.iter().rev().copied().collect())
        .collect();
    for start in [layers.clone(), mirrored] {
        layers = start;
        for list in &layers {
            stack(list, &vertices, &mut ys);
        }
        for sweep in 0..ORDER_SWEEPS {
            if fewest == 0 {
                break;
            }
            if sweep % 2 == 0 {
                for list in layers.iter_mut().skip(1) {
                    reorder(list, &left_of, &vertices, &mut ys);
                }
            } else {
                for list in layers.iter_mut().rev().skip(1) {
                    reorder(list, &right_of, &vertices, &mut ys);
                }
            }
            for list in layers.iter_mut() {
                transpose(list, &left_of, &right_of, &vertices, &mut ys);
            }
            let count = crossings(&between, &segments, &ys);
            if count < fewest {
                fewest = count;
                best = layers.clone();
            }
        }
    }
    layers = best;
    for list in &layers {
        stack(list, &vertices, &mut ys);
    }

    // Placement: pull each slot level with what it connects to.
    for pass in 0..PLACE_PASSES {
        let (left, right) = match pass {
            p if p + 2 >= PLACE_PASSES => (true, true),
            p if p % 2 == 0 => (true, false),
            _ => (false, true),
        };
        let sweep: Vec<usize> = if pass % 2 == 0 {
            (0..layer_count).collect()
        } else {
            (0..layer_count).rev().collect()
        };
        for l in sweep {
            let desired: Vec<f32> = layers[l]
                .iter()
                .map(|&v| {
                    let mut neighbours: Vec<&(usize, f32, f32)> = Vec::new();
                    if left {
                        neighbours.extend(&left_of[v]);
                    }
                    if right {
                        neighbours.extend(&right_of[v]);
                    }
                    if neighbours.is_empty() {
                        return ys[v];
                    }
                    neighbours
                        .iter()
                        .map(|&&(u, own, theirs)| ys[u] + theirs - own)
                        .sum::<f32>()
                        / neighbours.len() as f32
                })
                .collect();
            place(&layers[l], &desired, &vertices, &mut ys);
        }
    }

    let top = ys.iter().copied().fold(f32::INFINITY, f32::min);
    let top = if top.is_finite() { top } else { 0.0 };
    for y in &mut ys {
        *y -= top;
    }

    // Columns: each layer as wide as its widest table.
    let mut layer_x = vec![0.0_f32; layer_count];
    let mut layer_width = vec![0.0_f32; layer_count];
    for v in 0..n {
        layer_width[layer[v]] = layer_width[layer[v]].max(nodes[group[v]].width);
    }
    let mut x = 0.0;
    for l in 0..layer_count {
        layer_x[l] = x;
        x += layer_width[l] + LAYER_GAP;
    }

    let positions: Vec<(f32, f32)> = (0..n)
        .map(|v| {
            let l = layer[v];
            (
                layer_x[l] + (layer_width[l] - nodes[group[v]].width) / 2.0,
                ys[v],
            )
        })
        .collect();
    let waypoints = chains
        .into_iter()
        .map(|(e, dummies)| {
            let points = dummies
                .iter()
                .flat_map(|&d| {
                    let l = vertices[d].layer;
                    let y = ys[d] + DUMMY_HEIGHT / 2.0;
                    [(layer_x[l], y), (layer_x[l] + layer_width[l], y)]
                })
                .collect();
            (e, points)
        })
        .collect();
    let width = (0..n)
        .map(|v| positions[v].0 + nodes[group[v]].width)
        .fold(0.0, f32::max);
    let height = (0..n)
        .map(|v| positions[v].1 + heights[v])
        .fold(0.0, f32::max);

    Block {
        members: group.to_vec(),
        positions,
        waypoints,
        width,
        height,
    }
}

/// A vertex order with few arcs pointing backwards, by Eades, Lin and
/// Smyth's greedy heuristic: peel off sinks to the end and sources to the
/// front, and when neither is left, the vertex with the most outgoing arcs
/// over incoming ones to the front. A DAG comes back in topological order;
/// a cyclic graph keeps its feedback set small, which keeps the layering
/// shallow — a DFS order turns a tangle into long chains instead.
fn acyclic_order(n: usize, arcs: &[(usize, usize)]) -> Vec<usize> {
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut inc: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in arcs {
        out[a].push(b);
        inc[b].push(a);
    }
    let mut outdeg: Vec<usize> = out.iter().map(Vec::len).collect();
    let mut indeg: Vec<usize> = inc.iter().map(Vec::len).collect();
    let mut removed = vec![false; n];
    let (mut head, mut tail) = (Vec::with_capacity(n), Vec::new());

    let take = |v: usize, removed: &mut [bool], outdeg: &mut [usize], indeg: &mut [usize]| {
        removed[v] = true;
        for &w in &out[v] {
            indeg[w] = indeg[w].saturating_sub(1);
        }
        for &u in &inc[v] {
            outdeg[u] = outdeg[u].saturating_sub(1);
        }
    };
    let mut left = n;
    while left > 0 {
        while let Some(v) = (0..n).find(|&v| !removed[v] && outdeg[v] == 0) {
            take(v, &mut removed, &mut outdeg, &mut indeg);
            tail.push(v);
            left -= 1;
        }
        while let Some(v) = (0..n).find(|&v| !removed[v] && indeg[v] == 0) {
            take(v, &mut removed, &mut outdeg, &mut indeg);
            head.push(v);
            left -= 1;
        }
        if left == 0 {
            break;
        }
        let v = (0..n)
            .filter(|&v| !removed[v])
            .max_by_key(|&v| (outdeg[v] as isize - indeg[v] as isize, std::cmp::Reverse(v)))
            .expect("a vertex is left");
        take(v, &mut removed, &mut outdeg, &mut indeg);
        head.push(v);
        left -= 1;
    }
    tail.reverse();
    head.extend(tail);
    head
}

/// The minimum distance from the top of one slot to the top of the next.
fn spacing(above: &Vertex, below: &Vertex) -> f32 {
    let gap = if above.real && below.real {
        NODE_GAP
    } else {
        DUMMY_GAP
    };
    above.height + gap
}

/// Stack a layer tightly from zero, in order.
fn stack(list: &[usize], vertices: &[Vertex], ys: &mut [f32]) {
    let mut y = 0.0;
    for (i, &v) in list.iter().enumerate() {
        if i > 0 {
            y += spacing(&vertices[list[i - 1]], &vertices[v]);
        }
        ys[v] = y;
    }
}

/// Sort a layer by where its neighbours on one side want each slot's
/// center, then restack it. A slot with no neighbours there keeps its own
/// center, so it stays roughly where it was.
fn reorder(
    list: &mut [usize],
    neighbours: &[Vec<(usize, f32, f32)>],
    vertices: &[Vertex],
    ys: &mut [f32],
) {
    let key = |v: usize| -> f32 {
        let around = &neighbours[v];
        if around.is_empty() {
            return ys[v] + vertices[v].height / 2.0;
        }
        around
            .iter()
            .map(|&(u, own, theirs)| ys[u] + theirs - own)
            .sum::<f32>()
            / around.len() as f32
            + vertices[v].height / 2.0
    };
    let mut keyed: Vec<(f32, usize)> = list.iter().map(|&v| (key(v), v)).collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (slot, (_, v)) in list.iter_mut().zip(keyed) {
        *slot = v;
    }
    stack(list, vertices, ys);
}

/// Swap neighbouring slots in a layer while that removes crossings — the
/// local fix-up barycenter sorting leaves undone, since it moves each slot
/// by its own neighbours alone. Swapping two neighbours only changes how
/// their own segments cross each other, so that is all each test counts.
fn transpose(
    list: &mut [usize],
    left_of: &[Vec<(usize, f32, f32)>],
    right_of: &[Vec<(usize, f32, f32)>],
    vertices: &[Vertex],
    ys: &mut [f32],
) {
    for _ in 0..4 {
        let mut improved = false;
        for i in 0..list.len().saturating_sub(1) {
            let (upper, lower) = (list[i], list[i + 1]);
            let (mut kept, mut swapped) = (0, 0);
            for side in [left_of, right_of] {
                for &(a, _, a_port) in &side[upper] {
                    for &(b, _, b_port) in &side[lower] {
                        let (ya, yb) = (ys[a] + a_port, ys[b] + b_port);
                        if ya > yb {
                            kept += 1;
                        } else if ya < yb {
                            swapped += 1;
                        }
                    }
                }
            }
            if swapped < kept {
                list.swap(i, i + 1);
                stack(list, vertices, ys);
                improved = true;
            }
        }
        if !improved {
            return;
        }
    }
}

/// Place a layer as close to `desired` as its order and spacing allow: the
/// least-squares fit under "each slot at least `spacing` below the one
/// above", which is isotonic regression once each slot's minimum offset is
/// subtracted — solved exactly by pooling adjacent violators.
fn place(list: &[usize], desired: &[f32], vertices: &[Vertex], ys: &mut [f32]) {
    let mut offsets = Vec::with_capacity(list.len());
    let mut offset = 0.0;
    for (i, &v) in list.iter().enumerate() {
        if i > 0 {
            offset += spacing(&vertices[list[i - 1]], &vertices[v]);
        }
        offsets.push(offset);
    }
    // (sum, count) per pooled run.
    let mut pools: Vec<(f32, usize)> = Vec::new();
    for (d, o) in desired.iter().zip(&offsets) {
        pools.push((d - o, 1));
        while pools.len() >= 2 {
            let (s2, c2) = pools[pools.len() - 1];
            let (s1, c1) = pools[pools.len() - 2];
            if s1 / c1 as f32 <= s2 / c2 as f32 {
                break;
            }
            pools.pop();
            *pools.last_mut().expect("two pools") = (s1 + s2, c1 + c2);
        }
    }
    let mut i = 0;
    for (sum, count) in pools {
        let mean = sum / count as f32;
        for _ in 0..count {
            ys[list[i]] = mean + offsets[i];
            i += 1;
        }
    }
}

/// Edge crossings between every pair of adjacent layers, from where each
/// segment attaches.
fn crossings(between: &[Vec<usize>], segments: &[Segment], ys: &[f32]) -> u64 {
    between
        .iter()
        .map(|list| {
            let pairs: Vec<(f32, f32)> = list
                .iter()
                .map(|&i| {
                    let s = &segments[i];
                    (ys[s.left] + s.left_port, ys[s.right] + s.right_port)
                })
                .collect();
            count_crossings(pairs)
        })
        .sum()
}

/// How many of the straight segments from `a` on one line to `b` on a
/// parallel one cross: pairs whose order on one side is the reverse of the
/// other. Segments sharing an end point never cross. Merge-sort inversion
/// counting, so a busy layer costs `n log n` rather than `n²`.
pub(crate) fn count_crossings(mut pairs: Vec<(f32, f32)>) -> u64 {
    pairs.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.1.total_cmp(&q.1)));
    let mut ends: Vec<f32> = pairs.into_iter().map(|(_, b)| b).collect();
    let mut scratch = ends.clone();
    inversions(&mut ends, &mut scratch)
}

fn inversions(values: &mut [f32], scratch: &mut [f32]) -> u64 {
    let len = values.len();
    if len < 2 {
        return 0;
    }
    let mid = len / 2;
    let mut count = {
        let (left, right) = values.split_at_mut(mid);
        let (left_scratch, right_scratch) = scratch.split_at_mut(mid);
        inversions(left, left_scratch) + inversions(right, right_scratch)
    };
    let (mut i, mut j, mut k) = (0, mid, 0);
    while i < mid && j < len {
        if values[j] < values[i] {
            scratch[k] = values[j];
            count += (mid - i) as u64;
            j += 1;
        } else {
            scratch[k] = values[i];
            i += 1;
        }
        k += 1;
    }
    scratch[k..k + mid - i].copy_from_slice(&values[i..mid]);
    k += mid - i;
    scratch[k..k + len - j].copy_from_slice(&values[j..len]);
    values.copy_from_slice(&scratch[..len]);
    count
}

/// Pack rectangles into rows, left to right, wrapping at a width that aims
/// the whole at `ASPECT`. Returns each top-left and the overall size.
fn shelf_pack(sizes: &[(f32, f32)], gap: f32) -> (Vec<(f32, f32)>, f32, f32) {
    let area: f32 = sizes.iter().map(|(w, h)| (w + gap) * (h + gap)).sum();
    let widest = sizes.iter().map(|(w, _)| *w).fold(0.0, f32::max);
    let limit = (area * ASPECT).sqrt().max(widest);
    let (mut x, mut y, mut row_height) = (0.0_f32, 0.0_f32, 0.0_f32);
    let (mut width, mut height) = (0.0_f32, 0.0_f32);
    let mut positions = Vec::with_capacity(sizes.len());
    for &(w, h) in sizes {
        if x > 0.0 && x + w > limit {
            y += row_height + gap;
            x = 0.0;
            row_height = 0.0;
        }
        positions.push((x, y));
        width = width.max(x + w);
        height = height.max(y + h);
        x += w + gap;
        row_height = row_height.max(h);
    }
    (positions, width, height)
}

/// The route for one edge, from the referencing row to the referenced one.
/// `gaps` holds the dummy points left to right.
fn route(edge: &ErdEdge, nodes: &[ErdNode], gaps: &mut Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let (from, to) = (&nodes[edge.from], &nodes[edge.to]);
    let from_y = from.row_y(edge.from_row);
    let to_y = to.row_y(edge.to_row);
    if edge.from == edge.to {
        return vec![(from.x + from.width, from_y), (to.x + to.width, to_y)];
    }
    let (start, end) = if from.x + from.width <= to.x {
        ((from.x + from.width, from_y), (to.x, to_y))
    } else if to.x + to.width <= from.x {
        gaps.reverse();
        ((from.x, from_y), (to.x + to.width, to_y))
    } else {
        // Overlapping columns only happen across packed blocks, which no
        // edge joins; leave from the right of both rather than through one.
        ((from.x + from.width, from_y), (to.x + to.width, to_y))
    };
    let mut path = Vec::with_capacity(gaps.len() + 2);
    path.push(start);
    path.append(gaps);
    path.push(end);
    path
}

/// Translate the diagram so its bounding-box center sits at the origin.
fn center(nodes: &mut [ErdNode], edges: &mut [ErdEdge]) {
    if nodes.is_empty() {
        return;
    }
    let (min_x, min_y, max_x, max_y) = node_bounds(nodes);
    let (cx, cy) = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
    for node in nodes.iter_mut() {
        node.x -= cx;
        node.y -= cy;
    }
    for edge in edges.iter_mut() {
        for point in &mut edge.path {
            point.0 -= cx;
            point.1 -= cy;
        }
    }
}

fn node_bounds(nodes: &[ErdNode]) -> (f32, f32, f32, f32) {
    let mut bounds = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for node in nodes {
        bounds.0 = bounds.0.min(node.x);
        bounds.1 = bounds.1.min(node.y);
        bounds.2 = bounds.2.max(node.x + node.width);
        bounds.3 = bounds.3.max(node.y + node.height);
    }
    bounds
}

/// The diagram's bounding box, with `padding` around it.
pub fn bounds(diagram: &ErdDiagram, padding: f32) -> (f32, f32, f32, f32) {
    if diagram.nodes.is_empty() {
        return (-padding, -padding, padding, padding);
    }
    let (min_x, min_y, mut max_x, max_y) = node_bounds(&diagram.nodes);
    // A self-referencing edge loops out past its table's right side.
    for edge in diagram.edges.iter().filter(|edge| edge.from == edge.to) {
        let node = &diagram.nodes[edge.from];
        max_x = max_x.max(node.x + node.width + SELF_LOOP_REACH);
    }
    (
        min_x - padding,
        min_y - padding,
        max_x + padding,
        max_y + padding,
    )
}

/// Palette for the SVG, as hex colours. The view builds one from the app
/// theme; the export uses the same.
#[derive(Debug, Clone)]
pub struct ErdPalette {
    pub background: String,
    pub node_fill: String,
    pub header_fill: String,
    pub node_border: String,
    pub text: String,
    pub muted_text: String,
    pub edge: String,
}

/// Render the diagram as a standalone SVG document, used both for the
/// on-screen canvas and the export.
///
/// The viewBox is symmetric around the diagram's center (the origin): the
/// canvas scales the SVG around the element's own center, so this keeps the
/// diagram centered while panning and zooming.
pub fn diagram_svg(diagram: &ErdDiagram, palette: &ErdPalette) -> String {
    let (min_x, min_y, max_x, max_y) = bounds(diagram, 48.0);
    let half_w = min_x.abs().max(max_x.abs()).max(1.0);
    let half_h = min_y.abs().max(max_y.abs()).max(1.0);
    let (min_x, min_y, max_x, max_y) = (-half_w, -half_h, half_w, half_h);
    let width = max_x - min_x;
    let height = max_y - min_y;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" \
         viewBox=\"{min_x:.1} {min_y:.1} {width:.1} {height:.1}\">\n<title>Entity-relationship diagram</title>\n"
    ));
    svg.push_str(&format!(
        "<rect x=\"{min_x:.1}\" y=\"{min_y:.1}\" width=\"{width:.1}\" height=\"{height:.1}\" \
         fill=\"{bg}\"/>\n",
        bg = palette.background
    ));

    for edge in &diagram.edges {
        edge_svg(&mut svg, diagram, edge, palette);
    }

    // Which rows hold a referencing column, for the `FK` tag.
    let mut referencing: HashSet<(usize, &str)> = HashSet::new();
    for edge in &diagram.edges {
        for column in &edge.from_columns {
            referencing.insert((edge.from, column.as_str()));
        }
    }
    for (i, node) in diagram.nodes.iter().enumerate() {
        table_svg(&mut svg, i, node, &referencing, palette);
    }

    svg.push_str("</svg>\n");
    svg
}

fn edge_svg(svg: &mut String, diagram: &ErdDiagram, edge: &ErdEdge, palette: &ErdPalette) {
    let path = &edge.path;
    if path.len() < 2 {
        return;
    }
    let (from, to) = (&diagram.nodes[edge.from], &diagram.nodes[edge.to]);
    let title = format!(
        "{}: {}({}) → {}({})",
        edge.name,
        from.table.display_name(),
        edge.from_columns.join(", "),
        to.table.display_name(),
        edge.to_columns.join(", "),
    );

    let mut d = format!("M {:.1} {:.1}", path[0].0, path[0].1);
    if edge.from == edge.to {
        let ((x, y1), (_, y2)) = (path[0], path[1]);
        let reach = x + SELF_LOOP_REACH;
        d.push_str(&format!(
            " C {reach:.1} {y1:.1}, {reach:.1} {y2:.1}, {x:.1} {y2:.1}"
        ));
    } else {
        for pair in path.windows(2) {
            let ((x1, y1), (x2, y2)) = (pair[0], pair[1]);
            let half = (x2 - x1) / 2.0;
            d.push_str(&format!(
                " C {:.1} {y1:.1}, {:.1} {y2:.1}, {x2:.1} {y2:.1}",
                x1 + half,
                x2 - half
            ));
        }
    }

    // Crow's foot ("many") at the referencing end, a double bar ("exactly
    // one") at the referenced end, each facing out of its table.
    let outward = |at: (f32, f32), next: (f32, f32)| {
        if edge.from == edge.to || next.0 >= at.0 {
            1.0
        } else {
            -1.0
        }
    };
    let (start, end) = (path[0], path[path.len() - 1]);
    let s = outward(start, path[1]);
    let e = outward(end, path[path.len() - 2]);
    let marks = format!(
        "M {fx:.1} {sy:.1} L {sx:.1} {up:.1} M {fx:.1} {sy:.1} L {sx:.1} {down:.1} \
         M {b1:.1} {eup:.1} L {b1:.1} {edown:.1} M {b2:.1} {eup:.1} L {b2:.1} {edown:.1}",
        fx = start.0 + s * 12.0,
        sx = start.0,
        sy = start.1,
        up = start.1 - 6.0,
        down = start.1 + 6.0,
        b1 = end.0 + e * 6.0,
        b2 = end.0 + e * 10.0,
        eup = end.1 - 6.0,
        edown = end.1 + 6.0,
    );

    svg.push_str(&format!(
        "<g class=\"erd-edge\"><title>{title}</title>\
         <path d=\"{d}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.5\"/>\
         <path d=\"{marks}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.5\"/></g>\n",
        title = escape_xml(&title),
        color = palette.edge,
    ));
}

fn table_svg(
    svg: &mut String,
    index: usize,
    node: &ErdNode,
    referencing: &HashSet<(usize, &str)>,
    palette: &ErdPalette,
) {
    let (x, y, w, h) = (node.x, node.y, node.width, node.height);
    let name = node.table.display_name();
    let header_chars = ((w - 16.0) / 7.6).max(1.0) as usize;
    svg.push_str(&format!(
        "<g class=\"erd-table\"><title>{title}</title>\
         <rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"6\" fill=\"{fill}\"/>\
         <rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{HEADER_HEIGHT:.1}\" rx=\"6\" fill=\"{header}\"/>",
        title = escape_xml(&name),
        fill = palette.node_fill,
        header = palette.header_fill,
    ));
    if !node.table.columns.is_empty() {
        // Square off the header's lower corners where the rows begin.
        svg.push_str(&format!(
            "<rect x=\"{x:.1}\" y=\"{sy:.1}\" width=\"{w:.1}\" height=\"6\" fill=\"{header}\"/>\
             <line x1=\"{x:.1}\" y1=\"{ly:.1}\" x2=\"{x2:.1}\" y2=\"{ly:.1}\" stroke=\"{border}\"/>",
            sy = y + HEADER_HEIGHT - 6.0,
            ly = y + HEADER_HEIGHT,
            x2 = x + w,
            header = palette.header_fill,
            border = palette.node_border,
        ));
    }
    svg.push_str(&format!(
        "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"6\" fill=\"none\" \
         stroke=\"{border}\" stroke-width=\"1.5\"/>\
         <text x=\"{cx:.1}\" y=\"{cy:.1}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
         font-family=\"sans-serif\" font-size=\"13\" font-weight=\"bold\" fill=\"{text}\">{name}</text>",
        border = palette.node_border,
        cx = x + w / 2.0,
        cy = y + HEADER_HEIGHT / 2.0 + 1.0,
        text = palette.text,
        name = escape_xml(&truncate(&name, header_chars)),
    ));

    let available = w - ROW_PADDING * 2.0 - TAG_WIDTH - 12.0;
    for (row, column) in node.table.columns.iter().enumerate() {
        let cy = node.row_y(Some(row)) + 1.0;
        let foreign = referencing.contains(&(index, column.name.as_str()));
        let tag = match (column.primary_key, foreign) {
            (true, true) => "PF",
            (true, false) => "PK",
            (false, true) => "FK",
            (false, false) => "",
        };
        // The type gets what the name leaves, up to half the row.
        let type_chars = column.data_type.chars().count();
        let name_chars = column.name.chars().count();
        let total = (available / CHAR_WIDTH).max(2.0) as usize;
        let type_room = type_chars.min((total / 2).max(total.saturating_sub(name_chars)));
        let name_room = total.saturating_sub(type_room).max(1);
        if !tag.is_empty() {
            svg.push_str(&format!(
                "<text x=\"{tx:.1}\" y=\"{cy:.1}\" dominant-baseline=\"central\" font-family=\"sans-serif\" \
                 font-size=\"9\" font-weight=\"bold\" fill=\"{color}\">{tag}</text>",
                tx = x + ROW_PADDING,
                color = if column.primary_key {
                    &palette.text
                } else {
                    &palette.muted_text
                },
            ));
        }
        svg.push_str(&format!(
            "<text x=\"{nx:.1}\" y=\"{cy:.1}\" dominant-baseline=\"central\" font-family=\"sans-serif\" \
             font-size=\"12\" fill=\"{text}\">{name}</text>\
             <text x=\"{dx:.1}\" y=\"{cy:.1}\" text-anchor=\"end\" dominant-baseline=\"central\" \
             font-family=\"sans-serif\" font-size=\"11\" fill=\"{muted}\">{data_type}</text>",
            nx = x + ROW_PADDING + TAG_WIDTH,
            dx = x + w - ROW_PADDING,
            text = palette.text,
            muted = palette.muted_text,
            name = escape_xml(&truncate(&column.name, name_room)),
            data_type = escape_xml(&truncate(&column.data_type, type_room)),
        ));
    }
    svg.push_str("</g>\n");
}

/// Cut `text` to `max` characters, ending in an ellipsis when it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A comma-joined column list, as the foreign-key queries return one.
fn split_columns(list: Option<&str>) -> Vec<String> {
    list.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse the bulk foreign-key query both server engines share: one row per
/// constraint — local schema (nullable), local table, name, local columns,
/// referenced schema (nullable), referenced table, referenced columns,
/// `ON DELETE`, `ON UPDATE`.
pub(crate) fn parse_bulk_relations(result: &QueryResult) -> Vec<ErdRelation> {
    result
        .rows
        .iter()
        .enumerate()
        .map(|(position, row)| {
            let cell = |i: usize| row.get(i).and_then(|cell| cell.as_deref());
            ErdRelation {
                from: ErdTable {
                    schema: cell(0).map(str::to_string),
                    name: cell(1).unwrap_or_default().to_string(),
                    columns: Vec::new(),
                },
                to: ErdTable {
                    schema: cell(4).map(str::to_string),
                    name: cell(5).unwrap_or_default().to_string(),
                    columns: Vec::new(),
                },
                name: cell(2)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("fk_{}", position + 1)),
                from_columns: split_columns(cell(3)),
                to_columns: split_columns(cell(6)),
            }
        })
        .collect()
}

/// Parse one SQLite foreign-key listing (`sqlite::foreign_keys_sql`), whose
/// rows carry no local table — the caller supplies it.
pub(crate) fn parse_sqlite_relations(table: &str, result: &QueryResult) -> Vec<ErdRelation> {
    result
        .rows
        .iter()
        .enumerate()
        .map(|(position, row)| {
            let cell = |i: usize| row.get(i).and_then(|cell| cell.as_deref());
            ErdRelation {
                from: ErdTable {
                    schema: None,
                    name: table.to_string(),
                    columns: Vec::new(),
                },
                to: ErdTable {
                    schema: None,
                    name: cell(3).unwrap_or_default().to_string(),
                    columns: Vec::new(),
                },
                name: cell(0)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("fk_{}", position + 1)),
                from_columns: split_columns(cell(1)),
                to_columns: split_columns(cell(4)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, primary_key: bool) -> ErdColumn {
        ErdColumn {
            name: name.to_string(),
            data_type: "integer".to_string(),
            primary_key,
        }
    }

    /// A table with an `id` primary key and the named reference columns.
    fn table_with(name: &str, references: &[&str]) -> ErdTable {
        let mut columns = vec![column("id", true)];
        columns.extend(references.iter().map(|r| column(r, false)));
        columns.push(column("name", false));
        ErdTable {
            schema: None,
            name: name.to_string(),
            columns,
        }
    }

    fn table(name: &str) -> ErdTable {
        table_with(name, &[])
    }

    fn end(name: &str) -> ErdTable {
        ErdTable {
            schema: None,
            name: name.to_string(),
            columns: Vec::new(),
        }
    }

    fn relation(from: &str, to: &str) -> ErdRelation {
        relation_on(from, &format!("{to}_id"), to)
    }

    fn relation_on(from: &str, column: &str, to: &str) -> ErdRelation {
        ErdRelation {
            from: end(from),
            to: end(to),
            name: format!("fk_{from}_{column}"),
            from_columns: vec![column.to_string()],
            to_columns: vec!["id".to_string()],
        }
    }

    /// Edge crossings in the drawing, counted on the polylines between each
    /// path's points — the curves only bend within each such span.
    fn drawn_crossings(diagram: &ErdDiagram) -> usize {
        fn crosses(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> bool {
            let side = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
                (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0)
            };
            let (d1, d2) = (side(c, d, a), side(c, d, b));
            let (d3, d4) = (side(a, b, c), side(a, b, d));
            d1 * d2 < 0.0 && d3 * d4 < 0.0
        }
        let edges: Vec<&ErdEdge> = diagram.edges.iter().filter(|e| e.from != e.to).collect();
        let mut count = 0;
        for (i, a) in edges.iter().enumerate() {
            for b in &edges[i + 1..] {
                for p in a.path.windows(2) {
                    for q in b.path.windows(2) {
                        if crosses(p[0], p[1], q[0], q[1]) {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }

    fn overlaps(a: &ErdNode, b: &ErdNode) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    fn node<'a>(diagram: &'a ErdDiagram, name: &str) -> &'a ErdNode {
        diagram
            .nodes
            .iter()
            .find(|node| node.table.name == name)
            .expect("the table should be in the diagram")
    }

    #[test]
    fn layout_is_deterministic() {
        let tables = [
            table("users"),
            table_with("orders", &["users_id"]),
            table_with("items", &["orders_id", "products_id"]),
            table("products"),
        ]
        .to_vec();
        let relations = [
            relation("orders", "users"),
            relation("items", "orders"),
            relation("items", "products"),
        ]
        .to_vec();
        let first = layout_diagram(&tables, &relations);
        let second = layout_diagram(&tables, &relations);
        for (a, b) in first.nodes.iter().zip(second.nodes.iter()) {
            assert_eq!((a.x, a.y), (b.x, b.y), "positions must be reproducible");
        }
    }

    #[test]
    fn a_large_tangled_schema_lands_without_overlaps() {
        // A few hundred tables with a web of relations, cycles included,
        // the shape this has to stay efficient for.
        let mut state: u64 = 0x12345678;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        let mut relations = Vec::new();
        let mut references: Vec<Vec<String>> = vec![Vec::new(); 300];
        for (i, refs) in references.iter_mut().enumerate() {
            for k in 0..3 {
                let j = next() % 300;
                if i != j {
                    let column = format!("ref_{k}");
                    relations.push(relation_on(
                        &format!("table_{i:03}"),
                        &column,
                        &format!("table_{j:03}"),
                    ));
                    refs.push(column);
                }
            }
        }
        let tables: Vec<ErdTable> = references
            .iter()
            .enumerate()
            .map(|(i, refs)| {
                let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
                table_with(&format!("table_{i:03}"), &refs)
            })
            .collect();
        let diagram = layout_diagram(&tables, &relations);
        assert_eq!(diagram.nodes.len(), 300);
        for (i, a) in diagram.nodes.iter().enumerate() {
            assert!(a.x.is_finite() && a.y.is_finite());
            for b in &diagram.nodes[i + 1..] {
                assert!(
                    !overlaps(a, b),
                    "{} and {} overlap",
                    a.table.name,
                    b.table.name
                );
            }
        }
        for edge in &diagram.edges {
            assert!(edge.path.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
        }
    }

    #[test]
    fn a_shop_schema_is_drawn_without_crossings_and_parents_to_the_left() {
        let tables = [
            table("users"),
            table_with("orders", &["users_id"]),
            table_with("addresses", &["users_id"]),
            table_with("items", &["orders_id", "products_id"]),
            table("products"),
            table_with("payments", &["orders_id"]),
            table_with("reviews", &["products_id", "users_id"]),
        ]
        .to_vec();
        let relations = [
            relation("orders", "users"),
            relation("addresses", "users"),
            relation("items", "orders"),
            relation("items", "products"),
            relation("payments", "orders"),
            relation("reviews", "products"),
        ]
        .to_vec();
        let diagram = layout_diagram(&tables, &relations);
        assert_eq!(drawn_crossings(&diagram), 0);
        for edge in &diagram.edges {
            let (from, to) = (&diagram.nodes[edge.from], &diagram.nodes[edge.to]);
            assert!(
                to.x + to.width <= from.x,
                "{} should sit left of {}",
                to.table.name,
                from.table.name
            );
        }
    }

    #[test]
    fn crossings_are_counted_from_the_ends_order() {
        assert_eq!(count_crossings(vec![(0.0, 0.0), (1.0, 1.0)]), 0);
        assert_eq!(count_crossings(vec![(0.0, 1.0), (1.0, 0.0)]), 1);
        assert_eq!(count_crossings(vec![(0.0, 2.0), (1.0, 1.0), (2.0, 0.0)]), 3);
        // Sharing an end is not a crossing.
        assert_eq!(count_crossings(vec![(0.0, 1.0), (0.0, 0.0)]), 0);
    }

    #[test]
    fn unrelated_clusters_and_lone_tables_are_kept_apart() {
        let tables = [
            table("a"),
            table_with("b", &["a_id"]),
            table("lonely"),
            table("x"),
            table_with("y", &["x_id"]),
        ]
        .to_vec();
        let relations = [relation("b", "a"), relation("y", "x")].to_vec();
        let diagram = layout_diagram(&tables, &relations);
        assert_eq!(diagram.nodes.len(), 5);
        let cluster = |names: [&str; 2]| {
            let nodes = names.map(|name| node(&diagram, name));
            (
                nodes.iter().map(|n| n.x).fold(f32::INFINITY, f32::min),
                nodes.iter().map(|n| n.y).fold(f32::INFINITY, f32::min),
                nodes.iter().map(|n| n.x + n.width).fold(f32::MIN, f32::max),
                nodes
                    .iter()
                    .map(|n| n.y + n.height)
                    .fold(f32::MIN, f32::max),
            )
        };
        let (first, second) = (cluster(["a", "b"]), cluster(["x", "y"]));
        let apart = first.2 <= second.0
            || second.2 <= first.0
            || first.3 <= second.1
            || second.3 <= first.1;
        assert!(apart, "the clusters' boxes must not overlap");
        let lonely = node(&diagram, "lonely");
        for other in ["a", "b", "x", "y"] {
            assert!(!overlaps(lonely, node(&diagram, other)));
        }
    }

    #[test]
    fn edges_attach_to_the_rows_of_their_columns() {
        let tables = [table("authors"), table_with("books", &["author_id"])].to_vec();
        let diagram = layout_diagram(&tables, &[relation_on("books", "author_id", "authors")]);
        let edge = &diagram.edges[0];
        let (books, authors) = (node(&diagram, "books"), node(&diagram, "authors"));
        assert_eq!((edge.from_row, edge.to_row), (Some(1), Some(0)));
        let (start, end) = (edge.path[0], edge.path[edge.path.len() - 1]);
        assert_eq!(start, (books.x, books.row_y(Some(1))));
        assert_eq!(end, (authors.x + authors.width, authors.row_y(Some(0))));
        // Placement levels the two rows, so the edge runs straight across.
        assert!((start.1 - end.1).abs() < 0.5, "{start:?} {end:?}");
    }

    #[test]
    fn an_implicit_reference_resolves_to_the_primary_key() {
        let tables = [
            ErdTable {
                schema: None,
                name: "codes".to_string(),
                columns: vec![column("label", false), column("code", true)],
            },
            table_with("things", &["code"]),
        ]
        .to_vec();
        let mut implicit = relation_on("things", "code", "codes");
        implicit.to_columns.clear();
        let diagram = layout_diagram(&tables, &[implicit]);
        assert_eq!(diagram.edges[0].to_columns, ["code"]);
        assert_eq!(diagram.edges[0].to_row, Some(1));
    }

    #[test]
    fn relations_to_unknown_tables_are_dropped_and_duplicates_merged() {
        let tables = [table_with("a", &["b_id", "other_id"]), table("b")].to_vec();
        let relations = [
            relation("a", "b"),
            relation("a", "b"),
            relation_on("a", "other_id", "b"),
            relation("a", "missing"),
            relation("missing", "a"),
        ]
        .to_vec();
        let diagram = layout_diagram(&tables, &relations);
        assert_eq!(
            diagram.edges.len(),
            2,
            "two keys between one pair are two edges"
        );
        assert_eq!((diagram.edges[0].from, diagram.edges[0].to), (0, 1));
    }

    #[test]
    fn self_relations_are_drawn_on_the_table_itself() {
        let tables = [table_with("a", &["parent_id"])].to_vec();
        let diagram = layout_diagram(&tables, &[relation_on("a", "parent_id", "a")]);
        assert_eq!(diagram.edges.len(), 1);
        let a = &diagram.nodes[0];
        assert_eq!(
            diagram.edges[0].path,
            [
                (a.x + a.width, a.row_y(Some(1))),
                (a.x + a.width, a.row_y(Some(0)))
            ]
        );
    }

    #[test]
    fn empty_input_stays_empty() {
        let diagram = layout_diagram(&[], &[]);
        assert!(diagram.nodes.is_empty());
        assert!(diagram.edges.is_empty());
        let svg = diagram_svg(&diagram, &test_palette());
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>\n"));
    }

    #[test]
    fn svg_escapes_names_and_tags_keys_in_words() {
        let mut child = table_with("c&d", &["a_id"]);
        child.columns.push(ErdColumn {
            name: "x<y".to_string(),
            data_type: "varchar(<10>)".to_string(),
            primary_key: false,
        });
        let tables = [table("a<b"), child].to_vec();
        let mut fk = relation_on("c&d", "a_id", "a<b");
        fk.name = "fk_\"q\"".to_string();
        let diagram = layout_diagram(&tables, &[fk]);
        let svg = diagram_svg(&diagram, &test_palette());
        assert!(svg.contains("a&lt;b"));
        assert!(svg.contains("c&amp;d"));
        assert!(svg.contains("x&lt;y"));
        assert!(svg.contains("varchar(&lt;10&gt;)"));
        assert!(svg.contains("fk_&quot;q&quot;"));
        assert!(!svg.contains("a<b"), "raw < must not reach the XML");
        assert_eq!(svg.matches("<g class=\"erd-table\">").count(), 2);
        assert_eq!(svg.matches("<g class=\"erd-edge\">").count(), 1);
        assert_eq!(svg.matches(">PK</text>").count(), 2);
        assert_eq!(svg.matches(">FK</text>").count(), 1);
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a_very_long_name", 6), "a_ver…");
    }

    #[test]
    fn parsers_keep_the_column_lists() {
        let cell = |s: &str| Some(s.to_string());
        let bulk = QueryResult {
            rows: vec![vec![
                None,
                cell("items"),
                cell("fk_items"),
                cell("order_id,line"),
                None,
                cell("order_lines"),
                cell("order_id,line"),
                cell("CASCADE"),
                cell("NO ACTION"),
            ]],
            ..QueryResult::default()
        };
        let parsed = parse_bulk_relations(&bulk);
        assert_eq!(parsed[0].from_columns, ["order_id", "line"]);
        assert_eq!(parsed[0].to_columns, ["order_id", "line"]);

        let sqlite = QueryResult {
            rows: vec![vec![
                None,
                cell("code"),
                None,
                cell("codes"),
                None,
                cell("NO ACTION"),
                cell("NO ACTION"),
            ]],
            ..QueryResult::default()
        };
        let parsed = parse_sqlite_relations("things", &sqlite);
        assert_eq!(parsed[0].from.name, "things");
        assert_eq!(parsed[0].from_columns, ["code"]);
        assert!(parsed[0].to_columns.is_empty());
    }

    fn test_palette() -> ErdPalette {
        ErdPalette {
            background: "#ffffff".to_string(),
            node_fill: "#f5f5f5".to_string(),
            header_fill: "#e5e5e5".to_string(),
            node_border: "#888888".to_string(),
            text: "#111111".to_string(),
            muted_text: "#666666".to_string(),
            edge: "#555555".to_string(),
        }
    }
}
