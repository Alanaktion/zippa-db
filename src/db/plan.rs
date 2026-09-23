//! Reading a server's `EXPLAIN` output into a tree.
//!
//! Each engine spells a plan differently — Postgres answers with JSON, SQLite
//! with `(id, parent, detail)` rows, MySQL with an indented text tree — so
//! there is one parser per engine here behind the shared [`PlanNode`] shape.
//! The parsers are pure: they take the server's text or rows and answer with a
//! tree, or `None` when they cannot read it. A `None` never hides the plan; the
//! caller falls back to [`Plan::raw`], which shows the server's own output with
//! a note that it could not be read.
//!
//! The warning heuristics are deliberately few, and every threshold is a
//! constant here so a test can pin it.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use super::query::QueryResult;

/// A row estimate this far off the actual rows is worth mentioning.
const ROW_ESTIMATE_FACTOR: f64 = 10.0;
/// A sequential scan at or above this many estimated rows, with a filter, is
/// worth mentioning.
const LARGE_SCAN_ROWS: f64 = 10_000.0;
/// An inner scan run this many times is what makes a nested loop expensive.
const MANY_LOOPS: u64 = 1_000;
/// A filter that throws away this many times more rows than it returns.
const FILTER_WASTE_FACTOR: f64 = 10.0;

/// One step of a query plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PlanNode {
    /// `Seq Scan on orders`, `SEARCH items USING INDEX idx_name`.
    pub label: String,
    /// `Filter`, `Index Cond`, `Sort Key`, buffer counts, … in a fixed order.
    pub details: Vec<(String, String)>,
    pub estimated_rows: Option<f64>,
    pub actual_rows: Option<f64>,
    /// Startup and total cost, as the server reports them.
    pub cost: Option<(f64, f64)>,
    /// Per-loop total time, exactly as Postgres reports `Actual Total Time`.
    pub actual_ms: Option<f64>,
    pub loops: Option<u64>,
    pub children: Vec<PlanNode>,
    /// Heuristics that a reader could act on, in words.
    pub warnings: Vec<String>,
}

impl PlanNode {
    /// Everything this node took, loops included.
    pub fn total_ms(&self) -> Option<f64> {
        self.actual_ms.map(|ms| ms * self.loops.unwrap_or(1) as f64)
    }

    /// The time this node spent itself: its own total minus what its children
    /// report, so a slow step can be told from a slow subtree.
    pub fn exclusive_ms(&self) -> Option<f64> {
        let total = self.total_ms()?;
        let children: f64 = self.children.iter().filter_map(PlanNode::total_ms).sum();
        Some((total - children).max(0.0))
    }

    /// Self time as a share of `total`, for the viewer's bar.
    pub fn exclusive_share(&self, total: f64) -> Option<f64> {
        if total <= 0.0 {
            return None;
        }
        self.exclusive_ms().map(|ms| (ms / total).clamp(0.0, 1.0))
    }
}

/// One statement's plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub root: PlanNode,
    /// When the server reported it (`EXPLAIN ANALYZE`).
    pub planning_ms: Option<f64>,
    pub execution_ms: Option<f64>,
    /// Whether the statement was run to produce the plan.
    pub analyzed: bool,
    /// Wall-clock of the `EXPLAIN` statement itself.
    #[serde(skip)]
    pub elapsed: Duration,
    /// Whether `root` was read from the server's own format. False means only
    /// `raw` could be shown, as when a MySQL tree could not be parsed.
    #[serde(skip)]
    pub parsed: bool,
    /// The server's own output, kept for `Copy plan`.
    #[serde(skip)]
    pub raw: String,
}

impl Plan {
    /// A plan whose format could not be read: the raw output alone.
    pub fn raw(text: impl Into<String>) -> Self {
        let raw = text.into();
        Self {
            root: PlanNode {
                label: raw.clone(),
                ..PlanNode::default()
            },
            planning_ms: None,
            execution_ms: None,
            analyzed: false,
            elapsed: Duration::ZERO,
            parsed: false,
            raw,
        }
    }

    /// Total time the root reports, which every share is relative to.
    pub fn total_ms(&self) -> Option<f64> {
        self.root.total_ms()
    }

