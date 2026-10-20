//! ER diagram tab: the database's tables, their columns, and the foreign
//! keys between them as a pannable, zoomable canvas, exportable to SVG.
//!
//! Picking a table in the toolbar focuses its neighborhood — the tables
//! reachable through relations, up to the chosen depth — and re-layouts
//! just that subgraph. "All tables" goes back to the whole schema.
//!
//! The diagram is painted straight onto one [`canvas`] at the current zoom —
//! boxes as quads, edges as stroked paths, labels as shaped text — so it
//! stays crisp at every scale and only what is on screen is drawn. GPUI's
//! `svg()` element cannot stand in: it rasterizes to a one-colour mask.
//! The export writes an SVG built from the same layout and labels.
//!
//! The diagram is laid out centered on the origin, and the canvas puts the
//! origin at its own center plus the pan offset, so the resting state (zoom
//! 1, no offset) is the fitted state.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme, Disableable, IndexPath, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, BorderStyle, Bounds, ContentMask, Context, Corners, Entity, FocusHandle,
    Focusable, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, PathBuilder, Pixels, Point, Render, Rgba, ScrollDelta,
    ScrollWheelEvent, SharedString, Size, Styled, TextAlign, TextRun, Window, canvas, div, fill,
    point, px, quad, size, transparent_black,
};

use crate::db::erd::{
    self, CORNER_RADIUS, ErdDiagram, ErdPalette, ErdRelation, ErdTable, HEADER_HEIGHT, ROW_HEIGHT,
    ROW_PADDING, SELF_LOOP_REACH, TAG_WIDTH, TableLabels,
};
use crate::db::{Connection, runtime};
use crate::ui::busy::busy_label;
use crate::ui::sql_file;

enum ErdPhase {
    Loading,
    Ready {
        diagram: Arc<ErdDiagram>,
        labels: Arc<Vec<TableLabels>>,
    },
    Failed(String),
}

/// Zoom is relative to the fitted view: out to a quarter of it, and in until
/// the diagram is drawn at four times its natural size.
const MIN_ZOOM: f32 = 0.25;
const MAX_SCALE: f32 = 4.0;
/// The fitted view never blows a small diagram up past this.
const MAX_FIT: f32 = 1.5;
/// Margin around the diagram in the fitted view, in world units.
const FIT_PADDING: f32 = 48.0;
/// Below this many pixels, text is a smudge; boxes and edges alone read
/// better.
const MIN_TEXT_SIZE: f32 = 4.0;

pub struct ErdView {
    connection: Arc<Connection>,
    phase: ErdPhase,
    zoom: f32,
    /// Pan, in screen pixels, of the diagram's center from the canvas's.
    offset: (f32, f32),
    /// The canvas's bounds as last painted, for zooming about the pointer
    /// and for the zoom limits, which depend on the fitted scale.
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    last_mouse: Option<(f32, f32)>,
    notice: Option<String>,
    focus: FocusHandle,
    window: AnyWindowHandle,
    /// The whole schema, as loaded; the diagram shows all of it, or a
    /// neighborhood of `focus_table`.
    all_tables: Vec<ErdTable>,
    all_relations: Vec<ErdRelation>,
    /// The focused table, if any; `None` diagrams the whole schema.
    focus_table: Option<ErdTable>,
    /// How many hops out the neighborhood reaches; `None` is the table's
    /// whole connected component.
    max_depth: Option<usize>,
    /// Focus candidates, sorted by display name; parallel to the table
    /// picker's items after the "All tables" entry.
    picker_tables: Vec<ErdTable>,
    table_select: Entity<SelectState<SearchableVec<SharedString>>>,
    depth_select: Entity<SelectState<SearchableVec<SharedString>>>,
    /// Guards against out-of-order layout completions: only the latest
    /// generation may replace the diagram.
    layout_gen: u64,
}

