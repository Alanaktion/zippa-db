//! ER diagram model: the tables and their foreign-key relations as a graph,
//! a deterministic force-directed layout, and an SVG renderer shared by the
//! on-screen canvas and the export.
//!
//! The layout runs fdg-sim's Fruchterman-Reingold implementation.
//! `Simulation::from_graph` places nodes at random, so the initial positions
//! are overwritten on a deterministic circle before the first step — the
//! update itself uses no randomness, which keeps the layout (and its tests)
//! reproducible. A short box de-overlap pass follows, since the simulation
//! is point-based and table boxes would otherwise stack their labels.

use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;

use fdg_sim::glam::Vec3;
use fdg_sim::{ForceGraph, ForceGraphHelper, Simulation, SimulationParameters};

use super::query::QueryResult;

/// A table participating in the diagram.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ErdTable {
    /// Schema on engines that have them; `None` for MySQL/SQLite.
    pub schema: Option<String>,
    pub name: String,
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
}

/// One foreign-key relation: `from` references `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErdRelation {
    pub from: ErdTable,
    pub to: ErdTable,
    /// The constraint's name, for the SVG title.
    pub name: String,
}

/// One laid-out table: its box, centered on (`x`, `y`) in world units.
#[derive(Debug, Clone)]
pub struct ErdNode {
    pub table: ErdTable,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One drawn relation, by node index.
#[derive(Debug, Clone)]
pub struct ErdEdge {
    pub from: usize,
    pub to: usize,
    pub name: String,
}

/// The laid-out diagram: node boxes plus the relations between them.
#[derive(Debug, Clone, Default)]
pub struct ErdDiagram {
    pub nodes: Vec<ErdNode>,
    pub edges: Vec<ErdEdge>,
}

/// Simulation steps and timestep. The timestep is the stability bound, not
/// a tuning knob: `fdg-sim`'s explicit Euler integration of the
/// Fruchterman–Reingold forces diverges on a few-hundred-node schema at
/// `dt = 0.035` (positions blow up to NaN within ~15 steps), while
/// `dt <= 0.03` stays finite and settles to the same spread, so `0.02`
/// keeps a margin. Steps are fixed so the layout stays deterministic.
const SIM_STEPS: usize = 500;
const SIM_DT: f32 = 0.02;

/// Box geometry in world units.
const NODE_HEIGHT: f32 = 36.0;
const MIN_NODE_WIDTH: f32 = 72.0;
const MAX_NODE_WIDTH: f32 = 260.0;

/// Lay out `tables` and their `relations`, deterministically: the same input
/// always produces the same positions. Relations naming a table that is not
/// listed are dropped, as are duplicate relations between one pair (the
/// first name wins).
pub fn layout_diagram(tables: &[ErdTable], relations: &[ErdRelation]) -> ErdDiagram {
    let index: HashMap<(String, String), usize> = tables
        .iter()
        .enumerate()
        .map(|(i, table)| (table.key(), i))
        .collect();

    let mut edges = Vec::new();
    let mut sim_pairs: Vec<(usize, usize)> = Vec::new();
    let mut seen = HashSet::new();
    for relation in relations {
        let (Some(&from), Some(&to)) = (
            index.get(&relation.from.key()),
            index.get(&relation.to.key()),
        ) else {
            continue;
        };
        if !seen.insert((from, to)) {
            continue;
        }
        edges.push(ErdEdge {
            from,
            to,
            name: relation.name.clone(),
        });
        // A self-loop contributes nothing to the forces (and risks a
        // divide-by-zero in the repulsion); it is still drawn.
        if from != to {
            sim_pairs.push((from, to));
        }
    }

    let positions = simulate(tables.len(), &sim_pairs);

    let mut nodes: Vec<ErdNode> = tables
        .iter()
        .enumerate()
        .map(|(i, table)| {
            let (x, y) = positions[i];
            ErdNode {
                table: table.clone(),
                x,
                y,
                width: node_width(&table.display_name()),
                height: NODE_HEIGHT,
            }
        })
        .collect();

    resolve_overlaps(&mut nodes);
    center(&mut nodes);

    ErdDiagram { nodes, edges }
}

/// Box width from the label, so short names pack tightly and long ones do
/// not dominate the layout.
fn node_width(display_name: &str) -> f32 {
    let width = 24.0 + 7.4 * display_name.chars().count() as f32;
    width.clamp(MIN_NODE_WIDTH, MAX_NODE_WIDTH)
}

/// Run the force-directed simulation with deterministic initial positions.
fn simulate(node_count: usize, pairs: &[(usize, usize)]) -> Vec<(f32, f32)> {
    if node_count == 0 {
        return Vec::new();
    }
    let mut graph: ForceGraph<(), ()> = ForceGraph::default();
    let ids: Vec<_> = (0..node_count)
        .map(|i| graph.add_force_node(format!("n{i}"), ()))
        .collect();
    for (a, b) in pairs {
        graph.add_edge(ids[*a], ids[*b], ());
    }

    let mut simulation = Simulation::from_graph(graph, SimulationParameters::default());

    // `from_graph` placed every node at random; overwrite with a circle so
    // the run — and therefore the diagram — is reproducible. The circle
    // doubles as the fallback below.
    let radius = 150.0 * (node_count as f32).sqrt();
    let starts: Vec<(f32, f32)> = (0..node_count)
        .map(|i| {
            let angle = 2.0 * PI * i as f32 / node_count as f32;
            (radius * angle.cos(), radius * angle.sin())
        })
        .collect();
    {
        let graph = simulation.get_graph_mut();
        let indices: Vec<_> = graph.node_indices().collect();
        for (i, idx) in indices.into_iter().enumerate() {
            let (x, y) = starts[i];
            let location = Vec3::new(x, y, 0.0);
            let node = &mut graph[idx];
            node.location = location;
            node.old_location = location;
            node.velocity = Vec3::ZERO;
        }
    }

    for _ in 0..SIM_STEPS {
        simulation.update(SIM_DT);
    }

    // A divergent run poisons positions to NaN/infinity (the repulsion is
    // singular at zero distance); those nodes fall back to their circle
    // position so every diagram still has finite coordinates.
    simulation
        .get_graph()
        .node_weights()
        .enumerate()
        .map(|(i, node)| {
            let (x, y) = (node.location.x, node.location.y);
            if x.is_finite() && y.is_finite() {
                (x, y)
            } else {
                starts[i]
            }
        })
        .collect()
}

/// Push overlapping boxes apart, along the axis of least overlap. The
/// simulation treats nodes as points, so without this the labels of a dense
/// schema stack on top of each other.
fn resolve_overlaps(nodes: &mut [ErdNode]) {
    const PADDING: f32 = 14.0;
    for _ in 0..80 {
        let mut moved = false;
        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                let (left, right) = nodes.split_at_mut(j);
                let a = &mut left[i];
                let b = &mut right[0];
                let dx = b.x - a.x;
                let dy = b.y - a.y;
                let overlap_x = (a.width + b.width) / 2.0 + PADDING - dx.abs();
                let overlap_y = (a.height + b.height) / 2.0 + PADDING - dy.abs();
                if overlap_x > 0.0 && overlap_y > 0.0 {
                    moved = true;
                    if overlap_x < overlap_y {
                        let sign = if dx == 0.0 { 1.0 } else { dx.signum() };
                        let push = overlap_x / 2.0 * sign;
                        a.x -= push;
                        b.x += push;
                    } else {
                        let sign = if dy == 0.0 { 1.0 } else { dy.signum() };
                        let push = overlap_y / 2.0 * sign;
                        a.y -= push;
                        b.y += push;
                    }
                }
            }
        }
        if !moved {
            break;
        }
    }
}

