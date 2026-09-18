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
fn saving_a_connection_persists_its_tag_and_colour(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    crate::db::store::set_config_dir_for_test(dir.path.clone());

    let handle = workspace(cx);
    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    let config = ConnectionConfig {
        name: "Prod DB".into(),
        tag: Some("Production".into()),
        color: Some(TagColor::Red),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    let id = config.id;

    handle
        .update(cx, |_, window, cx| {
            welcome.update(cx, |welcome, cx| {
                welcome.save_for_test(config.clone(), "", window, cx)
            })
        })
        .unwrap();

    // The launcher holds it, and the store wrote it to the scratch dir.
    let (tag, color) = welcome.update(cx, |welcome, _| {
        let saved = welcome
            .connections_for_test()
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .expect("the connection should be in the launcher");
        (saved.tag, saved.color)
    });
    assert_eq!(tag.as_deref(), Some("Production"));
    assert_eq!(color, Some(TagColor::Red));

    let loaded = crate::db::store::load().expect("the connection should be on disk");
    assert!(loaded.iter().any(|c| {
        c.id == id && c.tag.as_deref() == Some("Production") && c.color == Some(TagColor::Red)
    }));
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
    assert!(
        crate::db::store::load()
            .expect("the store should be readable")
            .iter()
            .all(|c| c.id != id),
        "the deleted connection should not be on disk"
    );
}

#[gpui_kit::test]
fn tag_colors_contrast_in_every_bundled_theme(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
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