impl ErdView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![SharedString::from("All tables")]),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
            .searchable(true)
        });
        let depth_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![
                    SharedString::from("1"),
                    SharedString::from("2"),
                    SharedString::from("3"),
                    SharedString::from("All"),
                ]),
                Some(IndexPath::new(3)),
                window,
                cx,
            )
        });
        cx.subscribe(
            &table_select,
            |this, _, _: &SelectEvent<SearchableVec<SharedString>>, cx| {
                this.on_table_picked(cx);
            },
        )
        .detach();
        cx.subscribe(
            &depth_select,
            |this, _, _: &SelectEvent<SearchableVec<SharedString>>, cx| {
                this.on_depth_picked(cx);
            },
        )
        .detach();

        let mut view = Self {
            connection,
            phase: ErdPhase::Loading,
            zoom: 1.0,
            offset: (0.0, 0.0),
            viewport: Rc::new(Cell::new(None)),
            last_mouse: None,
            notice: None,
            focus: cx.focus_handle(),
            window: window.window_handle(),
            all_tables: Vec::new(),
            all_relations: Vec::new(),
            focus_table: None,
            max_depth: None,
            picker_tables: Vec::new(),
            table_select,
            depth_select,
            layout_gen: 0,
        };
        view.reload(cx);
        view
    }

    /// Read-only connections are fine — the diagram never writes.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        // The focus belongs to the old database: clear it and the picker
        // now, before the reload brings the new schema in.
        self.focus_table = None;
        self.max_depth = None;
        self.all_tables = Vec::new();
        self.all_relations = Vec::new();
        self.picker_tables = Vec::new();
        self.sync_table_picker(cx);
        self.sync_depth_picker(cx);
        self.reload(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.phase = ErdPhase::Loading;
        self.zoom = 1.0;
        self.offset = (0.0, 0.0);
        self.notice = None;
        self.layout_gen += 1;
        let generation = self.layout_gen;
        cx.notify();

        // Queries and the layout both go through the database runtime:
        // sqlx needs its Tokio reactor, and under test the runtime runs the
        // work inline on the calling thread instead.
        let connection = self.connection.clone();
        let focus_table = self.focus_table.clone();
        let max_depth = self.max_depth;
        let task = runtime::spawn(async move {
            let all_tables = connection.erd_tables().await?;
            let all_relations = connection.all_foreign_keys().await?;
            let (tables, relations) = match &focus_table {
                Some(start) => erd::neighborhood(&all_tables, &all_relations, start, max_depth),
                None => (all_tables.clone(), all_relations.clone()),
            };
            let diagram = erd::layout_diagram(&tables, &relations);
            Ok::<_, anyhow::Error>((all_tables, all_relations, diagram))
        });

        cx.spawn(async move |this, cx| {
            let outcome: Result<(Vec<ErdTable>, Vec<ErdRelation>, ErdDiagram)> = task
                .await
                .map_err(|_| anyhow::anyhow!("the schema read was cancelled"))
                .and_then(|inner| inner);
            this.update(cx, |this, cx| {
                if this.layout_gen != generation {
                    // A newer reload or refocus already superseded this.
                    return;
                }
                match outcome {
                    Ok((all_tables, all_relations, diagram)) => {
                        this.all_tables = all_tables;
                        this.all_relations = all_relations;
                        // The focused table may be gone after a refresh.
                        let focus_gone = this.focus_table.as_ref().is_some_and(|focus| {
                            !this.all_tables.iter().any(|t| t.key() == focus.key())
                        });
                        if focus_gone {
                            this.focus_table = None;
                        }
                        this.sync_table_picker(cx);
                        this.phase = ErdPhase::Ready {
                            labels: Arc::new(erd::table_labels(&diagram)),
                            diagram: Arc::new(diagram),
                        };
                    }
                    Err(error) => {
                        this.phase = ErdPhase::Failed(format!("{error:#}"));
                    }
                }
                // The resting state is the fitted state (see the module docs).
                this.zoom = 1.0;
                this.offset = (0.0, 0.0);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-layout the focused neighborhood (or the whole schema) without
    /// re-reading it: picking a table or a depth only recomputes the
    /// subgraph. The current diagram stays up until the new one lands.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        if self.all_tables.is_empty() {
            return; // Nothing loaded yet; the pending reload covers it.
        }
        self.layout_gen += 1;
        let generation = self.layout_gen;
        let all_tables = self.all_tables.clone();
        let all_relations = self.all_relations.clone();
        let focus_table = self.focus_table.clone();
        let max_depth = self.max_depth;
        let task = runtime::spawn(async move {
            let (tables, relations) = match &focus_table {
                Some(start) => erd::neighborhood(&all_tables, &all_relations, start, max_depth),
                None => (all_tables, all_relations),
            };
            Ok::<_, anyhow::Error>(erd::layout_diagram(&tables, &relations))
        });

        cx.spawn(async move |this, cx| {
            let outcome: Result<ErdDiagram> = task
                .await
                .map_err(|_| anyhow::anyhow!("the schema read was cancelled"))
                .and_then(|inner| inner);
            this.update(cx, |this, cx| {
                if this.layout_gen != generation {
                    return;
                }
                if let Ok(diagram) = outcome {
                    this.phase = ErdPhase::Ready {
                        labels: Arc::new(erd::table_labels(&diagram)),
                        diagram: Arc::new(diagram),
                    };
                    this.zoom = 1.0;
                    this.offset = (0.0, 0.0);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The table picker's choice: "All tables" (or nothing) clears the
    /// focus, anything else focuses that table's neighborhood.
    fn on_table_picked(&mut self, cx: &mut Context<Self>) {
        let row = self
            .table_select
            .read(cx)
            .selected_index(cx)
            .map(|index| index.row);
        self.focus_table = match row {
            Some(0) | None => None,
            Some(i) => self.picker_tables.get(i - 1).cloned(),
        };
        self.relayout(cx);
    }

    /// The depth picker's choice: "All" is the table's whole connected
    /// component, a number caps the hops out.
    fn on_depth_picked(&mut self, cx: &mut Context<Self>) {
        let value = self.depth_select.read(cx).selected_value().cloned();
        self.max_depth = match value.as_deref() {
            Some("All") | None => None,
            Some(depth) => depth.parse::<usize>().ok(),
        };
        self.relayout(cx);
    }

    /// Rebuild the table picker's items from the loaded schema, keeping the
    /// focused table selected — or "All tables" when there is no focus.
    /// `set_selected_index` does not emit an event, so this never loops
    /// back into `on_table_picked`.
    fn sync_table_picker(&mut self, cx: &mut Context<Self>) {
        let mut picker_tables = self.all_tables.clone();
        picker_tables.sort_by_key(|table| table.display_name());
        let mut items = vec![SharedString::from("All tables")];
        items.extend(
            picker_tables
                .iter()
                .map(|table| SharedString::from(table.display_name())),
        );
        let selected = match &self.focus_table {
            None => 0,
            Some(focus) => picker_tables
                .iter()
                .position(|table| table.key() == focus.key())
                .map_or(0, |i| i + 1),
        };
        self.picker_tables = picker_tables;
        let select = self.table_select.clone();
        let _ = self.window.update(cx, |_, window, cx| {
            select.update(cx, |state, cx| {
                state.set_items(SearchableVec::new(items), window, cx);
                state.set_selected_index(Some(IndexPath::new(selected)), window, cx);
            });
        });
    }

    /// Reset the depth picker to "All".
    fn sync_depth_picker(&mut self, cx: &mut Context<Self>) {
        let selected = match self.max_depth {
            None => 3,
            Some(depth) => [1, 2, 3].iter().position(|&n| n == depth).unwrap_or(3),
        };
        let select = self.depth_select.clone();
        let _ = self.window.update(cx, |_, window, cx| {
            select.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(selected)), window, cx);
            });
        });
    }

    /// The resting state is the fitted state; "fit" just returns there.
    fn fit(&mut self, cx: &mut Context<Self>) {
        self.zoom = 1.0;
        self.offset = (0.0, 0.0);
        cx.notify();
    }

    /// The scale the fitted view draws at, from the last painted bounds.
    fn fit_scale(&self) -> Option<f32> {
        let ErdPhase::Ready { diagram, .. } = &self.phase else {
            return None;
        };
        Some(fit_scale(diagram, self.viewport.get()?.size))
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.zoom_about(factor, None, cx);
    }

    /// Zoom by `factor`, keeping the point under `anchor` (a window
    /// position) where it is; the canvas's center when `None`.
    fn zoom_about(&mut self, factor: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let max_zoom = self
            .fit_scale()
            .map_or(MAX_SCALE, |fit| (MAX_SCALE / fit).max(1.0));
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, max_zoom);
        let applied = zoom / self.zoom;
        if let (Some(anchor), Some(bounds)) = (anchor, self.viewport.get()) {
            // The anchor's offset from the diagram's center scales with the
            // zoom; move the center so the anchor stays put.
            let center = bounds.center();
            let (ax, ay) = (
                (anchor.x - center.x).as_f32(),
                (anchor.y - center.y).as_f32(),
            );
            self.offset.0 = ax - (ax - self.offset.0) * applied;
            self.offset.1 = ay - (ay - self.offset.1) * applied;
        } else {
            self.offset.0 *= applied;
            self.offset.1 *= applied;
        }
        self.zoom = zoom;
        cx.notify();
    }

    fn export_svg(&mut self, cx: &mut Context<Self>) {
        let ErdPhase::Ready { diagram, .. } = &self.phase else {
            return;
        };
        // Built now rather than at load, so it takes the theme as it is.
        let svg = erd::diagram_svg(diagram, &palette_for(cx.theme()));
        let database = self.connection.database().to_string();
        // The prompt is created here, on the UI thread; the spawned task only
        // waits for its answer.
        let title = match (&self.focus_table, database.is_empty()) {
            (Some(table), _) => format!("{} relations", table.display_name()),
            (None, false) => format!("{database} ER diagram"),
            (None, true) => "ER diagram".to_string(),
        };
        let prompt = sql_file::prompt_for_save(None, &title, "svg", cx);
        cx.spawn(async move |this, cx| {
            let path = prompt.await.unwrap_or(None);
            let Some(path) = path else { return };
            let written = cx
                .background_spawn(async move { std::fs::write(&path, &svg) })
                .await;
            this.update(cx, |this, cx| {
                this.notice = written
                    .err()
                    .map(|error| format!("Could not export the diagram: {error:#}"));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (tables, relations) = match &self.phase {
            ErdPhase::Ready { diagram, .. } => (diagram.nodes.len(), diagram.edges.len()),
            _ => (0, 0),
        };
        let ready = matches!(self.phase, ErdPhase::Ready { .. });
        // The scale the diagram is drawn at, which is what a percentage
        // reads as; the fitted view's own until the canvas has been painted.
        let scale = self.fit_scale().unwrap_or(1.0) * self.zoom;
        let zoom_label = format!("{}%", (scale * 100.0).round());

        h_flex()
            .id("erd-toolbar")
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {}",
                        plural(tables, "table"),
                        plural(relations, "relation"),
                    )),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Table:"),
            )
            .child(
                div().w(px(200.)).child(
                    Select::new(&self.table_select)
                        .id("erd-table")
                        .xsmall()
                        .accessibility_label("Focus table"),
                ),
            )
            .when(self.focus_table.is_some(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Depth:"),
                )
                .child(
                    div().w(px(88.)).child(
                        Select::new(&self.depth_select)
                            .id("erd-depth")
                            .xsmall()
                            .accessibility_label("Neighborhood depth"),
                    ),
                )
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(notice))
            })
            .child(
                Button::new("erd-fit")
                    .ghost()
                    .small()
                    .icon(IconName::Maximize)
                    .accessibility_label("Fit diagram to view")
                    .tooltip("Fit the whole diagram into view")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.fit(cx))),
            )
            .child(
                Button::new("erd-zoom-out")
                    .ghost()
                    .small()
                    .icon(IconName::ZoomOut)
                    .accessibility_label("Zoom out")
                    .tooltip("Zoom out")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1.0 / 1.25, cx))),
            )
            .child(
                div()
                    .text_sm()
                    .w(px(52.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(zoom_label),
            )
            .child(
                Button::new("erd-zoom-in")
                    .ghost()
                    .small()
                    .icon(IconName::ZoomIn)
                    .accessibility_label("Zoom in")
                    .tooltip("Zoom in")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1.25, cx))),
            )
            .child(
                Button::new("erd-export")
                    .ghost()
                    .small()
                    .icon(IconName::Download)
                    .label("Export SVG…")
                    .accessibility_label("Export the diagram as SVG")
                    .tooltip("Write the diagram to an SVG file")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.export_svg(cx))),
            )
    }

    fn render_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.phase {
            ErdPhase::Loading => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(busy_label(
                    "Reading the schema…",
                    cx.theme().muted_foreground,
                ))
                .into_any_element(),
            ErdPhase::Failed(error) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(format!("Could not build the diagram: {error}")),
                )
                .into_any_element(),
            ErdPhase::Ready { diagram, .. } if diagram.nodes.is_empty() => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("This database has no tables to diagram."),
                )
                .into_any_element(),
            ErdPhase::Ready { diagram, labels } => {
                let scene = Scene {
                    diagram: diagram.clone(),
                    labels: labels.clone(),
                    colors: SceneColors::from(cx.theme()),
                    zoom: self.zoom,
                    offset: self.offset,
                };
                let viewport = self.viewport.clone();
                div()
                    .id("erd-canvas")
                    .test_support()
                    .size_full()
                    .overflow_hidden()
                    .bg(cx.theme().background)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.last_mouse =
                                Some((event.position.x.as_f32(), event.position.y.as_f32()));
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        let at = (event.position.x.as_f32(), event.position.y.as_f32());
                        if event.dragging()
                            && let Some(last) = this.last_mouse
                        {
                            this.offset.0 += at.0 - last.0;
                            this.offset.1 += at.1 - last.1;
                            cx.notify();
                        }
                        this.last_mouse = Some(at);
                    }))
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        let amount = match event.delta {
                            ScrollDelta::Pixels(delta) => delta.y.as_f32() / 48.0,
                            ScrollDelta::Lines(lines) => lines.y,
                        };
                        if amount != 0.0 {
                            this.zoom_about((amount * 0.15).exp(), Some(event.position), cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        canvas(
                            move |bounds, _, _| viewport.set(Some(bounds)),
                            move |bounds, (), window, cx| scene.paint(bounds, window, cx),
                        )
                        .size_full(),
                    )
                    .into_any_element()
            }
        }
    }
}

