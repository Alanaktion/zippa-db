mod app;
mod db;
mod keymap;
mod settings;
mod ui;

use gpui_kit::component::Root;
use gpui_kit::prelude::*;
use gpui_kit::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::app::Workspace;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            // Themes first: a theme named in the settings has to be in the
            // registry before the settings are applied.
            settings::load_user_themes(cx);
            settings::init(cx);
            keymap::bind(cx);
            ui::settings_window::init(cx);

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Zippa DB".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            cx.open_window(options, |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("failed to open window");

            cx.activate(true);
        });
}
