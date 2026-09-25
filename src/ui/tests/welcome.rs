//! The connection manager: empty state, cards, and the editor's round trips.

use super::*;

use gpui_kit::component::{Theme, ThemeRegistry};

#[gpui_kit::test]
fn the_empty_state_offers_new_connection_and_open_file(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");
    welcome.update(cx, |welcome, cx| {
        welcome.set_connections_for_test(vec![], cx)
    });

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        assert!(window.find("empty-new-connection").visible());
        assert!(window.find("open-sqlite-file").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_a_connection_persists_its_colour(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    crate::db::store::set_config_dir_for_test(dir.path.clone());

    let handle = workspace(cx);
    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    let config = ConnectionConfig {
        name: "Prod DB".into(),
        color: Some(TagColor::Red),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    let id = config.id;

    handle
        .update(cx, |_, window, cx| {
            welcome.update(cx, |welcome, cx| {
                welcome.save_for_test(config.clone(), None, window, cx)
            })
        })
        .unwrap();

    // The write goes to the background executor; let it land before reading it
    // back from the scratch dir.
    cx.run_until_parked();

    // The launcher holds it, and the store wrote it to the scratch dir.
    let color = welcome.update(cx, |welcome, _| {
        let saved = welcome
            .connections_for_test()
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .expect("the connection should be in the launcher");
        saved.color
    });
    assert_eq!(color, Some(TagColor::Red));

    let loaded = crate::db::store::load().expect("the connection should be on disk");
    assert!(
        loaded
            .iter()
            .any(|c| c.id == id && c.color == Some(TagColor::Red))
    );
}

/// The footgun IDEAS.md's safety nudge exists to catch: a card for a
/// production-marked, auto-applying connection asks before connecting rather
/// than opening straight away, the way an ordinary card does.
#[gpui_kit::test]
fn connecting_to_a_production_auto_apply_connection_asks_first(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let database = runtime::block_on(TempDatabase::new());
    let config = ConnectionConfig {
        color: Some(TagColor::Red),
        safety: SafetyMode::AutoApply,
        ..database.config()
    };
    let id = config.id;

    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");
    welcome.update(cx, |welcome, cx| {
        welcome.set_connections_for_test(vec![config], cx)
    });

    click_workspace(
        cx,
        &handle,
        gpui_kit::SharedString::from(format!("connect-{id}")),
    );

    assert!(
        workspace_dialog_open(cx, &handle),
        "a production connection set to auto-apply should ask before connecting"
    );
    assert!(
        handle
            .update(cx, |workspace, _, _| workspace.active_session_for_test())
            .unwrap()
            .is_none(),
        "connecting should wait for confirmation"
    );

    click_workspace(cx, &handle, "ok");

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace.active_session_for_test())
            .unwrap()
            .is_some(),
        "confirming should connect"
    );
}

#[gpui_kit::test]
fn duplicating_a_connection_uses_a_new_id(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    crate::db::store::set_config_dir_for_test(dir.path.clone());

    let handle = workspace(cx);
    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    let config = ConnectionConfig {
        name: "Original".into(),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    let id = config.id;
    welcome.update(cx, |welcome, cx| {
        welcome.set_connections_for_test(vec![config], cx)
    });

    handle
        .update(cx, |_, window, cx| {
            welcome.update(cx, |welcome, cx| welcome.duplicate_for_test(id, window, cx))
        })
        .unwrap();

    let ids = welcome.update(cx, |welcome, _| {
        welcome
            .connections_for_test()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>()
    });
    assert_eq!(ids.len(), 2, "duplicating should add a second connection");
    assert!(ids.contains(&id), "the original should stay");
    assert_eq!(
        ids.iter().filter(|other| **other == id).count(),
        1,
        "the duplicate should have its own id"
    );
}

#[gpui_kit::test]
fn deleting_a_connection_removes_it_and_persists(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    crate::db::store::set_config_dir_for_test(dir.path.clone());

    let handle = workspace(cx);
    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    let config = ConnectionConfig {
        name: "Doomed".into(),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    let id = config.id;
    welcome.update(cx, |welcome, cx| {
        welcome.set_connections_for_test(vec![config], cx)
    });

    // The card's `…` menu asks first via `open_alert_dialog` (covered by the
    // workspace close-confirmation test); this is what its Delete button runs.
    handle
        .update(cx, |_, window, cx| {
            welcome.update(cx, |welcome, cx| {
                welcome.delete_confirmed_for_test(id, window, cx)
            })
        })
        .unwrap();

    assert_eq!(
        welcome.update(cx, |welcome, _| welcome.connections_for_test().len()),
        0,
        "deleting should remove the connection"
    );
    cx.run_until_parked();
    assert!(
        crate::db::store::load()
            .expect("the store should be readable")
            .iter()
            .all(|c| c.id != id),
        "the deleted connection should not be on disk"
    );
}

#[gpui_kit::test]
fn editing_a_connection_leaves_its_stored_password_alone(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    crate::db::store::set_config_dir_for_test(dir.path.clone());

    let handle = workspace(cx);
    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    let config = ConnectionConfig {
        name: "Prod DB".into(),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    let id = config.id;
    welcome.update(cx, |welcome, cx| {
        welcome.set_connections_for_test(vec![config], cx)
    });

    // Open the editor on the saved connection, the way its card's `…` menu
    // does. `update_window` rather than `handle.update`: opening a dialog
    // updates the window's `Root`, which the latter already holds.
    cx.update_window(handle.window.into(), |_, window, cx| {
        welcome.update(cx, |welcome, cx| welcome.edit_for_test(id, window, cx));
    })
    .unwrap();
    assert!(
        welcome.update(cx, |welcome, _| welcome.editor_open_for_test()),
        "the editor should be open on the saved connection"
    );

    // The stored password is never loaded into the form, so an edit that was
    // not about the password has to say "leave it alone" rather than "write an
    // empty one", which would delete it.
    assert_eq!(
        welcome.update(cx, |welcome, cx| welcome
            .editor_password_intent_for_test(cx)),
        None,
        "an untouched password box must not claim a password to write"
    );

    // Typing one still reports it, so the box keeps working.
    draw_workspace(cx, &handle);
    welcome
        .downgrade()
        .update_in(cx, |welcome, window, cx| {
            welcome.focus_editor_password_for_test(window, cx)
        })
        .unwrap();
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.input("hunter2", cx);
    })
    .unwrap();

    assert_eq!(
        welcome.update(cx, |welcome, cx| welcome
            .editor_password_intent_for_test(cx)),
        Some("hunter2".to_string()),
        "typing in the box reports the new password"
    );
}

#[gpui_kit::test]
fn editing_a_connection_keeps_its_last_connected_stamp(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    crate::db::store::set_config_dir_for_test(dir.path.clone());

    let handle = workspace(cx);
    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    let when = "2026-01-02T03:04:05Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .expect("a timestamp");
    let config = ConnectionConfig {
        name: "Prod DB".into(),
        last_connected: Some(when),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    let id = config.id;
    welcome.update(cx, |welcome, cx| {
        welcome.set_connections_for_test(vec![config], cx)
    });

    // The editor builds its config fresh from the form, so it carries no stamp;
    // saving must not let that wipe the stored one and drop the card out of
    // most-recent-first order.
    let mut edited = ConnectionConfig::new(Engine::Postgres);
    edited.id = id;
    edited.name = "Prod DB (edited)".into();
    assert!(edited.last_connected.is_none());
    cx.update_window(handle.window.into(), |_, window, cx| {
        welcome.update(cx, |welcome, cx| {
            welcome.save_for_test(edited, None, window, cx)
        });
    })
    .unwrap();

    let saved = welcome.read_with(cx, |welcome, _| welcome.connections_for_test().to_vec());
    let saved = saved
        .iter()
        .find(|saved| saved.id == id)
        .expect("the edited connection should be saved");
    assert_eq!(saved.name, "Prod DB (edited)");
    assert_eq!(
        saved.last_connected,
        Some(when),
        "editing a connection is not connecting to it, so the stamp survives"
    );
}

#[gpui_kit::test]
fn tag_colors_contrast_in_every_bundled_theme(cx: &mut TestAppContext) {
    cx.update(|cx| {
        init_ui(cx);
        crate::settings::load_builtin_themes(cx);
    });

    cx.update(|cx| {
        let themes = ThemeRegistry::global(cx)
            .themes()
            .values()
            .cloned()
            .collect::<Vec<_>>();

        for config in themes {
            let mode = config.mode;
            {
                let theme = Theme::global_mut(cx);
                theme.mode = mode;
                theme.apply_config(&config);
            }
            let theme = Theme::global(cx);

            for color in TagColor::ALL {
                let background = color.hsla_on(theme);
                let foreground = color.on_color_for(theme);
                let ratio = crate::ui::contrast(background, foreground);
                assert!(
                    ratio >= 4.5,
                    "{} {} text ({:.2}:1) falls below 4.5:1",
                    theme.theme_name(),
                    color.label(),
                    ratio
                );
            }
        }
    });
}

#[gpui_kit::test]
fn engine_marks_read_in_every_bundled_theme(cx: &mut TestAppContext) {
    cx.update(|cx| {
        init_ui(cx);
        crate::settings::load_builtin_themes(cx);
    });

    cx.update(|cx| {
        let themes = ThemeRegistry::global(cx)
            .themes()
            .values()
            .cloned()
            .collect::<Vec<_>>();

        for config in themes {
            let mode = config.mode;
            {
                let theme = Theme::global_mut(cx);
                theme.mode = mode;
                theme.apply_config(&config);
            }
            let theme = Theme::global(cx);

            // The official colours are picked for light surfaces; a dark theme
            // is allowed to move one in lightness, but never below the floor
            // for a mark that is only a shape.
            for engine in Engine::ALL {
                let mark = crate::ui::engine_color(engine, cx);
                let ratio = crate::ui::contrast(mark, theme.background);
                assert!(
                    ratio >= 3.0,
                    "{}'s mark on {} reads at {:.2}:1, below 3:1",
                    engine.label(),
                    theme.theme_name(),
                    ratio
                );
            }
        }
    });
}