impl ErdView {
    /// The loaded diagram, for tests.
    #[cfg(test)]
    pub(crate) fn diagram_for_test(&self) -> Option<&ErdDiagram> {
        match &self.phase {
            ErdPhase::Ready { diagram, .. } => Some(diagram.as_ref()),
            ErdPhase::Loading | ErdPhase::Failed(_) => None,
        }
    }

    /// Focus a table's neighborhood the way the toolbar's pickers do, for
    /// tests. `None` clears the focus back to the whole schema.
    #[cfg(test)]
    pub(crate) fn focus_table_for_test(
        &mut self,
        name: Option<&str>,
        max_depth: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.focus_table = name.and_then(|name| {
            self.all_tables
                .iter()
                .find(|table| table.name == name)
                .cloned()
        });
        self.max_depth = max_depth;
        self.relayout(cx);
    }
}

impl Focusable for ErdView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ErdView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("erd-view")
            .test_support()
            .size_full()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.render_canvas(cx)))
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The scale that fits the whole diagram, with its margin, into `viewport`.
fn fit_scale(diagram: &ErdDiagram, viewport: Size<Pixels>) -> f32 {
    let (min_x, min_y, max_x, max_y) = erd::bounds(diagram, FIT_PADDING);
    // The diagram is centered on the origin, so fit its larger half each way.
    let half_w = min_x.abs().max(max_x.abs()).max(1.0);
    let half_h = min_y.abs().max(max_y.abs()).max(1.0);
    let fit = (viewport.width.as_f32() / (2.0 * half_w))
        .min(viewport.height.as_f32() / (2.0 * half_h))
        .min(MAX_FIT);
    if fit.is_finite() && fit > 0.0 {
        fit
    } else {
        1.0
    }
}

