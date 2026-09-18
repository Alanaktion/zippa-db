//! The query plan viewer: an `EXPLAIN` result as a readable tree.
//!
//! The tree rows use `gpui-kit`'s `Tree`, as the sidebar's object list does, so
//! arrow-key navigation and the ARIA tree roles come from the component. Each
//! row lays the engine's numbers out in fixed columns beside the step, and a
//! node whose timing matters carries a bar showing its own share of the total
//! time — time a step spent itself, its children's time taken out, which is
//! what points at the step to fix.
//!
//! Warnings are written in words in a block at the top and marked on their step
//! with an icon, so colour is never the only thing carrying them.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tree::{TreeEntry, TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClipboardItem, Context, Entity, IntoElement, MouseButton, Render, SharedString, Window,
    div, px,
};

use gpui_kit::assets::IconName;

use crate::db::plan::compact_rows;
use crate::db::{Plan, PlanNode};
use crate::settings;

/// Starting expansion: plans deeper than this open collapsed, so a plan with
/// hundreds of steps shows its shape rather than its whole bulk.
const AUTO_EXPAND_DEPTH: usize = 6;
/// Width of a numbers column. Fixed so the header and every row line up.
const NUMBER_WIDTH: f32 = 78.;
const COST_WIDTH: f32 = 96.;
const TIME_WIDTH: f32 = 78.;
const SHARE_WIDTH: f32 = 92.;
const BAR_WIDTH: f32 = 54.;
const INDENT: f32 = 14.;

/// What one row needs to draw its columns, keyed by tree id — the node's path
/// from the root.
struct NodeFacts {
    estimated_rows: Option<f64>,
    actual_rows: Option<f64>,
    cost: Option<f64>,
    total_ms: Option<f64>,
    /// Time this node spent itself, as a share of the whole plan.
    share: Option<f64>,
    warnings: usize,
}

#[derive(Clone, Copy)]
enum Expansion {
    /// Everything, as `Expand all` asks.
    All,
    /// Nothing.
    None,
    /// Everything above [`AUTO_EXPAND_DEPTH`].
    Auto,
}

pub struct PlanView {
    plan: Option<Plan>,
    tree: Entity<TreeState>,
    nodes: HashMap<SharedString, NodeFacts>,
}

