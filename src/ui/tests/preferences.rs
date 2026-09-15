//! The settings window and what the settings change.
//!
//! Named for what it covers rather than `settings`, which is the name of the
//! module those settings live in.

use super::*;

#[gpui_kit::test]
fn a_table_opens_with_the_page_size_from_the_settings(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Settings {
            page_size: 25,
            ..Settings::default()
        })
    });

    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        assert_eq!(view.limit_for_test(), 25);
        assert_eq!(
            view.query(cx),
            "select * from items limit 25 offset 0",
            "the page size should reach the statement"
        );
    });
}

#[gpui_kit::test]
fn the_settings_window_is_opened_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);

    let opened = cx.update(|cx| {
        settings_window::open(cx);
        cx.windows().len()
    });
    assert_eq!(opened, 1, "the settings window did not open");

    let reopened = cx.update(|cx| {
        settings_window::open(cx);
        cx.windows().len()
    });
    assert_eq!(
        reopened, 1,
        "opening the settings again should raise the window already open"
    );
}

#[gpui_kit::test]
fn the_settings_window_shows_its_pages(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    cx.update(settings_window::open);

    let handle = cx
        .update(|cx| cx.windows().first().copied())
        .expect("the settings window did not open");

    cx.update_window(handle, |_, window, cx| {
        window.draw(cx).clear(cx);
        let settings = window.find("settings");
        assert!(settings.visible(), "the settings pages are not on screen");
        assert!(
            settings.bounds().size.width > px(0.),
            "the settings pages collapsed: {:?}",
            settings.bounds()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_toolbar_button_opens_the_settings(cx: &mut TestAppContext) {
    // The settings used to be reachable only by `Cmd`/`Ctrl` + `,`, which
    // nothing on screen mentions.
    let handle = workspace(cx);
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        1,
        "only the main window should be open to start with"
    );

    click(cx, &handle, "settings");
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        2,
        "the toolbar button should open the settings window"
    );
}

#[gpui_kit::test]
fn scrollbars_stay_on_screen_unless_the_setting_says_otherwise(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let _handle = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        Workspace::new(window, cx)
    });

    let mode = |cx: &mut TestAppContext| cx.update(|cx| Theme::global(cx).scrollbar_mode);

    assert_eq!(
        mode(cx),
        ScrollbarMode::Always,
        "a wide result is scrolled sideways by dragging its scrollbar, so it has to be there"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.always_show_scrollbars = false));
    assert_ne!(
        mode(cx),
        ScrollbarMode::Always,
        "turning the setting off should hand the decision back to the system"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.always_show_scrollbars = true));
    assert_eq!(mode(cx), ScrollbarMode::Always);
}

#[gpui_kit::test]
fn pinning_the_appearance_overrides_the_system(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    // The workspace is what subscribes to the window's appearance.
    let _handle = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        Workspace::new(window, cx)
    });

    let mode = |cx: &mut TestAppContext| cx.update(|cx| Theme::global(cx).mode);

    assert_eq!(
        mode(cx),
        ThemeMode::Light,
        "the default setting should follow the system, which reports light here"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.appearance = Appearance::Dark));
    assert_eq!(
        mode(cx),
        ThemeMode::Dark,
        "a pinned appearance should ignore the system"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.appearance = Appearance::Auto));
    assert_eq!(
        mode(cx),
        ThemeMode::Light,
        "back on auto, the system decides"
    );
}
