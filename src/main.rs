#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod app;
mod db;
mod keymap;
mod menu;
mod settings;
mod ui;
mod workspace_state;

use std::borrow::Cow;

use gpui_kit::component::{Root, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, AssetSource, Bounds, Result, SharedString, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};

use crate::app::Workspace;
use crate::ui::EngineLogos;

// The default component bundle carries only the 101 icons the components
// themselves use, so every wider-Lucide icon the app names has to be listed
// here or it renders as nothing. `every_icon_the_ui_names_is_embedded` (below)
// reads the references back out of the source and fails when one is missing,
// so this list does not have to be kept in step by hand.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        RefreshCw,
        FilePlus,
        DatabasePlus,
        Table,
        ListTree,
        Route,
        Gauge,
        Braces,
        ChevronsDownUp,
        Pencil,
        Trash,
        Columns3,
        Key,
        SquareFunction,
        Zap,
        Lock,
        ScrollText,
        Activity,
        SlidersHorizontal,
        Import,
        Binoculars,
        Wrench,
    ]
);

#[derive(Clone, Copy, Default)]
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => match EngineLogos.load(path)? {
                Some(bytes) => Ok(Some(bytes)),
                None => gpui_kit::assets::Assets.load(path),
            },
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(EngineLogos.list(path)?);
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

                // The last save before the process ends: a checkpoint on every
                // change covers most of it, but text typed since the last one
                // would be lost, so the final buffer is written here rather
                // than left to the background task that is about to be torn
                // down.
                let quitting = workspace.clone();
                cx.on_app_quit(move |cx| {
                    quitting.update(cx, |workspace, cx| workspace.flush_state(cx));
                    std::future::ready(())
                })
                .detach();

                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("failed to open window");

            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use gpui_kit::assets::IconName;
    use regex::Regex;

    /// Every `.rs` file under `src/`, each with its path relative to the crate
    /// root and its text.
    fn sources() -> Vec<(String, String)> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
            let entries = fs::read_dir(dir).expect("a source directory should be readable");
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    let name = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    let text = fs::read_to_string(&path).expect("a source file should be UTF-8");
                    out.push((name, text));
                }
            }
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        walk(&root.join("src"), &root, &mut files);
        files
    }

    /// Whether `AppAssets` serves an icon at `path` — the same question a
    /// render asks, so an unbundled icon fails here instead of drawing nothing.
    fn is_embedded(path: &str) -> bool {
        matches!(AppAssets.load(path), Ok(Some(_)))
    }

    #[test]
    fn every_icon_the_ui_names_is_embedded() {
        // `gpui_kit::assets::Assets` carries only the icons the components
        // themselves use, and `icon_assets!`/`EngineLogos` name the rest one by
        // one — so an icon the app names that is in neither silently draws
        // nothing. A hand-kept list of what is used drifts the moment a new icon
        // is named, so the references are read back out of the source instead:
        // every `IconName::Variant` (or `AssetIcon::Variant`) resolved through
        // the full catalog, and every bare `icons/….svg` path.
        let catalog: BTreeMap<String, String> = IconName::ALL
            .iter()
            .map(|name| (format!("{name:?}"), (*name).path().to_string()))
            .collect();

        // Icon-name enums are `IconName` and aliases such as `AssetIcon`; each
        // ends in `IconName` or `Icon`. Matching only those suffixes keeps
        // unrelated `Enum::Variant` pairs — `CatalogKind::Table`, say — out of
        // the scan.
        let variant = Regex::new(r"([A-Za-z][A-Za-z0-9_]*)::([A-Za-z][A-Za-z0-9_]*)").unwrap();
        let bare_path = Regex::new(r#""(icons/[^"]+\.svg)""#).unwrap();

        let mut missing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (file, text) in sources() {
            for caps in variant.captures_iter(&text) {
                let icon_enum = caps[1].ends_with("IconName") || caps[1].ends_with("Icon");
                if icon_enum
                    && let Some(path) = catalog.get(&caps[2])
                    && !is_embedded(path)
                {
                    missing
                        .entry(path.clone())
                        .or_default()
                        .insert(format!("{file}: IconName::{}", &caps[2]));
                }
            }
            for caps in bare_path.captures_iter(&text) {
                if !is_embedded(&caps[1]) {
                    missing
                        .entry(caps[1].to_string())
                        .or_default()
                        .insert(file.clone());
                }
            }
        }

        assert!(
            missing.is_empty(),
            "{} icon(s) the UI names are not embedded and would draw nothing. \
             Add each to `icon_assets!` in src/main.rs, or to `EngineLogos` for \
             an engine brand mark:\n{}",
            missing.len(),
            missing
                .iter()
                .map(|(path, used_by)| format!(
                    "  {path}\n      named in {}",
                    used_by.iter().cloned().collect::<Vec<_>>().join(", ")
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