impl PlanView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            plan: None,
            tree: cx.new(|cx| TreeState::new(cx)),
            nodes: HashMap::new(),
        }
    }

    /// Show `plan`, opening its upper levels.
    pub fn set_plan(&mut self, plan: Plan, cx: &mut Context<Self>) {
        self.plan = Some(plan);
        self.refresh_tree(Expansion::Auto, cx);
    }

    /// Put the keyboard on the tree, so its arrow keys work straight away.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.tree.update(cx, |tree, cx| tree.focus(window, cx));
    }

    fn refresh_tree(&mut self, expansion: Expansion, cx: &mut Context<Self>) {
        let Some(plan) = &self.plan else {
            return;
        };
        let total = plan.total_ms();
        let mut nodes = HashMap::new();
        let root = tree_item(&plan.root, "0", 0, total, expansion, &mut nodes);
        self.nodes = nodes;
        self.tree
            .update(cx, |tree, cx| tree.set_items(vec![root], cx));
        cx.notify();
    }

    fn expand_all(&mut self, _: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh_tree(Expansion::All, cx);
    }

    fn collapse_all(&mut self, _: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh_tree(Expansion::None, cx);
    }

    fn copy_plan(&mut self, _: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(plan) = &self.plan {
            cx.write_to_clipboard(ClipboardItem::new_string(plan.raw.clone()));
        }
    }

    fn copy_json(&mut self, _: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(plan) = &self.plan {
            cx.write_to_clipboard(ClipboardItem::new_string(plan.json()));
        }
    }

    /// One tree row: its step, then the columns the engine reported.
    fn row(
        &self,
        entry: &TreeEntry,
        cx: &mut Context<Self>,
    ) -> gpui_kit::component::list::ListItem {
        use gpui_kit::component::list::ListItem;

        let label = entry.item().label.clone();
        let id = entry.item().id.clone();
        let facts = self.nodes.get(&id);

        let chevron = match (entry.is_folder(), entry.is_expanded()) {
            (true, true) => Some(IconName::ChevronDown),
            (true, false) => Some(IconName::ChevronRight),
            (false, _) => None,
        };
        let warnings = facts.map(|facts| facts.warnings).unwrap_or(0);
        let mono = settings::grid_font(cx);

        let number = |value: Option<f64>| -> gpui_kit::AnyElement {
            let text = value.map(compact_rows).unwrap_or_else(|| "—".to_string());
            div()
                .w(px(NUMBER_WIDTH))
                .flex_none()
                .text_right()
                .font_family(mono.clone())
                .when(value.is_none(), |this| {
                    this.text_color(cx.theme().muted_foreground)
                })
                .child(text)
                .into_any_element()
        };

        let cost = div()
            .w(px(COST_WIDTH))
            .flex_none()
            .text_right()
            .font_family(mono.clone())
            .child(match facts.and_then(|facts| facts.cost) {
                Some(total) => format!("{total:.2}"),
                None => "—".to_string(),
            })
            .into_any_element();

        let time = div()
            .w(px(TIME_WIDTH))
            .flex_none()
            .text_right()
            .font_family(mono.clone())
            .child(match facts.and_then(|facts| facts.total_ms) {
                Some(ms) => format!("{ms:.2} ms"),
                None => "—".to_string(),
            })
            .into_any_element();

        let share = match facts.and_then(|facts| facts.share) {
            Some(share) => h_flex()
                .w(px(SHARE_WIDTH))
                .flex_none()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .w(px(BAR_WIDTH))
                        .h(px(6.))
                        .rounded_full()
                        .bg(cx.theme().muted)
                        .child(
                            div()
                                .h_full()
                                .w(px(BAR_WIDTH * share as f32))
                                .rounded_full()
                                .bg(cx.theme().primary),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .text_right()
                        .font_family(mono.clone())
                        .child(format!("{:.0}%", share * 100.0)),
                )
                .into_any_element(),
            None => div().w(px(SHARE_WIDTH)).flex_none().into_any_element(),
        };

        ListItem::new(SharedString::from(id.to_string()))
            .h(px(26.))
            .px_2()
            .py_0()
            .text_sm()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .items_center()
                            .child(
                                div()
                                    .w(px(INDENT))
                                    .flex_none()
                                    .text_color(cx.theme().muted_foreground)
                                    .when_some(chevron, |this, chevron| {
                                        this.child(Icon::new(chevron).xsmall())
                                    }),
                            )
                            .child(div().w(px(entry.depth() as f32 * INDENT)).flex_none())
                            .when(warnings > 0, |this| {
                                this.child(
                                    Icon::new(IconName::TriangleAlert)
                                        .xsmall()
                                        .text_color(cx.theme().warning),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(label)),
                    )
                    .child(number(facts.and_then(|facts| facts.estimated_rows)))
                    .child(number(facts.and_then(|facts| facts.actual_rows)))
                    .child(cost)
                    .child(time)
                    .child(share),
            )
    }

    /// `Planning 0.4 ms · Execution 12.8 ms · analyzed · 7 steps`.
    fn summary(&self) -> String {
        let Some(plan) = &self.plan else {
            return String::new();
        };
        let mut parts = Vec::new();
        if let Some(planning) = plan.planning_ms {
            parts.push(format!("Planning {planning:.2} ms"));
        }
        if let Some(execution) = plan.execution_ms {
            parts.push(format!("Execution {execution:.2} ms"));
        }
        if plan.analyzed {
            parts.push("analyzed".to_string());
        }
        let steps = steps(&plan.root);
        parts.push(format!("{steps} step{}", if steps == 1 { "" } else { "s" }));
        parts.join(" · ")
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let warnings = self.plan.as_ref().map(Plan::warnings).unwrap_or_default();
        let count = warnings.len();

        h_flex()
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.summary()),
                    )
                    // The warnings themselves live in a dialog, so a long list
                    // never pushes the tree off the pane; the button is the way
                    // in, and the step that raised each one is still marked.
                    .when(count > 0, |this| {
                        this.child(
                            Button::new("plan-warnings")
                                .ghost()
                                .xsmall()
                                .icon(IconName::TriangleAlert)
                                .label(format!(
                                    "{count} warning{}",
                                    if count == 1 { "" } else { "s" }
                                ))
                                .text_color(cx.theme().warning)
                                .tooltip("Show what the plan might do better")
                                .on_click(cx.listener(Self::show_warnings)),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Button::new("plan-expand-all")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronsUpDown)
                            .accessibility_label("Expand every step")
                            .tooltip("Expand every step")
                            .on_click(cx.listener(Self::expand_all)),
                    )
                    .child(
                        Button::new("plan-collapse-all")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronsDownUp)
                            .accessibility_label("Collapse every step")
                            .tooltip("Collapse every step")
                            .on_click(cx.listener(Self::collapse_all)),
                    )
                    .child(
                        Button::new("plan-copy")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .label("Copy plan")
                            .tooltip("Copy the server's own output")
                            .on_click(cx.listener(Self::copy_plan)),
                    )
                    .child(
                        Button::new("plan-copy-json")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Braces)
                            .label("Copy JSON")
                            .tooltip("Copy the plan as JSON")
                            .on_click(cx.listener(Self::copy_json)),
                    ),
            )
    }

    /// Show the warnings in a dialog, in words, so a long list stays out of
    /// the tree's way and the icon on a marked step still has its explanation.
    fn show_warnings(
        &mut self,
        _: &gpui_kit::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let warnings = self.plan.as_ref().map(Plan::warnings).unwrap_or_default();
        if warnings.is_empty() {
            return;
        }

        window.open_dialog(cx, move |dialog, _window, cx| {
            dialog
                .w(px(560.))
                .title("Plan warnings")
                .overlay_closable(true)
                .keyboard(true)
                .child(
                    v_flex()
                        .id("plan-warnings-list")
                        .gap_3()
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Icon::new(IconName::TriangleAlert)
                                        .xsmall()
                                        .text_color(cx.theme().warning),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child("Warning:"),
                                ),
                        )
                        .children(warnings.iter().map(|(node, warning)| {
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(node.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(warning.clone()),
                                )
                        })),
                )
        });
    }

    /// The column headings, aligned over the tree's own numbers.
    fn render_columns(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let heading = |width: f32, text: &str| -> gpui_kit::AnyElement {
            div()
                .w(px(width))
                .flex_none()
                .text_right()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text.to_string())
                .into_any_element()
        };

        h_flex()
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Step"),
            )
            .child(heading(NUMBER_WIDTH, "Est. rows"))
            .child(heading(NUMBER_WIDTH, "Actual"))
            .child(heading(COST_WIDTH, "Cost"))
            .child(heading(TIME_WIDTH, "Time"))
            .child(heading(SHARE_WIDTH, "Self time"))
    }
}