/// The theme's colours for the canvas, matching [`palette_for`].
#[derive(Clone, Copy)]
struct SceneColors {
    node_fill: Hsla,
    header_fill: Hsla,
    node_border: Hsla,
    text: Hsla,
    muted_text: Hsla,
    edge: Hsla,
}

impl From<&Theme> for SceneColors {
    fn from(theme: &Theme) -> Self {
        Self {
            node_fill: theme.background,
            header_fill: theme.secondary,
            node_border: theme.border,
            text: theme.foreground,
            muted_text: theme.muted_foreground,
            edge: theme.muted_foreground,
        }
    }
}

/// Everything one frame of the canvas needs, moved into its paint closure.
struct Scene {
    diagram: Arc<ErdDiagram>,
    labels: Arc<Vec<TableLabels>>,
    colors: SceneColors,
    zoom: f32,
    offset: (f32, f32),
}

impl Scene {
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let scale = fit_scale(&self.diagram, bounds.size) * self.zoom;
        let center = bounds.center();
        let origin = (
            center.x.as_f32() + self.offset.0,
            center.y.as_f32() + self.offset.1,
        );
        let at = |(x, y): (f32, f32)| point(px(origin.0 + x * scale), px(origin.1 + y * scale));
        let rect = |x: f32, y: f32, w: f32, h: f32| Bounds {
            origin: at((x, y)),
            size: size(px(w * scale), px(h * scale)),
        };
        let on_screen = |area: &Bounds<Pixels>| area.intersects(&bounds);