    /// Every warning in the tree, with the label of the node it belongs to, in
    /// tree order.
    pub fn warnings(&self) -> Vec<(String, String)> {
        let mut collected = Vec::new();
        collect_warnings(&self.root, &mut collected);
        collected
    }

    /// The plan as JSON, for the viewer's `Copy JSON`.
    pub fn json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| self.raw.clone())
    }
}

fn collect_warnings(node: &PlanNode, collected: &mut Vec<(String, String)>) {
    for warning in &node.warnings {
        collected.push((node.label.clone(), warning.clone()));
    }
    for child in &node.children {
        collect_warnings(child, collected);
    }
}

/// The outcome of an `EXPLAIN`, before the caller decides how to show it.
#[derive(Debug)]
pub enum Explained {
    /// A tree the plan viewer can draw.
    Plan(Plan),
    /// The server's own tabular output, which the ordinary grid shows as it
    /// would any result. MySQL older than the tree format answers this way.
    Rows(QueryResult),
}

/// Read Postgres' `EXPLAIN (FORMAT JSON)` output.
///
/// `None` when the text is not the shape Postgres documents — a plan the user
/// typed with `FORMAT TEXT`, say — so the caller can show the raw output.
pub fn postgres(text: &str, analyze: bool) -> Option<Plan> {
    let value: Value = serde_json::from_str(text).ok()?;
    let document = value.as_array()?.first()?;
    let root = document.get("Plan")?;
    let analyzed = analyze || document.get("Execution Time").is_some();

    Some(Plan {
        root: postgres_node(root),
        planning_ms: document.get("Planning Time").and_then(Value::as_f64),
        execution_ms: document.get("Execution Time").and_then(Value::as_f64),
        analyzed,
        elapsed: Duration::ZERO,
        parsed: true,
        raw: text.to_string(),
    })
}

/// The node types Postgres reports in the JSON plan, with the fields that say
/// what a reader wants to know about them.
const POSTGRES_DETAILS: [&str; 18] = [
    "Filter",
    "Index Cond",
    "Recheck Cond",
    "Hash Cond",
    "Merge Cond",
    "Join Filter",
    "Sort Key",
    "Sort Method",
    "Group Key",
    "Hash",
    "Rows Removed by Filter",
    "Workers Planned",
    "Workers Launched",
    "Heap Fetches",
    "Shared Hit Blocks",
    "Shared Read Blocks",
    "Temp Read Blocks",
    "Temp Written Blocks",
];

fn postgres_node(value: &Value) -> PlanNode {
    let node_type = text_of(value, "Node Type").unwrap_or_else(|| "Plan".to_string());
    let mut label = node_type.clone();

    // `Index Scan using idx_items_name on items`.
    if let Some(index) = text_of(value, "Index Name") {
        label.push_str(&format!(" using {index}"));
    }
    match text_of(value, "Relation Name") {
        Some(relation) => {
            label.push_str(&format!(" on {relation}"));
            // Only name an alias when it adds to the relation it renames.
            if let Some(alias) = text_of(value, "Alias")
                && alias != relation
            {
                label.push_str(&format!(" {alias}"));
            }
        }
        // A CTE or a work table names itself rather than a relation.
        None => {
            if (node_type.contains("CTE Scan") || node_type.contains("WorkTable Scan"))
                && let Some(alias) = text_of(value, "Alias")
            {
                label.push_str(&format!(" on {alias}"));
            }
        }
    }

    let mut details = Vec::new();
    for key in POSTGRES_DETAILS {
        if let Some(field) = value.get(key) {
            details.push((key.to_string(), json_text(field)));
        }
    }

    let children = value
        .get("Plans")
        .and_then(Value::as_array)
        .map(|plans| plans.iter().map(postgres_node).collect())
        .unwrap_or_default();

    let mut node = PlanNode {
        label,
        details,
        estimated_rows: number_of(value, "Plan Rows"),
        actual_rows: number_of(value, "Actual Rows"),
        cost: match (
            number_of(value, "Startup Cost"),
            number_of(value, "Total Cost"),
        ) {
            (Some(startup), Some(total)) => Some((startup, total)),
            _ => None,
        },
        actual_ms: number_of(value, "Actual Total Time"),
        loops: value.get("Actual Loops").and_then(Value::as_u64),
        children,
        warnings: Vec::new(),
    };
    node.warnings = postgres_warnings(value, &node);
    node
}

