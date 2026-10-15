//! ER diagram tab: the database's tables and foreign keys as a pannable,
//! zoomable canvas, exportable to SVG.
//!
//! The whole diagram renders as one [`svg`] element, so hundreds of tables
//! stay cheap: panning and zooming only change the element's transformation,
//! never the SVG bytes. The export writes those same bytes.
//!
//! The SVG's viewBox is symmetric around the diagram's center, so the
//! element's transformation — which scales around the element's own center —
//! keeps the diagram centered: the resting state (zoom 1, no offset) is the
//! fitted state, and zooming never drifts it.

use std::sync::Arc;

use anyhow::Result;
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, FocusHandle, Focusable, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Render, Rgba, ScrollDelta, ScrollWheelEvent,
    Styled, Transformation, Window, div, point, px, size, svg,
};

use crate::db::erd::{self, ErdDiagram, ErdPalette, ErdTable};
use crate::db::{Connection, ObjectKind, runtime};
use crate::ui::busy::busy_label;
use crate::ui::sql_file;

enum ErdPhase {
    Loading,
    Ready { diagram: ErdDiagram, svg: Vec<u8> },
    Failed(String),
}

const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 8.0;

pub struct ErdView {
    connection: Arc<Connection>,
    phase: ErdPhase,
    zoom: f32,
    /// Translation applied to the SVG element, in screen pixels.
    offset: (f32, f32),
    last_mouse: Option<(f32, f32)>,
    notice: Option<String>,
    focus: FocusHandle,
}

impl ErdView {
    pub fn new(connection: Arc<Connection>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            connection,
            phase: ErdPhase::Loading,
            zoom: 1.0,
            offset: (0.0, 0.0),
            last_mouse: None,
            notice: None,
            focus: cx.focus_handle(),
        };
        view.reload(cx);
        view
    }

    /// Read-only connections are fine — the diagram never writes.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
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
        cx.notify();

        // Queries and the simulation both go through the database runtime:
        // sqlx needs its Tokio reactor, and under test the runtime runs the
        // work inline on the calling thread instead.
        let connection = self.connection.clone();
        let task = runtime::spawn(async move {
            let objects = connection.objects().await?;
            let tables: Vec<ErdTable> = objects
                .iter()
                .filter(|object| object.kind == ObjectKind::Table)
                .map(|object| ErdTable {
                    schema: object.schema.clone(),
                    name: object.name.clone(),
                })
                .collect();
            let relations = connection.all_foreign_keys().await?;
            Ok::<_, anyhow::Error>(erd::layout_diagram(&tables, &relations))
        });

        cx.spawn(async move |this, cx| {
            let outcome: Result<ErdDiagram> = task
                .await
                .map_err(|_| anyhow::anyhow!("the schema read was cancelled"))
                .and_then(|inner| inner);
            this.update(cx, |this, cx| {
                this.phase = match outcome {
                    Ok(diagram) => {
                        let svg = erd::diagram_svg(&diagram, &palette_for(cx.theme())).into_bytes();
                        ErdPhase::Ready { diagram, svg }
                    }
                    Err(error) => ErdPhase::Failed(format!("{error:#}")),
                };
                // The resting state is the fitted state (see the module docs).
                this.zoom = 1.0;
                this.offset = (0.0, 0.0);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The resting state is the fitted state; "fit" just returns there.
    fn fit(&mut self, cx: &mut Context<Self>) {
        self.zoom = 1.0;
        self.offset = (0.0, 0.0);
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        cx.notify();
    }

    fn export_svg(&mut self, cx: &mut Context<Self>) {
        let ErdPhase::Ready { svg, .. } = &self.phase else {
            return;
        };
        let svg = svg.clone();
        let database = self.connection.database().to_string();
        // The prompt is created here, on the UI thread; the spawned task only
        // waits for its answer.
        let title = if database.is_empty() {
            "ER diagram".to_string()
        } else {
            format!("{database} ER diagram")
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
        let zoom_label = format!("{}%", (self.zoom * 100.0).round());

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
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1.25, cx))),
            )
            .child(
                Button::new("erd-export")
                    .ghost()
                    .small()
                    .icon(IconName::Download)
                    .label("Export SVG")
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
            ErdPhase::Ready { svg: bytes, .. } => {
                let transformation = Transformation::scale(size(self.zoom, self.zoom))
                    .with_translation(point(px(self.offset.0), px(self.offset.1)));
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
                            this.zoom_by((amount * 0.15).exp(), cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        svg()
                            .data(bytes)
                            .with_transformation(transformation)
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
            ErdPhase::Ready { diagram, .. } => Some(diagram),
            ErdPhase::Loading | ErdPhase::Failed(_) => None,
        }
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

/// The SVG palette follows the app theme; the export uses the same colours
/// the canvas shows.
fn palette_for(theme: &Theme) -> ErdPalette {
    ErdPalette {
        background: hex(theme.background),
        node_fill: hex(theme.secondary),
        node_border: hex(theme.border),
        text: hex(theme.foreground),
        edge: hex(theme.muted_foreground),
    }
}

fn hex(color: Hsla) -> String {
    format!("#{:06x}", u32::from(Rgba::from(color)) >> 8)
}
