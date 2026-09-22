//! The brand mark and accent colour shown for each database engine.
//!
//! Both come from the design sheet in `assets/brand-colors.md` and the logos
//! in `assets/icons/engines/`: the colour an engine is officially known by, and
//! its logo as a monochrome silhouette drawn in that colour.

use std::borrow::Cow;

use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::{App, AssetSource, Hsla, Result, SharedString, rgb};

use crate::db::Engine;
use crate::ui::{contrast, relative_luminance};

/// The paths the three logos are served at. Each is embedded beside its path
/// in [`logo_bytes`], so the two stay in step.
const POSTGRES_LOGO: &str = "icons/engines/postgres.svg";
const MYSQL_LOGO: &str = "icons/engines/mysql.svg";
const SQLITE_LOGO: &str = "icons/engines/sqlite.svg";

const LOGO_PATHS: [&str; 3] = [POSTGRES_LOGO, MYSQL_LOGO, SQLITE_LOGO];

/// The smallest contrast a brand mark needs to read as a shape against a
/// surface — WCAG's 3:1 floor for non-text graphics.
const MIN_CONTRAST: f64 = 3.0;

/// How far the lightness moves per step while searching for a legible one.
const LIGHTNESS_STEP: f32 = 0.02;

/// The accent an engine is drawn with: its official brand colour.
///
/// Purely decorative — every place this is used also carries the engine's name
/// in text or its logo as a shape, so colour is never the only way to tell one
/// connection from another.
pub fn engine_color(engine: Engine, cx: &App) -> Hsla {
    readable_on(official_color(engine), cx.theme().background)
}

/// The engine's logo, a silhouette meant to be tinted with [`engine_color`].
pub fn engine_icon(engine: Engine) -> Icon {
    Icon::default().path(logo_path(engine))
}

/// The colour `assets/brand-colors.md` gives as the engine's official one,
/// exactly as written.
fn official_color(engine: Engine) -> Hsla {
    match engine {
        Engine::Postgres => rgb(0x4169E1).into(),
        Engine::MySql => rgb(0x4479A1).into(),
        Engine::Sqlite => rgb(0x003B57).into(),
    }
}

/// Move `color`'s lightness — hue and saturation kept — until it reads against
/// `background`; one that already reads comes back untouched.
///
/// The official colours are picked for light surfaces, and SQLite's near-black
/// navy is all but invisible on a dark theme. Moving away from the surface,
/// lightening on a dark one and darkening on a light one, keeps the brand's
/// hue while making the mark legible, and only ever goes as far as it has to.
fn readable_on(color: Hsla, background: Hsla) -> Hsla {
    if contrast(color, background) >= MIN_CONTRAST {
        return color;
    }

    let step = match relative_luminance(background) < 0.5 {
        true => LIGHTNESS_STEP,
        false => -LIGHTNESS_STEP,
    };
    let mut color = color;
    loop {
        let lightness = (color.l + step).clamp(0.0, 1.0);
        if lightness == color.l {
            return color;
        }
        color.l = lightness;
        if contrast(color, background) >= MIN_CONTRAST {
            return color;
        }
    }
}

/// The asset path an engine's logo is served at.
fn logo_path(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => POSTGRES_LOGO,
        Engine::MySql => MYSQL_LOGO,
        Engine::Sqlite => SQLITE_LOGO,
    }
}

/// The bytes of the logo served at `path`, or `None` for anything else.
fn logo_bytes(path: &str) -> Option<&'static [u8]> {
    let bytes: &'static [u8] = match path {
        POSTGRES_LOGO => include_bytes!("../../assets/icons/engines/postgres.svg"),
        MYSQL_LOGO => include_bytes!("../../assets/icons/engines/mysql.svg"),
        SQLITE_LOGO => include_bytes!("../../assets/icons/engines/sqlite.svg"),
        _ => return None,
    };
    Some(bytes)
}

/// The engine logos, embedded so the asset source can serve them at the paths
/// [`engine_icon`] asks for.
///
/// The default bundle carries only Lucide icons and `icon_assets!` names them
/// one by one, so brand marks the components know nothing about are served
/// from here.
#[derive(Clone, Copy, Default)]
pub struct EngineLogos;

impl AssetSource for EngineLogos {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(logo_bytes(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(LOGO_PATHS
            .iter()
            .copied()
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_engine_has_an_embedded_logo() {
        for engine in Engine::ALL {
            let path = logo_path(engine);
            let bytes = logo_bytes(path).unwrap_or_else(|| panic!("{path} is not embedded"));
            assert!(
                bytes.starts_with(b"<svg"),
                "{}'s logo should be an SVG",
                engine.label()
            );
        }
    }

    #[test]
    fn only_the_engine_logos_are_served() {
        assert!(logo_bytes("icons/table.svg").is_none());
        assert!(EngineLogos.list("icons/engines/").expect("a listing").len() == 3);
    }

    #[test]
    fn every_official_colour_is_used_verbatim_on_a_light_surface() {
        // The brand sheet's values are picked for light surfaces, so a light
        // theme should get them exactly as written.
        for engine in Engine::ALL {
            let official = official_color(engine);
            assert_eq!(
                official.to_rgb(),
                readable_on(official, Hsla::white()).to_rgb(),
                "{} should keep its official colour on a light surface",
                engine.label()
            );
        }
    }

    #[test]
    fn a_low_contrast_mark_is_moved_until_it_reads() {
        let background: Hsla = rgb(0x808080).into();
        let mark: Hsla = rgb(0x8A8A8A).into();
        assert!(
            contrast(mark, background) < MIN_CONTRAST,
            "the test's starting point should be unreadable"
        );
        assert!(contrast(readable_on(mark, background), background) >= MIN_CONTRAST);
    }

    #[test]
    fn a_dark_colour_is_lightened_on_a_dark_surface() {
        let navy = official_color(Engine::Sqlite);
        let background: Hsla = rgb(0x0D1016).into();
        let readable = readable_on(navy, background);

        assert!(readable.l > navy.l, "the navy should have been lightened");
        assert!(
            contrast(readable, background) >= MIN_CONTRAST,
            "the lightened mark still does not read"
        );
        assert_eq!(readable.h, navy.h, "the brand hue should be kept");
        assert!(readable.s >= navy.s - f32::EPSILON);
    }

    #[test]
    fn a_light_colour_is_darkened_on_a_light_surface() {
        let pale: Hsla = rgb(0xEEEEEE).into();
        let background = Hsla::white();
        let readable = readable_on(pale, background);

        assert!(
            readable.l < pale.l,
            "the pale mark should have been darkened"
        );
        assert!(contrast(readable, background) >= MIN_CONTRAST);
    }
}