impl Render for PlanView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();

        let body: gpui_kit::AnyElement = match &self.plan {
            None => div()
                .flex_1()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No plan yet. Run Explain from the editor.")
                .into_any_element(),
            Some(plan) if !plan.parsed => v_flex()
                .flex_1()
                .min_h_0()
                .gap_1()
                .p_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child("Could not read this plan format; showing raw output."),
                )
                .child(
                    div()
                        .id("plan-raw")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .font_family(settings::grid_font(cx))
                        .text_sm()
                        .child(plan.raw.clone()),
                )
                .into_any_element(),
            Some(_) => v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div().flex_1().min_h_0().child(
                        tree(&self.tree, move |_ix, entry, _selected, _window, cx| {
                            this.update(cx, |this, cx| this.row(entry, cx))
                        })
                        .size_full(),
                    ),
                )
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .key_context("PlanView")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus(window, cx)),
            )
            .child(self.render_header(cx))
            .when(self.plan.as_ref().is_some_and(|plan| plan.parsed), |this| {
                this.child(self.render_columns(cx))
            })
            .child(body)
    }
}

/// How many steps a tree holds, the root included.
fn steps(node: &PlanNode) -> usize {
    1 + node.children.iter().map(steps).sum::<usize>()
}

/// Turn one plan node into a tree row, recording what the row will need to draw
/// itself. `path` is the node's id, which is also how a row finds its facts.
fn tree_item(
    node: &PlanNode,
    path: &str,
    depth: usize,
    total: Option<f64>,
    expansion: Expansion,
    nodes: &mut HashMap<SharedString, NodeFacts>,
) -> TreeItem {
    let id = SharedString::from(path.to_string());
    nodes.insert(
        id.clone(),
        NodeFacts {
            estimated_rows: node.estimated_rows,
            actual_rows: node.actual_rows,
            cost: node.cost.map(|(_, total_cost)| total_cost),
            total_ms: node.total_ms(),
            share: total.and_then(|total| node.exclusive_share(total)),
            warnings: node.warnings.len(),
        },
    );

    let expanded = match expansion {
        Expansion::All => true,
        Expansion::None => false,
        Expansion::Auto => depth < AUTO_EXPAND_DEPTH,
    };

    TreeItem::new(id, node.label.clone())
        .expanded(expanded)
        .children(
            node.children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    tree_item(
                        child,
                        &format!("{path}/{index}"),
                        depth + 1,
                        total,
                        expansion,
                        nodes,
                    )
                })
                .collect::<Vec<_>>(),
        )
}

#[cfg(test)]
impl PlanView {
    /// The labels of the plan's steps, in tree order, for a test to inspect.
    pub(crate) fn labels_for_test(&self) -> Vec<String> {
        self.plan
            .as_ref()
            .map(|plan| {
                let mut labels = Vec::new();
                collect_labels(&plan.root, &mut labels);
                labels
            })
            .unwrap_or_default()
    }

    pub(crate) fn warnings_for_test(&self) -> Vec<(String, String)> {
        self.plan.as_ref().map(Plan::warnings).unwrap_or_default()
    }
}

#[cfg(test)]
fn collect_labels(node: &PlanNode, labels: &mut Vec<String>) {
    labels.push(node.label.clone());
    for child in &node.children {
        collect_labels(child, labels);
    }
}
