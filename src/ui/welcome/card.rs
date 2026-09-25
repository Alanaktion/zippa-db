//! One saved connection, rendered as a card on the welcome screen.
//!
//! The card is the connect target (a full-width [`Button`]) with a leading bar
//! in the tag colour, a title, the connection target, its engine and tag, and
//! a trailing `…` menu for the edit/duplicate/delete commands. The colour is
//! always accompanied by the tag's text, so it is never the only cue.

use chrono::{DateTime, Utc};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, Size, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, SharedString, div, px};
use uuid::Uuid;

use crate::db::{ConnectionConfig, TagColor};
use crate::ui::{engine_color, engine_icon};

use super::Welcome;

/// Render one connection card. All of its commands run on `this`.
pub(super) fn render_card(
    this: &Welcome,
    config: &ConnectionConfig,
    cx: &mut Context<Welcome>,
) -> gpui_kit::AnyElement {
    let id = config.id;
    let connecting = this.is_connecting(&id);
    let name = config.display_name();
    let engine = config.engine;
    let tag = config.tag.clone();
    let tag_color = config.color.map(|color| color.hsla(cx));
    let bar = tag_color.unwrap_or_else(|| cx.theme().muted);
    let weak = cx.entity().downgrade();
    // The click handler is `'static`, so it owns its copy rather than
    // borrowing the list this card is rendered from.
    let connect = config.clone();

    div()
        .id(SharedString::from(format!("card-{id}")))
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .overflow_hidden()
        .child(
            h_flex()
                .items_stretch()
                // The environment colour, four pixels down the leading edge;
                // the tag text beside it carries the same information.
                .child(div().flex_none().w(px(4.)).bg(bar))
                .child(
                    Button::new(SharedString::from(format!("connect-{id}")))
                        .ghost()
                        .flex_1()
                        .min_w_0()
                        // A fixed size would impose the button's own height and
                        // clip the multi-line card; a custom size only adds a
                        // little horizontal padding and lets the content set the
                        // row's height.
                        .with_size(Size::Size(px(0.)))
                        .disabled(connecting)
                        .accessibility_label(format!(
                            "{}, {}, {}",
                            name,
                            engine.label(),
                            tag_text(&tag, config.color)
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.connect_saved_with_confirmation(connect.clone(), window, cx)
                        }))
                        .child(card_body(config, connecting, cx)),
                )
                .child(h_flex().px_2().child(actions_button(id, &name, &weak))),
        )
        .into_any_element()
}

/// The text inside the connect button: name, target, and the meta line.
fn card_body(config: &ConnectionConfig, connecting: bool, cx: &App) -> impl IntoElement {
    v_flex()
        .w_full()
        .min_w_0()
        .gap_0p5()
        .py_2()
        .px_3()
        .child(div().text_sm().truncate().child(if connecting {
            "Connecting…".to_string()
        } else {
            config.display_name()
        }))
        .child(
            div()
                .text_xs()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(subtitle(config)),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_1()
                        // The engine's logo in its own colour; the label beside
                        // it names the engine, so the mark is never the only cue.
                        .child(
                            engine_icon(config.engine)
                                .flex_none()
                                .size_3p5()
                                .text_color(engine_color(config.engine, cx)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(config.engine.label()),
                        ),
                )
                .children(crate::ui::tag_chip(config.tag.as_deref(), config.color, cx))
                .when_some(config.last_connected, |this, when| {
                    this.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(connected_label(when, Utc::now())),
                    )
                }),
        )
}

/// The trailing `…` menu: Edit, Duplicate, Delete.
fn actions_button(
    id: Uuid,
    name: &str,
    welcome: &gpui_kit::WeakEntity<Welcome>,
) -> impl IntoElement {
    let welcome = welcome.clone();

    Button::new(SharedString::from(format!("actions-{id}")))
        .ghost()
        .xsmall()
        .icon(IconName::Ellipsis)
        .accessibility_label(format!("Actions for {name}"))
        .tooltip("Actions")
        .dropdown_menu(move |menu, _window, _cx| {
            let edit = welcome.clone();
            let duplicate = welcome.clone();
            let delete = welcome.clone();

            menu.item(PopupMenuItem::new("Edit").icon(IconName::Pencil).on_click(
                move |_, window, cx| {
                    if let Some(welcome) = edit.clone().upgrade() {
                        welcome.update(cx, |this, cx| this.edit(id, window, cx));
                    }
                },
            ))
            .item(
                PopupMenuItem::new("Duplicate")
                    .icon(IconName::Copy)
                    .on_click(move |_, window, cx| {
                        if let Some(welcome) = duplicate.clone().upgrade() {
                            welcome.update(cx, |this, cx| this.duplicate(id, window, cx));
                        }
                    }),
            )
            .item(PopupMenuItem::new("Delete").icon(IconName::Trash).on_click(
                move |_, window, cx| {
                    if let Some(welcome) = delete.clone().upgrade() {
                        welcome.update(cx, |this, cx| this.delete(id, window, cx));
                    }
                },
            ))
        })
}

/// `localhost:5432/app`, or the file path for SQLite, middle-truncated.
pub(super) fn subtitle(config: &ConnectionConfig) -> String {
    if config.engine.is_file_based() {
        middle_truncate(&config.database, 40)
    } else {
        format!("{}:{}/{}", config.host, config.port, config.database)
    }
}

/// What the accessibility label says about the tag, or "Untagged".
fn tag_text(tag: &Option<String>, color: Option<TagColor>) -> String {
    crate::ui::tag_text(tag.as_deref(), color).unwrap_or_else(|| "Untagged".to_string())
}

/// Keep both ends of a long path so the file name still reads.
pub(super) fn middle_truncate(path: &str, max_chars: usize) -> String {
    let count = path.chars().count();
    if count <= max_chars {
        return path.to_string();
    }
    let keep = (max_chars.saturating_sub(1)) / 2;
    let start: String = path.chars().take(keep).collect();
    let end: String = path
        .chars()
        .rev()
        .take(keep)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}…{end}")
}

/// "Connected 2 h ago", with the unit picked to stay short.
pub(super) fn connected_label(when: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = now.signed_duration_since(when).num_minutes();
    if minutes < 1 {
        "Connected just now".to_string()
    } else if minutes < 60 {
        format!("Connected {minutes} min ago")
    } else if minutes < 60 * 24 {
        format!("Connected {} h ago", minutes / 60)
    } else {
        format!("Connected {} d ago", minutes / (60 * 24))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_truncate_keeps_both_ends() {
        assert_eq!(middle_truncate("/short.db", 40), "/short.db");
        let path = "/a/very/long/path/to/a/database/file.sqlite";
        let truncated = middle_truncate(path, 20);
        assert!(truncated.chars().count() <= 20);
        assert!(truncated.ends_with("e.sqlite") || truncated.ends_with("file.sqlite"));
        assert!(truncated.contains('…'));
    }

    #[test]
    fn connected_label_picks_a_unit() {
        let now: DateTime<Utc> = "2024-01-02T03:04:05Z".parse().expect("a timestamp");
        assert_eq!(
            connected_label("2024-01-02T03:03:05Z".parse().expect("a timestamp"), now),
            "Connected 1 min ago"
        );
        assert_eq!(
            connected_label("2024-01-02T01:04:05Z".parse().expect("a timestamp"), now),
            "Connected 2 h ago"
        );
    }
}