fn postgres_warnings(value: &Value, node: &PlanNode) -> Vec<String> {
    let mut warnings = Vec::new();

    // A sequential scan over a large table that then throws rows away is the
    // classic missing-index case.
    let filtered = value.get("Filter").is_some();
    if node.label.starts_with("Seq Scan")
        && filtered
        && let Some(rows) = node.estimated_rows
        && rows >= LARGE_SCAN_ROWS
    {
        warnings.push(format!(
            "Sequential scan of about {} rows with a filter; an index on the filtered column may help.",
            compact_rows(rows)
        ));
    }

    // Estimator trouble: the plan was built on a guess far from reality.
    if let (Some(estimated), Some(actual)) = (node.estimated_rows, node.actual_rows)
        && estimated > 0.0
        && actual > 0.0
    {
        let factor = (estimated / actual).max(actual / estimated);
        if factor >= ROW_ESTIMATE_FACTOR {
            warnings.push(format!(
                "Row estimate is off by about {:.0}× ({} estimated, {} actual); the statistics may be stale.",
                factor,
                compact_rows(estimated),
                compact_rows(actual)
            ));
        }
    }

    // A sort that ran out of memory.
    if let Some((_, method)) = node.details.iter().find(|(key, _)| key == "Sort Method")
        && method.starts_with("external")
    {
        warnings.push("Sort spilled to disk; raising work_mem may speed this up.".to_string());
    }

    // A nested loop whose inner side is a scan repeats that scan per row.
    if node.label.starts_with("Nested Loop")
        && let Some(inner) = node
            .children
            .iter()
            .filter(|child| child.label.starts_with("Seq Scan"))
            .filter_map(|child| child.loops)
            .max()
        && inner >= MANY_LOOPS
    {
        warnings.push(format!(
            "Nested loop runs its inner scan {inner} times; an index or a hash join may be faster."
        ));
    }

    // A filter that discards far more than it keeps is usually an index
    // waiting to happen, even when the estimate looked small.
    if let Some(removed) = value.get("Rows Removed by Filter").and_then(Value::as_f64)
        && let Some(actual) = node.actual_rows
        && removed >= actual.max(1.0) * FILTER_WASTE_FACTOR
    {
        warnings.push(format!(
            "Filter discarded {} rows to return {}; an index may make this selective.",
            compact_rows(removed),
            compact_rows(actual)
        ));
    }

    warnings
}

fn text_of(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn number_of(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

/// A JSON field as it reads in the details panel: a string is itself, an array
/// is its items joined, a number is written plainly.
fn json_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(json_text).collect::<Vec<_>>().join(", "),
        Value::Null => "NULL".to_string(),
        other => other.to_string(),
    }
}

/// Build SQLite's `EXPLAIN QUERY PLAN` tree from its
/// `(id, parent, notused, detail)` rows. Never fails: output that does not have
/// that shape comes back as raw.
pub fn sqlite(result: &QueryResult) -> Plan {
    let raw = raw_rows(result);

    let mut by_id: HashMap<i64, PlanNode> = HashMap::new();
    let mut parent_of: HashMap<i64, i64> = HashMap::new();
    let mut order: Vec<i64> = Vec::new();
    for row in &result.rows {
        if row.len() != 4 {
            return Plan::raw(raw);
        }
        let (Some(id), Some(parent)) = (
            row.first()
                .and_then(|cell| cell.as_deref())
                .and_then(as_i64),
            row.get(1).and_then(|cell| cell.as_deref()).and_then(as_i64),
        ) else {
            return Plan::raw(raw);
        };
        let detail = row.get(3).cloned().flatten().unwrap_or_default();
        if by_id
            .insert(
                id,
                PlanNode {
                    label: detail,
                    ..PlanNode::default()
                },
            )
            .is_some()
        {
            // A repeated id means this is not the shape we know.
            return Plan::raw(raw);
        }
        parent_of.insert(id, parent);
        order.push(id);
    }

    // A node whose parent is not another node is a root; SQLite's top-level
    // steps point at the implicit `0`.
    let roots: Vec<i64> = order
        .iter()
        .copied()
        .filter(|id| !by_id.contains_key(&parent_of[id]))
        .collect();

    let mut root = match roots.as_slice() {
        [only] => sqlite_node(*only, &by_id, &parent_of, &order),
        [] => return Plan::raw(raw),
        _ => PlanNode {
            label: "Query plan".to_string(),
            children: roots
                .iter()
                .map(|id| sqlite_node(*id, &by_id, &parent_of, &order))
                .collect(),
            ..PlanNode::default()
        },
    };
    annotate_sqlite(&mut root);

    Plan {
        root,
        planning_ms: None,
        execution_ms: None,
        analyzed: false,
        elapsed: Duration::ZERO,
        parsed: true,
        raw,
    }
}