        let stroke = px((1.5 * scale).clamp(0.75, 2.5));
        for edge in &self.diagram.edges {
            let Some(&first) = edge.path.first() else {
                continue;
            };
            let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
            for &(x, y) in &edge.path {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
            // Room for the end marks and a self-reference's loop.
            let reach = 12.0
                + if edge.from == edge.to {
                    SELF_LOOP_REACH
                } else {
                    0.0
                };
            let area = rect(x0 - reach, y0 - 8.0, x1 - x0 + 2.0 * reach, y1 - y0 + 16.0);
            if !on_screen(&area) {
                continue;
            }
            let mut curve = PathBuilder::stroke(stroke);
            curve.move_to(at(first));
            for piece in erd::edge_curve(edge) {
                curve.cubic_bezier_to(at(piece.to), at(piece.control_a), at(piece.control_b));
            }
            if let Ok(path) = curve.build() {
                window.paint_path(path, self.colors.edge);
            }
            let mut marks = PathBuilder::stroke(stroke);
            for (a, b) in erd::edge_marks(edge) {
                marks.move_to(at(a));
                marks.line_to(at(b));
            }
            if let Ok(path) = marks.build() {
                window.paint_path(path, self.colors.edge);
            }
        }

        let radius = px(CORNER_RADIUS * scale);
        let show_text = 12.0 * scale >= MIN_TEXT_SIZE;
        for (node, labels) in self.diagram.nodes.iter().zip(self.labels.iter()) {
            let area = rect(node.x, node.y, node.width, node.height);
            if !on_screen(&area) {
                continue;
            }
            let header = rect(node.x, node.y, node.width, HEADER_HEIGHT);
            let header_corners = if labels.rows.is_empty() {
                Corners::all(radius)
            } else {
                Corners {
                    top_left: radius,
                    top_right: radius,
                    bottom_right: px(0.0),
                    bottom_left: px(0.0),
                }
            };
            window.paint_quad(quad(
                area,
                radius,
                self.colors.node_fill,
                px(0.0),
                transparent_black(),
                BorderStyle::Solid,
            ));
            window.paint_quad(quad(
                header,
                header_corners,
                self.colors.header_fill,
                px(0.0),
                transparent_black(),
                BorderStyle::Solid,
            ));
            if !labels.rows.is_empty() {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(header.origin.x, header.origin.y + header.size.height),
                        size: size(header.size.width, px(1.0)),
                    },
                    self.colors.node_border,
                ));
            }
            window.paint_quad(quad(
                area,
                radius,
                transparent_black(),
                px(1.0),
                self.colors.node_border,
                BorderStyle::Solid,
            ));

            if !show_text {
                continue;
            }
            window.with_content_mask(Some(ContentMask { bounds: area }), |window| {
                paint_text(
                    &labels.header,
                    13.0 * scale,
                    true,
                    self.colors.text,
                    header.origin,
                    header.size.height,
                    TextAlign::Center,
                    Some(header.size.width),
                    window,
                    cx,
                );
                let padding = ROW_PADDING * scale;
                for (row, label) in labels.rows.iter().enumerate() {
                    let line = rect(
                        node.x,
                        node.y + HEADER_HEIGHT + ROW_HEIGHT * row as f32,
                        node.width,
                        ROW_HEIGHT,
                    );
                    if !on_screen(&line) {
                        continue;
                    }
                    let left = line.origin.x + px(padding);
                    if !label.tag.is_empty() {
                        let color = if label.primary_key {
                            self.colors.text
                        } else {
                            self.colors.muted_text
                        };
                        paint_text(
                            label.tag,
                            9.0 * scale,
                            true,
                            color,
                            point(left, line.origin.y),
                            line.size.height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    paint_text(
                        &label.name,
                        12.0 * scale,
                        false,
                        self.colors.text,
                        point(left + px(TAG_WIDTH * scale), line.origin.y),
                        line.size.height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    paint_text(
                        &label.data_type,
                        11.0 * scale,
                        false,
                        self.colors.muted_text,
                        point(left, line.origin.y),
                        line.size.height,
                        TextAlign::Right,
                        Some(line.size.width - px(2.0 * padding)),
                        window,
                        cx,
                    );
                }
            });
        }
    }
}

/// Shape and paint one line of text in the window's own font, centered
/// vertically in `line_height`.
#[allow(clippy::too_many_arguments)]
fn paint_text(
    text: &str,
    font_size: f32,
    bold: bool,
    color: Hsla,
    origin: Point<Pixels>,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    if text.is_empty() {
        return;
    }
    let mut font = window.text_style().font();
    if bold {
        font.weight = FontWeight::BOLD;
    }
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(text.to_string()),
        px(font_size),
        &[run],
        None,
    );
    line.paint(origin, line_height, align, align_width, window, cx)
        .ok();
}

/// The SVG palette follows the app theme; the export uses the same colours
/// the canvas shows.
fn palette_for(theme: &Theme) -> ErdPalette {
    ErdPalette {
        background: hex(theme.background),
        node_fill: hex(theme.background),
        header_fill: hex(theme.secondary),
        node_border: hex(theme.border),
        text: hex(theme.foreground),
        muted_text: hex(theme.muted_foreground),
        edge: hex(theme.muted_foreground),
    }
}

fn hex(color: Hsla) -> String {
    format!("#{:06x}", u32::from(Rgba::from(color)) >> 8)
}
