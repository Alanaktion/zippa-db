//! The window's shape that outlives a session: where the window sits, the
//! splits the user dragged, and whether the sidebar is hidden.
//!
//! The split sizes are app-wide, not per session, so a connection opened
//! tomorrow gets the sidebar width chosen today. They live in one global,
//! changed only by a finished drag or the sidebar toggle; the workspace folds
//! the global into `workspace.json` at its usual checkpoints and on quit,
//! so a drag never writes the file by itself.

use gpui_kit::component::{ResizablePanelEvent, ResizableState};
use gpui_kit::prelude::*;
use gpui_kit::{App, Bounds, Context, Entity, Global, Pixels, WindowBounds, point, px, size};

use crate::workspace_state::{Layout, WindowPlacement};

/// The window can be no smaller than this: below it the sidebar, the editor
/// toolbar, and the footer start to overlap.
pub const MIN_WINDOW: (f32, f32) = (800., 500.);

/// How much of a saved window must still be on a display for it to be put
/// back there; less than this and it opens centred instead, so a window
/// saved on a monitor that is gone cannot open out of reach.
const MIN_VISIBLE: f32 = 120.;

struct Current(Layout);

impl Global for Current {}

/// The layout as it stands. Before the workspace has put one in place (a
/// view built on its own in a test), the defaults.
pub fn current(cx: &App) -> Layout {
    cx.try_global::<Current>()
        .map(|current| current.0)
        .unwrap_or_default()
}

/// Put the layout read from `workspace.json` in place.
pub fn init(layout: Layout, cx: &mut App) {
    cx.set_global(Current(layout));
}

/// Change the layout. The next workspace checkpoint records it.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Layout)) {
    let mut layout = current(cx);
    change(&mut layout);
    cx.set_global(Current(layout));
}

/// One of the splits [`Layout`] remembers: which field, and which panel of
/// its group carries the size.
#[derive(Clone, Copy, Debug)]
pub enum Split {
    /// The first panel of `session-columns`.
    Sidebar,
    /// The second panel of `session-panes`.
    Results,
    /// The second panel of `session-agents`.
    Agents,
}

impl Split {
    fn panel(self) -> usize {
        match self {
            Self::Sidebar => 0,
            Self::Results | Self::Agents => 1,
        }
    }

    /// The remembered size, or `default` when there is none yet.
    pub fn size(self, default: Pixels, cx: &App) -> Pixels {
        let layout = current(cx);
        let saved = match self {
            Self::Sidebar => layout.sidebar,
            Self::Results => layout.results,
            Self::Agents => layout.agents,
        };
        saved.map(px).unwrap_or(default)
    }

    fn record(self, size: f32, cx: &mut App) {
        update(cx, |layout| {
            let slot = match self {
                Self::Sidebar => &mut layout.sidebar,
                Self::Results => &mut layout.results,
                Self::Agents => &mut layout.agents,
            };
            *slot = Some(size);
        });
    }
}

/// A fresh split state whose finished drags are remembered as `split`.
///
/// The kit emits `Resized` only when a drag ends (not as the window resizes
/// around it), so this records what the user chose and nothing else.
pub fn tracked<T: 'static>(split: Split, cx: &mut Context<T>) -> Entity<ResizableState> {
    let state = cx.new(|_| ResizableState::default());
    cx.subscribe(&state, move |_, state, _: &ResizablePanelEvent, cx| {
        let size = state.read(cx).sizes().get(split.panel()).copied();
        if let Some(size) = size {
            split.record(f32::from(size), cx);
        }
    })
    .detach();
    state
}

/// What to record for a window's bounds.
pub fn placement(bounds: WindowBounds) -> WindowPlacement {
    let (rect, maximized) = match bounds {
        WindowBounds::Windowed(rect) => (rect, false),
        // Fullscreen is not put back on its own: reopening into it is more
        // surprising than useful, so it is remembered as maximised.
        WindowBounds::Maximized(rect) | WindowBounds::Fullscreen(rect) => (rect, true),
    };
    WindowPlacement {
        x: f32::from(rect.origin.x),
        y: f32::from(rect.origin.y),
        width: f32::from(rect.size.width),
        height: f32::from(rect.size.height),
        maximized,
    }
}

/// The bounds to open the window at: `saved`, if enough of it is still on
/// one of `displays`, grown to the minimum size; otherwise `None`, which
/// opens the default window.
pub fn restore(
    saved: Option<WindowPlacement>,
    displays: &[Bounds<Pixels>],
) -> Option<WindowBounds> {
    let saved = saved?;
    let finite = [saved.x, saved.y, saved.width, saved.height]
        .iter()
        .all(|value| value.is_finite());
    if !finite || saved.width <= 0. || saved.height <= 0. {
        return None;
    }
    let rect = Bounds {
        origin: point(px(saved.x), px(saved.y)),
        size: size(
            px(saved.width.max(MIN_WINDOW.0)),
            px(saved.height.max(MIN_WINDOW.1)),
        ),
    };
    let visible = displays.iter().any(|display| {
        let overlap = rect.intersect(display);
        f32::from(overlap.size.width) >= MIN_VISIBLE
            && f32::from(overlap.size.height) >= MIN_VISIBLE
    });
    if !visible {
        return None;
    }
    Some(if saved.maximized {
        WindowBounds::Maximized(rect)
    } else {
        WindowBounds::Windowed(rect)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1920.), px(1080.)),
        }
    }

    fn saved(x: f32, y: f32, width: f32, height: f32) -> WindowPlacement {
        WindowPlacement {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    #[test]
    fn a_window_on_screen_reopens_where_it_was() {
        let bounds = restore(Some(saved(100., 80., 1200., 700.)), &[display()]);
        assert_eq!(
            placement(bounds.expect("on screen")),
            saved(100., 80., 1200., 700.)
        );
    }

    #[test]
    fn a_maximised_window_reopens_maximised() {
        let mut state = saved(10., 10., 1000., 600.);
        state.maximized = true;
        assert!(matches!(
            restore(Some(state), &[display()]),
            Some(WindowBounds::Maximized(_))
        ));
    }

    #[test]
    fn a_window_off_every_display_opens_the_default() {
        // The second monitor it was on is gone.
        assert_eq!(
            restore(Some(saved(2400., 100., 1200., 700.)), &[display()]),
            None
        );
        // Only a sliver still shows.
        assert_eq!(
            restore(Some(saved(1880., 100., 1200., 700.)), &[display()]),
            None
        );
        assert_eq!(restore(None, &[display()]), None);
    }

    #[test]
    fn a_tiny_or_broken_window_is_grown_or_dropped() {
        let bounds = restore(Some(saved(0., 0., 200., 100.)), &[display()]).expect("on screen");
        let placed = placement(bounds);
        assert_eq!((placed.width, placed.height), MIN_WINDOW);
        assert_eq!(
            restore(Some(saved(f32::NAN, 0., 900., 600.)), &[display()]),
            None
        );
        assert_eq!(restore(Some(saved(0., 0., 0., 600.)), &[display()]), None);
    }
}