fn sqlite_node(
    id: i64,
    by_id: &HashMap<i64, PlanNode>,
    parent_of: &HashMap<i64, i64>,
    order: &[i64],
) -> PlanNode {
    let mut node = by_id.get(&id).cloned().unwrap_or_default();
    node.children = order
        .iter()
        .copied()
        .filter(|child| parent_of.get(child) == Some(&id))
        .map(|child| sqlite_node(child, by_id, parent_of, order))
        .collect();
    node
}

fn annotate_sqlite(node: &mut PlanNode) {
    // `SCAN items` is SQLite's full table scan; `SEARCH items USING INDEX …`
    // is the indexed read it wants instead.
    if node.label.starts_with("SCAN") {
        node.warnings.push(
            "Full table scan; an index on the searched column may speed this up.".to_string(),
        );
    }
    if node.label.contains("USE TEMP B-TREE") {
        node.warnings
            .push("A temporary B-tree was built; an index may avoid the extra work.".to_string());
    }
    for child in &mut node.children {
        annotate_sqlite(child);
    }
}

/// Read MySQL's `EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE` indented text.
///
/// `None` when no line looks like a tree step, which is how older MySQL and
/// MariaDB are told apart from a parse failure: the caller falls back to the
/// classic tabular output.
pub fn mysql(text: &str, analyze: bool) -> Option<Plan> {
    let mut roots: Vec<PlanNode> = Vec::new();
    // The indentation of each open level, and where its node lives.
    let mut stack: Vec<(usize, Path)> = Vec::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let Some(node) = mysql_node(line) else {
            continue;
        };

        while stack.last().is_some_and(|(level, _)| *level >= indent) {
            stack.pop();
        }

        match stack.last() {
            Some((_, path)) => {
                if let Some(parent) = node_at_path(&mut roots, path) {
                    parent.children.push(node);
                    let index = parent.children.len() - 1;
                    let mut child_path = path.clone();
                    child_path.push(index);
                    stack.push((indent, child_path));
                }
            }
            None => {
                let index = roots.len();
                roots.push(node);
                stack.push((indent, vec![index]));
            }
        }
    }

    if roots.is_empty() {
        return None;
    }

    let mut root = match roots.len() {
        1 => roots.remove(0),
        _ => PlanNode {
            label: "Query plan".to_string(),
            children: std::mem::take(&mut roots),
            ..PlanNode::default()
        },
    };
    annotate_mysql(&mut root);
    // `EXPLAIN ANALYZE` is the only form that reports actual times, so a tree
    // that has them was analyzed even when the caller did not ask for it.
    let analyzed = analyze || has_timing(&root);

    Some(Plan {
        root,
        planning_ms: None,
        execution_ms: None,
        analyzed,
        elapsed: Duration::ZERO,
        parsed: true,
        raw: text.to_string(),
    })
}

/// Whether any step in a tree reported an actual time.
fn has_timing(node: &PlanNode) -> bool {
    node.actual_ms.is_some() || node.children.iter().any(has_timing)
}

/// A path to a node, as indices from the root.
type Path = Vec<usize>;

fn node_at_path<'a>(roots: &'a mut [PlanNode], path: &Path) -> Option<&'a mut PlanNode> {
    let (first, rest) = path.split_first()?;
    let mut node = roots.get_mut(*first)?;
    for index in rest {
        node = node.children.get_mut(*index)?;
    }
    Some(node)
}

