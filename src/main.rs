mod app;
mod db;
mod keymap;
mod menu;
mod settings;
mod ui;

use std::borrow::Cow;

use gpui_kit::component::{Root, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, AssetSource, Bounds, Result, SharedString, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};

use crate::app::Workspace;

// The default component bundle carries only the 101 icons the components
// themselves use, so every wider-Lucide icon the app names has to be listed
// here or it renders as nothing.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        RefreshCw,
        FilePlus,
        DatabasePlus,
        Settings,
        Table,
        Route,
        Gauge,
        Braces,
        ChevronsDownUp,
    ]
);

#[derive(Clone, Copy, Default)]
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            // Themes first: a theme named in the settings has to be in the
            // registry before the settings are applied.
            settings::load_builtin_themes(cx);
            settings::load_user_themes(cx);
            settings::init(cx);
            keymap::bind(cx);
            menu::init(cx);
            ui::settings_window::init(cx);

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Keep the OS-level title for the window menu; the bar itself
                // is ours to draw, alongside the traffic lights.
                titlebar: Some(TitlebarOptions {
                    title: Some("Zippa DB".into()),
                    ..TitleBar::title_bar_options()
                }),
                // `app_owns_titlebar_drag` is what stops AppKit fighting the
                // bar's own `start_window_move`.
                ..TitleBar::window_options()
            };

            cx.open_window(options, |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("failed to open window");

            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_the_app_names_beyond_the_default_bundle_is_embedded() {
        // `Assets` carries only the 101 component icons, so an icon outside
        // that set that is not named in `icon_assets!` silently draws nothing.
        for path in [
            "icons/table.svg",
            "icons/route.svg",
            "icons/gauge.svg",
            "icons/braces.svg",
            "icons/chevrons-down-up.svg",
        ] {
            let loaded = AppAssets
                .load(path)
                .expect("loading an asset should not fail");
            assert!(loaded.is_some(), "{path} is used but not embedded");
        }
    }
}
