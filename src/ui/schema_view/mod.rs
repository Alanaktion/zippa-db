//! A table's own definition: columns, indexes, and foreign keys.
//!
//! TODO.md's "Table Schema Inspector & DDL Generator", phase 1: read-only for
//! now, so this loads a [`TableSchema`] the way `TableView::load_row_key`
//! loads the primary key and lays it out as three sections of plain flex
//! rows. A table's own structure is a handful of rows with heterogeneous
//! per-row detail, which is a form rather than a data grid, so this does not
//! reach for `gpui_kit::component::table` the way `DataGrid` does.

use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Window, div, px};

use crate::db::{
    ColumnDef, Connection, DatabaseObject, ForeignKeyDef, IndexDef, ObjectKind, TableSchema,
    runtime,
};

pub struct SchemaView {
    connection: Arc<Connection>,
    object: DatabaseObject,
    schema: Option<TableSchema>,
    loading: bool,
    error: Option<String>,
}

impl SchemaView {
    pub fn new(
        connection: Arc<Connection>,
        object: DatabaseObject,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            connection,
            object,
            schema: None,
            loading: false,
            error: None,
        };
        view.reload(cx);
        view
    }

    pub fn object(&self) -> &DatabaseObject {
        &self.object
    }

    /// Always false in phase 1: nothing here is editable yet, so there is
    /// nothing to lose by closing the tab.
    pub(crate) fn is_dirty(&self) -> bool {
        false
    }

    /// Point the view at a new connection, as when the database is switched.
    pub(crate) fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        // A table of the same name in another database is another table, so
        // its structure is read again rather than carried over.
        self.schema = None;
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        cx.notify();

        let connection = self.connection.clone();
        let object = self.object.clone();
        let task = runtime::spawn(async move { connection.table_schema(&object).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(schema)) => this.schema = Some(schema),
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => {
                        this.error = Some("reading the table's structure was cancelled".into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        let kind = match self.object.kind {
            ObjectKind::Table => "table",
            ObjectKind::View => "view",
        };

        h_flex()
            .w_full()
            .flex_none()
            .gap_2()
            .child(div().text_sm().child(self.object.label()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(kind),
            )
    }

    fn render_section_heading(&self, title: &'static str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .px_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(title)
    }

    fn render_columns(&self, columns: &[ColumnDef], cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(self.render_section_heading("COLUMNS", cx))
            .children(columns.iter().map(|column| {
                h_flex()
                    .w_full()
                    .gap_3()
                    .px_1()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(column.name.clone()),
                    )
                    .when(column.is_primary_key, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child("PK"),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(220.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(column.type_name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(80.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if column.nullable { "NULL" } else { "NOT NULL" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(column.default.clone().unwrap_or_default()),
                    )
            }))
    }

    fn render_indexes(&self, indexes: &[IndexDef], cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(self.render_section_heading("INDEXES", cx))
            .when(indexes.is_empty(), |this| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No indexes"),
                )
            })
            .children(indexes.iter().map(|index| {
                let kind = match (index.is_primary_key, index.unique) {
                    (true, _) => "PRIMARY KEY",
                    (false, true) => "UNIQUE",
                    (false, false) => "INDEX",
                };

                h_flex()
                    .w_full()
                    .gap_3()
                    .px_1()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(index.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(100.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(kind),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(index.columns.join(", ")),
                    )
            }))
    }

    fn render_foreign_keys(&self, keys: &[ForeignKeyDef], cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(self.render_section_heading("FOREIGN KEYS", cx))
            .when(keys.is_empty(), |this| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No foreign keys"),
                )
            })
            .children(keys.iter().map(|key| {
                let reference = match &key.referenced_schema {
                    Some(schema) => format!("{schema}.{}", key.referenced_table),
                    None => key.referenced_table.clone(),
                };

                h_flex()
                    .w_full()
                    .gap_3()
                    .px_1()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(key.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(format!(
                                "{} → {reference}({})",
                                key.columns.join(", "),
                                key.referenced_columns.join(", ")
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(260.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "ON DELETE {} · ON UPDATE {}",
                                key.on_delete.label(),
                                key.on_update.label()
                            )),
                    )
            }))
    }
}

impl Render for SchemaView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("schema-view")
            .test_support()
            .size_full()
            .p_3()
            .gap_3()
            .child(self.render_header(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Error: {error}")),
                )
            })
            .when(self.loading && self.schema.is_none(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Loading…"),
                )
            })
            .child(
                div()
                    .id("schema-sections")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().size_full().gap_4().when_some(
                        self.schema.clone(),
                        |this, schema| {
                            this.child(self.render_columns(&schema.columns, cx))
                                .child(self.render_indexes(&schema.indexes, cx))
                                .child(self.render_foreign_keys(&schema.foreign_keys, cx))
                        },
                    )),
            )
    }
}

#[cfg(test)]
impl SchemaView {
    pub(crate) fn schema_for_test(&self) -> Option<TableSchema> {
        self.schema.clone()
    }

    pub(crate) fn error_for_test(&self) -> Option<String> {
        self.error.clone()
    }
}