/// One `->`-introduced line of MySQL's tree, or `None` for a line that is not
/// a step.
fn mysql_node(line: &str) -> Option<PlanNode> {
    let body = line.trim_start().strip_prefix("->")?.trim();

    // A step reads `Label  (cost=0.35 rows=2) (actual time=… rows=… loops=…)`;
    // either group can be missing.
    let cost_at = body.find("(cost=");
    let actual_at = body.find("(actual time=");
    let header_end = [cost_at, actual_at]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(body.len());
    let label = body[..header_end].trim().to_string();
    if label.is_empty() {
        return None;
    }

    // Each group is read only up to where the next one starts, so a group that
    // omits `rows=` cannot pick up the other group's value. Either can be
    // missing, and the cost group normally comes first.
    let group = |at: Option<usize>, next: Option<usize>| -> &str {
        match at {
            Some(at) => {
                let end = next.filter(|next| *next > at).unwrap_or(body.len());
                &body[at..end]
            }
            None => "",
        }
    };
    let cost_text = group(cost_at, actual_at);
    let actual_text = group(actual_at, cost_at);

    Some(PlanNode {
        label,
        details: Vec::new(),
        estimated_rows: mysql_value(cost_text, "rows="),
        actual_rows: mysql_value(actual_text, "rows="),
        cost: mysql_cost(cost_text),
        actual_ms: mysql_pair(actual_text, "actual time=").map(|(_, total)| total),
        loops: mysql_value(actual_text, "loops=").map(|loops| loops as u64),
        children: Vec::new(),
        warnings: Vec::new(),
    })
}

fn annotate_mysql(node: &mut PlanNode) {
    if node.label.contains("Table scan") {
        node.warnings
            .push("Full table scan; an index may speed this up.".to_string());
    }
    for child in &mut node.children {
        annotate_mysql(child);
    }
}

/// `cost=0.35..2.10` as `(0.35, 2.10)`, or MySQL's single `cost=0.55` as
/// `(0.55, 0.55)`.
fn mysql_cost(text: &str) -> Option<(f64, f64)> {
    let start = text.find("cost=")? + "cost=".len();
    let value: String = text[start..]
        .chars()
        .take_while(|character| character.is_ascii_digit() || matches!(character, '.' | '-' | '+'))
        .collect();
    match value.split_once("..") {
        Some((from, to)) => Some((from.parse().ok()?, to.parse().ok()?)),
        None => {
            let single: f64 = value.parse().ok()?;
            Some((single, single))
        }
    }
}

/// `actual time=0.041..0.055` as `(0.041, 0.055)`.
fn mysql_pair(text: &str, key: &str) -> Option<(f64, f64)> {
    let start = text.find(key)? + key.len();
    let value: String = text[start..]
        .chars()
        .take_while(|character| character.is_ascii_digit() || matches!(character, '.' | '-' | '+'))
        .collect();
    let (from, to) = value.split_once("..")?;
    Some((from.parse().ok()?, to.parse().ok()?))
}

/// `rows=2` as `2.0`.
fn mysql_value(text: &str, key: &str) -> Option<f64> {
    let start = text.find(key)? + key.len();
    let value: String = text[start..]
        .chars()
        .take_while(|character| character.is_ascii_digit() || matches!(character, '.' | '-'))
        .collect();
    value.parse().ok()
}