/// Translate the diagram so its bounding-box center sits at the origin.
fn center(nodes: &mut [ErdNode]) {
    if nodes.is_empty() {
        return;
    }
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for node in nodes.iter() {
        min_x = min_x.min(node.x - node.width / 2.0);
        max_x = max_x.max(node.x + node.width / 2.0);
        min_y = min_y.min(node.y - node.height / 2.0);
        max_y = max_y.max(node.y + node.height / 2.0);
    }
    let (cx, cy) = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
    for node in nodes.iter_mut() {
        node.x -= cx;
        node.y -= cy;
    }
}

/// The diagram's bounding box, with `padding` around it.
pub fn bounds(diagram: &ErdDiagram, padding: f32) -> (f32, f32, f32, f32) {
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for node in &diagram.nodes {
        min_x = min_x.min(node.x - node.width / 2.0);
        max_x = max_x.max(node.x + node.width / 2.0);
        min_y = min_y.min(node.y - node.height / 2.0);
        max_y = max_y.max(node.y + node.height / 2.0);
    }
    if diagram.nodes.is_empty() {
        return (-padding, -padding, padding, padding);
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
    pub node_border: String,
    pub text: String,
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
        "<defs><marker id=\"erd-arrow\" viewBox=\"0 0 10 10\" refX=\"8\" refY=\"5\" \
         markerWidth=\"7\" markerHeight=\"7\" orient=\"auto-start-reverse\">\
         <path d=\"M 0 1 L 9 5 L 0 9 z\" fill=\"{edge}\"/></marker></defs>\n",
        edge = palette.edge
    ));
    svg.push_str(&format!(
        "<rect x=\"{min_x:.1}\" y=\"{min_y:.1}\" width=\"{width:.1}\" height=\"{height:.1}\" \
         fill=\"{bg}\"/>\n",
        bg = palette.background
    ));

    for edge in &diagram.edges {
        let (from, to) = (&diagram.nodes[edge.from], &diagram.nodes[edge.to]);
        let title = format!("<title>{}</title>", escape_xml(&edge.name));
        if edge.from == edge.to {
            svg.push_str(&format!(
                "<path d=\"M {x1:.1} {y1:.1} C {x1:.1} {y2:.1}, {x2:.1} {y2:.1}, {x2:.1} {y1:.1}\" \
                 fill=\"none\" stroke=\"{e}\" stroke-width=\"1.5\" marker-end=\"url(#erd-arrow)\">{title}</path>\n",
                x1 = from.x - 16.0,
                y1 = from.y - from.height / 2.0,
                x2 = from.x + 16.0,
                y2 = from.y - from.height / 2.0 - 30.0,
                e = palette.edge,
                title = title,
            ));
            continue;
        }
        let (x1, y1, x2, y2) = clipped_endpoints(from, to);
        svg.push_str(&format!(
            "<line x1=\"{x1:.1}\" y1=\"{y1:.1}\" x2=\"{x2:.1}\" y2=\"{y2:.1}\" \
             stroke=\"{e}\" stroke-width=\"1.5\" marker-end=\"url(#erd-arrow)\">{title}</line>\n",
            e = palette.edge,
            title = title,
        ));
    }

    for node in &diagram.nodes {
        let name = escape_xml(&node.table.display_name());
        svg.push_str(&format!(
            "<g><rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"6\" \
             fill=\"{fill}\" stroke=\"{border}\" stroke-width=\"1.5\"/>\
             <text x=\"{cx:.1}\" y=\"{cy:.1}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
             font-family=\"sans-serif\" font-size=\"13\" fill=\"{text}\">{name}</text></g>\n",
            x = node.x - node.width / 2.0,
            y = node.y - node.height / 2.0,
            w = node.width,
            h = node.height,
            cx = node.x,
            cy = node.y + 1.0,
            fill = palette.node_fill,
            border = palette.node_border,
            text = palette.text,
            name = name,
        ));
    }

    svg.push_str("</svg>\n");
    svg
}

