//! The keyboard shortcut reference (`Ctrl+/`).
//!
//! A dialog over the window listing every binding in `keymap.rs`, grouped by
//! what it acts on. The list is written out here rather than read back from
//! GPUI's keymap: the keymap knows contexts, not descriptions, and a reader
//! wants "Close the active tab" more than `CloseTab`. Keep it in step with
//! `keymap.rs` when a binding is added.

use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Keystroke, Window, actions, div, px};

actions!(zippa_db, [ShowShortcuts]);

const WIDTH: f32 = 620.;
const HEIGHT: f32 = 520.;

struct Group {
    title: &'static str,
    /// What it does, and the keystrokes that do it, in the keymap's syntax.
    rows: &'static [(&'static str, &'static [&'static str])],
}

const GROUPS: &[Group] = &[
    Group {
        title: "General",
        rows: &[
            ("Show this list", &["ctrl-/"]),
            ("Quick switcher", &["secondary-k"]),
            ("Search the schema", &["secondary-shift-o"]),
            ("Settings", &["secondary-,"]),
        ],
    },
    Group {
        title: "Connections",
        rows: &[
            ("New connection (or another tab)", &["secondary-n"]),
            ("Close the active connection", &["secondary-shift-w"]),
            ("Next connection", &["secondary-shift-]"]),
            ("Previous connection", &["secondary-shift-["]),
        ],
    },
    Group {
        title: "Tabs",
        rows: &[
            ("New query tab", &["secondary-t"]),
            ("Close the active tab", &["secondary-w"]),
            ("Next tab", &["ctrl-tab"]),
            ("Previous tab", &["ctrl-shift-tab"]),
            ("Refresh the schema and the open table", &["secondary-r"]),
        ],
    },
    Group {
        title: "Files",
        rows: &[
            ("Open a SQL file", &["secondary-o"]),
            ("Import a SQL dump", &["secondary-shift-i"]),
            ("Save the active tab", &["secondary-s"]),
            (
                "Save the active tab under a new name",
                &["secondary-shift-s"],
            ),
        ],
    },
    Group {
        title: "Running SQL",
        rows: &[
            (
                "Run the selection, or the statement at the caret",
                &["secondary-enter"],
            ),
            (
                "Run every statement in the buffer",
                &["secondary-shift-enter"],
            ),
            (
                "Show the statement's plan without running it",
                &["secondary-e"],
            ),
            (
                "Show the plan and run the statement for actual times",
                &["secondary-shift-e"],
            ),
            ("Give up on a running query", &["secondary-."]),
        ],
    },
    Group {
        title: "Result grid",
        rows: &[
            ("Copy the selected cell or rows", &["secondary-c"]),
            ("Copy the rows with a header line", &["secondary-shift-c"]),
            ("Open the cell's value in a dialog", &["secondary-shift-v"]),
            ("Pick the focused row", &["space"]),
            ("Extend the picked rows", &["shift-up", "shift-down"]),
            ("Pick every row", &["secondary-a"]),
            ("Unpick every row", &["secondary-shift-a", "escape"]),
        ],
    },
    Group {
        title: "Editing a table",
        rows: &[
            ("Edit the selected cell", &["enter"]),
            ("Set the selected cell to NULL", &["secondary-shift-n"]),
            ("Add a row to fill in", &["secondary-shift-i"]),
            ("Mark rows for deletion", &["secondary-backspace"]),
            ("Take the deletion mark off", &["secondary-shift-backspace"]),
            ("Apply staged changes", &["secondary-s"]),
            ("Discard staged changes", &["secondary-z"]),
            ("Toggle the row panel", &["secondary-\\"]),
            ("Save the value dialog", &["secondary-enter"]),
            ("Give up on a cell edit or dialog", &["escape"]),
        ],
    },
];

/// Show the shortcut list over the active window.
pub fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }

    window.open_dialog(cx, |dialog, _window, _cx| {
        dialog
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .title("Keyboard shortcuts")
            .overlay_closable(true)
            .keyboard(true)
            .child(body())
    });
}

fn body() -> impl IntoElement {
    div()
        .id("shortcuts")
        .size_full()
        .overflow_y_scroll()
        .child(v_flex().gap_4().children(GROUPS.iter().map(group)))
}

fn group(group: &Group) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::BOLD)
                .child(group.title),
        )
        .children(group.rows.iter().map(|(what, keys)| row(what, keys)))
}

fn row(what: &'static str, keys: &'static [&'static str]) -> impl IntoElement {
    h_flex()
        .justify_between()
        .gap_4()
        .text_sm()
        .child(div().child(what))
        .child(h_flex().flex_none().gap_1().items_center().children(
            keys.iter().enumerate().flat_map(|(ix, keys)| {
                let separator = (ix > 0).then(|| div().text_xs().child("or").into_any_element());
                let stroke = Keystroke::parse(keys)
                    .unwrap_or_else(|_| panic!("bad keystroke {keys:?} in the shortcut list"));
                separator
                    .into_iter()
                    .chain([Kbd::new(stroke).into_any_element()])
            }),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_keystroke_parses() {
        for group in GROUPS {
            for (what, keys) in group.rows {
                for keys in *keys {
                    assert!(Keystroke::parse(keys).is_ok(), "{what}: {keys:?}");
                }
            }
        }
    }
}