/// The rows as text, for the raw fallback and for `Copy plan`.
pub fn raw_rows(result: &QueryResult) -> String {
    result
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| cell.clone().unwrap_or_else(|| "NULL".to_string()))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A whole number reads without a decimal; anything else keeps two places.
pub fn compact_rows(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

fn as_i64(text: &str) -> Option<i64> {
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A nested plan with actual timings, a filter, a large seq scan, and an
    /// external sort — everything the warnings look for.
    const ANALYZED: &str = r#"[
      {
        "Plan": {
          "Node Type": "Sort",
          "Startup Cost": 100.00,
          "Total Cost": 200.00,
          "Plan Rows": 5,
          "Sort Key": ["o.created_at"],
          "Sort Method": "external merge",
          "Actual Startup Time": 4.0,
          "Actual Total Time": 12.0,
          "Actual Rows": 5,
          "Actual Loops": 1,
          "Plans": [
            {
              "Node Type": "Seq Scan",
              "Relation Name": "orders",
              "Alias": "o",
              "Startup Cost": 0.00,
              "Total Cost": 179.00,
              "Plan Rows": 10000,
              "Filter": "(o.total > 100)",
              "Rows Removed by Filter": 995,
              "Actual Startup Time": 0.02,
              "Actual Total Time": 4.0,
              "Actual Rows": 5,
              "Actual Loops": 1,
              "Shared Hit Blocks": 123
            }
          ]
        },
        "Planning Time": 0.4,
        "Execution Time": 12.8
      }
    ]"#;

    fn labels(node: &PlanNode) -> Vec<String> {
        let mut all = vec![node.label.clone()];
        for child in &node.children {
            all.extend(labels(child));
        }
        all
    }

    #[test]
    fn postgres_json_becomes_a_tree() {
        let plan = postgres(ANALYZED, true).expect("the fixture is a JSON plan");

        assert_eq!(plan.root.label, "Sort");
        assert_eq!(plan.root.children.len(), 1);
        assert_eq!(plan.root.children[0].label, "Seq Scan on orders o");
        assert_eq!(plan.root.children[0].estimated_rows, Some(10_000.0));
        assert_eq!(plan.root.children[0].actual_rows, Some(5.0));
        assert_eq!(plan.root.children[0].cost, Some((0.0, 179.0)));
        assert_eq!(plan.planning_ms, Some(0.4));
        assert_eq!(plan.execution_ms, Some(12.8));
        assert!(plan.analyzed);
        // A detail is passed through, and an array is joined.
        assert_eq!(
            plan.root
                .details
                .iter()
                .find(|(key, _)| key == "Sort Method"),
            Some(&("Sort Method".to_string(), "external merge".to_string()))
        );
        assert_eq!(
            plan.root.details.iter().find(|(key, _)| key == "Sort Key"),
            Some(&("Sort Key".to_string(), "o.created_at".to_string()))
        );
    }

    #[test]
    fn a_nodes_own_time_is_its_total_minus_its_children() {
        let plan = postgres(ANALYZED, true).unwrap();

        // The sort took 12 ms and the scan under it took 4 ms of that.
        assert_eq!(plan.root.total_ms(), Some(12.0));
        assert_eq!(plan.root.exclusive_ms(), Some(8.0));
        assert_eq!(plan.root.exclusive_share(12.0), Some(8.0 / 12.0));

        // A scan run three times reports its time per loop, so the total is
        // the time that matters.
        let repeated = PlanNode {
            actual_ms: Some(2.0),
            loops: Some(3),
            ..PlanNode::default()
        };
        assert_eq!(repeated.total_ms(), Some(6.0));
    }

    #[test]
    fn postgres_warnings_name_what_could_be_better() {
        let plan = postgres(ANALYZED, true).unwrap();
        let warnings = plan.warnings();

        assert!(
            warnings
                .iter()
                .any(|(node, warning)| node == "Seq Scan on orders o"
                    && warning.contains("Sequential scan")),
            "a large filtered seq scan should warn: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|(_, warning)| warning.contains("spilled to disk")),
            "an external sort should warn: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|(node, warning)| node == "Seq Scan on orders o"
                    && warning.contains("estimate is off")),
            "an estimate 2000× out should warn: {warnings:?}"
        );
    }

    #[test]
    fn a_trustworthy_plan_says_nothing() {
        let text = r#"[{"Plan": {"Node Type": "Index Scan", "Relation Name": "items",
            "Index Name": "items_pkey", "Startup Cost": 0.29, "Total Cost": 8.31,
            "Plan Rows": 1, "Actual Rows": 1, "Actual Total Time": 0.05, "Actual Loops": 1}}]"#;
        let plan = postgres(text, true).unwrap();

        assert_eq!(plan.root.label, "Index Scan using items_pkey on items");
        assert!(plan.warnings().is_empty());
    }

    #[test]
    fn text_output_is_not_json() {
        // A user who typed `FORMAT TEXT` gets the plan shown raw rather than
        // an empty tree.
        assert!(postgres("Seq Scan on orders  (cost=0.00..179.00 rows=10000)", false).is_none());
        assert!(!Plan::raw("Seq Scan on orders").parsed);
    }

    fn rows(rows: Vec<Vec<Option<&str>>>) -> QueryResult {
        QueryResult {
            columns: vec![
                "id".into(),
                "parent".into(),
                "notused".into(),
                "detail".into(),
            ],
            rows: rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|cell| cell.map(str::to_string))
                        .collect()
                })
                .collect(),
            ..QueryResult::default()
        }
    }

    #[test]
    fn sqlite_rows_build_the_tree_from_parent_ids() {
        let result = rows(vec![
            vec![Some("2"), Some("0"), Some("0"), Some("SCAN items")],
            vec![
                Some("6"),
                Some("2"),
                Some("0"),
                Some("SEARCH orders USING INDEX idx"),
            ],
            vec![
                Some("8"),
                Some("6"),
                Some("0"),
                Some("USE TEMP B-TREE FOR ORDER BY"),
            ],
        ]);
        let plan = sqlite(&result);

        assert!(plan.parsed);
        assert_eq!(plan.root.label, "SCAN items");
        assert_eq!(plan.root.children.len(), 1);
        assert_eq!(plan.root.children[0].label, "SEARCH orders USING INDEX idx");
        assert_eq!(
            plan.root.children[0].children[0].label,
            "USE TEMP B-TREE FOR ORDER BY"
        );
        assert_eq!(labels(&plan.root).len(), 3);
    }

    #[test]
    fn sqlite_warnings_name_a_scan_and_a_temp_tree() {
        let result = rows(vec![
            vec![Some("2"), Some("0"), Some("0"), Some("SCAN items")],
            vec![
                Some("8"),
                Some("2"),
                Some("0"),
                Some("USE TEMP B-TREE FOR ORDER BY"),
            ],
        ]);
        let warnings = sqlite(&result).warnings();

        assert!(
            warnings
                .iter()
                .any(|(_, warning)| warning.contains("Full table scan")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|(_, warning)| warning.contains("temporary B-tree")),
            "{warnings:?}"
        );
    }

    #[test]
    fn sqlite_output_of_another_shape_is_shown_raw() {
        let result = rows(vec![vec![Some("2"), Some("0")]]);
        assert!(!sqlite(&result).parsed);
    }

    const MYSQL_TREE: &str = "-> Sort: items.score DESC  (cost=0.55 rows=2)  (actual time=0.055..0.055 rows=2 loops=1)\n    -> Table scan on items  (cost=0.35 rows=2)  (actual time=0.040..0.040 rows=2 loops=1)\n";

    #[test]
    fn mysql_tree_text_parses_by_indentation() {
        let plan = mysql(MYSQL_TREE, true).expect("the fixture is a tree");

        assert!(plan.analyzed);
        assert_eq!(plan.root.label, "Sort: items.score DESC");
        assert_eq!(plan.root.estimated_rows, Some(2.0));
        assert_eq!(plan.root.actual_rows, Some(2.0));
        assert_eq!(plan.root.actual_ms, Some(0.055));
        assert_eq!(plan.root.loops, Some(1));
        assert_eq!(plan.root.cost, Some((0.55, 0.55)));
        assert_eq!(plan.root.children.len(), 1);
        assert_eq!(plan.root.children[0].label, "Table scan on items");
        assert!(
            plan.warnings()
                .iter()
                .any(|(_, warning)| warning.contains("Full table scan")),
            "a table scan should warn"
        );
    }

    #[test]
    fn mysql_actual_rows_are_not_read_as_the_estimate() {
        // The estimate group may omit `rows=`, which must not let the actual
        // group's value stand in for it.
        let text =
            "-> Filter: (items.id > 1)  (cost=0.35)  (actual time=0.05..0.05 rows=7 loops=1)\n";
        let plan = mysql(text, true).expect("the fixture is a tree");
        assert_eq!(plan.root.estimated_rows, None, "there is no estimate");
        assert_eq!(plan.root.actual_rows, Some(7.0));
    }

    #[test]
    fn a_classic_explain_is_not_a_tree() {
        // The tabular form has no `->` lines, so the caller falls back.
        assert!(mysql("id | select_type | table\n1 | SIMPLE | items", false).is_none());
    }

    #[test]
    fn a_plan_serializes_for_copy() {
        let plan = postgres(ANALYZED, true).unwrap();
        let json = plan.json();
        assert!(json.contains("\"label\""));
        assert!(json.contains("Seq Scan on orders o"));
    }
}