/// The segment between two node centers, clipped to each box's border.
fn clipped_endpoints(from: &ErdNode, to: &ErdNode) -> (f32, f32, f32, f32) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let dist = (dx * dx + dy * dy).sqrt().max(1.0);
    let (ux, uy) = (dx / dist, dy / dist);
    let clip = |node: &ErdNode, sx: f32, sy: f32| {
        let tx = if sx != 0.0 {
            node.width / 2.0 / sx.abs()
        } else {
            f32::INFINITY
        };
        let ty = if sy != 0.0 {
            node.height / 2.0 / sy.abs()
        } else {
            f32::INFINITY
        };
        let t = tx.min(ty);
        (node.x + sx * t, node.y + sy * t)
    };
    let (x1, y1) = clip(from, ux, uy);
    let (x2, y2) = clip(to, -ux, -uy);
    (x1, y1, x2, y2)
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
                },
                to: ErdTable {
                    schema: cell(4).map(str::to_string),
                    name: cell(5).unwrap_or_default().to_string(),
                },
                name: cell(2)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("fk_{}", position + 1)),
            }
        })
        .collect()
}

/// Parse one SQLite `pragma_foreign_key_list` result, whose rows carry no
/// local table — the caller supplies it.
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
                },
                to: ErdTable {
                    schema: None,
                    name: cell(3).unwrap_or_default().to_string(),
                },
                name: cell(0)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("fk_{}", position + 1)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> ErdTable {
        ErdTable {
            schema: None,
            name: name.to_string(),
        }
    }

    fn relation(from: &str, to: &str) -> ErdRelation {
        ErdRelation {
            from: table(from),
            to: table(to),
            name: format!("fk_{from}_{to}"),
        }
    }

    #[test]
    fn layout_is_deterministic() {
        let tables = ["users", "orders", "items", "products"].map(table).to_vec();
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
    fn layout_places_every_node_at_finite_coordinates() {
        // A few hundred tables with a web of relations, the shape this has
        // to stay efficient for.
        let tables: Vec<ErdTable> = (0..300).map(|i| table(&format!("table_{i:03}"))).collect();
        let mut relations = Vec::new();
        // A simple deterministic pseudo-random generator (LCG).
        let mut state: u64 = 0x12345678;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        for i in 0..300 {
            for _ in 0..3 {
                let j = next() % 300;
                if i != j {
                    relations.push(relation(&format!("table_{i:03}"), &format!("table_{j:03}")));
                }
            }
        }
        let diagram = layout_diagram(&tables, &relations);
        assert_eq!(diagram.nodes.len(), 300);
        for node in &diagram.nodes {
            assert!(
                node.x.is_finite() && node.y.is_finite(),
                "every node must land somewhere real"
            );
        }
    }

    #[test]
    fn relations_to_unknown_tables_are_dropped_and_duplicates_merged() {
        let tables = [table("a"), table("b")].to_vec();
        let relations = [
            relation("a", "b"),
            relation("a", "b"),
            relation("a", "missing"),
            relation("missing", "a"),
        ]
        .to_vec();
        let diagram = layout_diagram(&tables, &relations);
        assert_eq!(diagram.edges.len(), 1);
        assert_eq!((diagram.edges[0].from, diagram.edges[0].to), (0, 1));
    }

    #[test]
    fn self_relations_are_drawn_but_not_simulated() {
        let tables = [table("a")].to_vec();
        let diagram = layout_diagram(&tables, &[relation("a", "a")]);
        assert_eq!(diagram.edges.len(), 1);
        assert!(diagram.nodes[0].x.is_finite());
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
    fn svg_escapes_names_and_draws_every_node() {
        let tables = [table("a<b"), table("c&d")].to_vec();
        let diagram = layout_diagram(&tables, &[relation("a<b", "c&d")]);
        let svg = diagram_svg(&diagram, &test_palette());
        assert!(svg.contains("a&lt;b"));
        assert!(svg.contains("c&amp;d"));
        assert!(!svg.contains("a<b</text>"), "raw < must not reach the XML");
        assert_eq!(svg.matches("<g><rect").count(), 2);
        assert_eq!(svg.matches("<line ").count(), 1);
        assert!(svg.contains("marker-end=\"url(#erd-arrow)\""));
    }

    fn test_palette() -> ErdPalette {
        ErdPalette {
            background: "#ffffff".to_string(),
            node_fill: "#f5f5f5".to_string(),
            node_border: "#888888".to_string(),
            text: "#111111".to_string(),
            edge: "#555555".to_string(),
        }
    }
}
