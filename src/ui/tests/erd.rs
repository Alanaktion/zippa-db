//! The ER diagram tab: it opens on every engine (SQLite has foreign keys
//! too), reuses the open panel, and loads the tables and their relations in
//! the background. `db::erd::tests` covers the layout and the SVG; the live
//! `all_foreign_keys` queries are covered by the ignored live-server tests in
//! `db::tests` alongside the other per-engine SQL.

use super::*;
use crate::db::runtime;
use crate::ui::session::Tool;

#[gpui_kit::test]
fn erd_tab_opens_and_reuses_the_open_panel(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    for _ in 0..2 {
        handle
            .update(cx, |session, window, cx| {
                session.open_tool(Tool::Erd, window, cx)
            })
            .unwrap();
    }

    let titles = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        titles.iter().filter(|t| *t == "ER diagram").count(),
        1,
        "opening the ER diagram twice must reuse the one panel"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_erd_view(cx))
            .unwrap()
            .is_some()
    );
}

#[gpui_kit::test]
fn erd_diagram_loads_tables_and_their_foreign_keys(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // Two tables joined by a foreign key, on top of the seeded `items` table.
    let connection = handle
        .update(cx, |session, _, _| session.connection())
        .unwrap();
    runtime::block_on(async {
        connection
            .run_query("CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .expect("creating authors failed");
        connection
            .run_query(
                "CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES authors(id), title TEXT)",
            )
            .await
            .expect("creating books failed");
    });

    handle
        .update(cx, |session, window, cx| {
            session.open_tool(Tool::Erd, window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let view = handle
        .update(cx, |session, _, cx| session.active_erd_view(cx))
        .unwrap()
        .expect("the ER diagram tab should hold a view");
    let diagram = view
        .update(cx, |view, _| view.diagram_for_test().cloned())
        .expect("the diagram should have loaded");
    let names: Vec<&str> = diagram
        .nodes
        .iter()
        .map(|node| node.table.name.as_str())
        .collect();
    assert!(names.contains(&"authors"), "tables: {names:?}");
    assert!(names.contains(&"books"), "tables: {names:?}");
    assert_eq!(
        diagram.edges.len(),
        1,
        "the one foreign key should be the one edge"
    );
    let edge = &diagram.edges[0];
    let (from, to) = (
        diagram.nodes[edge.from].table.name.as_str(),
        diagram.nodes[edge.to].table.name.as_str(),
    );
    assert_eq!((from, to), ("books", "authors"));

    // The edge runs between the rows of the columns the key joins.
    let books = &diagram.nodes[edge.from];
    let columns: Vec<&str> = books
        .table
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    assert_eq!(columns, ["id", "author_id", "title"]);
    assert!(books.table.columns[0].primary_key);
    assert_eq!((edge.from_row, edge.to_row), (Some(1), Some(0)));
    assert_eq!(edge.to_columns, ["id"]);
}

#[gpui_kit::test]
fn erd_focus_shows_only_the_start_tables_neighborhood(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // books references authors and publishers; authors references countries.
    let connection = handle
        .update(cx, |session, _, _| session.connection())
        .unwrap();
    runtime::block_on(async {
        connection
            .run_query("CREATE TABLE countries (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .expect("creating countries failed");
        connection
            .run_query(
                "CREATE TABLE authors (id INTEGER PRIMARY KEY, country_id INTEGER REFERENCES countries(id), name TEXT)",
            )
            .await
            .expect("creating authors failed");
        connection
            .run_query("CREATE TABLE publishers (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .expect("creating publishers failed");
        connection
            .run_query(
                "CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES authors(id), publisher_id INTEGER REFERENCES publishers(id), title TEXT)",
            )
            .await
            .expect("creating books failed");
        connection
            .run_query("CREATE TABLE unrelated (id INTEGER PRIMARY KEY)")
            .await
            .expect("creating unrelated failed");
    });

    handle
        .update(cx, |session, window, cx| {
            session.open_tool(Tool::Erd, window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let view = handle
        .update(cx, |session, _, cx| session.active_erd_view(cx))
        .unwrap()
        .expect("the ER diagram tab should hold a view");

    let focused_names = |view: &gpui_kit::Entity<crate::ui::erd::ErdView>,
                         cx: &mut TestAppContext| {
        let diagram = view
            .update(cx, |view, _| view.diagram_for_test().cloned())
            .expect("the diagram should have loaded");
        let mut names: Vec<String> = diagram
            .nodes
            .iter()
            .map(|node| node.table.name.clone())
            .collect();
        names.sort();
        (names, diagram.edges.len())
    };

    // Depth 1 from books: books and its direct relations only.
    view.update(cx, |view, cx| {
        view.focus_table_for_test(Some("books"), Some(1), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        focused_names(&view, cx),
        (
            vec![
                "authors".to_string(),
                "books".to_string(),
                "publishers".to_string()
            ],
            2
        )
    );

    // Depth 2 reaches countries through authors.
    view.update(cx, |view, cx| {
        view.focus_table_for_test(Some("books"), Some(2), cx)
    });
    cx.run_until_parked();
    let (names, edges) = focused_names(&view, cx);
    assert_eq!(
        names,
        vec![
            "authors".to_string(),
            "books".to_string(),
            "countries".to_string(),
            "publishers".to_string()
        ]
    );
    assert_eq!(edges, 3);

    // Clearing the focus restores the whole schema, unrelated tables included.
    view.update(cx, |view, cx| view.focus_table_for_test(None, None, cx));
    cx.run_until_parked();
    let (names, _) = focused_names(&view, cx);
    assert!(names.contains(&"unrelated".to_string()), "{names:?}");
    assert!(names.contains(&"items".to_string()), "{names:?}");
}
