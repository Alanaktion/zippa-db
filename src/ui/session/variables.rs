//! The query-variables dialog, opened from a query tab's toolbar.
//!
//! The dialog edits one tab's `:name` variables; saving stores them on the
//! tab's editor (and asks for a session checkpoint, so they persist) while
//! cancelling leaves them alone.

use gpui_kit::component::WindowExt;
use gpui_kit::prelude::*;
use gpui_kit::{AppContext, Context, Entity, Window, px};

use crate::db::params::Variable;

use super::panel::SessionPanel;
use super::{Session, SessionEvent};
use crate::ui::variables_dialog::{VariablesEvent, VariablesView};

impl Session {
    /// Build the variables dialog for `panel`, if one is not already open.
    pub(crate) fn open_variables_dialog(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.variables_dialog.is_some() {
            return;
        }
        let Some((editor, _)) = panel.read(cx).query_parts() else {
            return;
        };
        let variables = editor.read(cx).variables();

        let view = cx.new(|cx| VariablesView::new(variables, window, cx));
        cx.subscribe_in(&view, window, Self::on_variables_event)
            .detach();

        let session = cx.entity().downgrade();
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let session = session.clone();
            dialog
                .title("Query variables")
                .w(px(460.))
                // The dialog's own keys are off; `escape` is bound to
                // `CloseVariablesDialog` on the body instead.
                .keyboard(false)
                .on_close(move |_, _window, cx| {
                    session
                        .update(cx, |this, cx| {
                            this.variables_dialog = None;
                            cx.notify();
                        })
                        .ok();
                })
                .child(body.clone())
        });

        // Put the keyboard on the first name box so typing starts at once.
        view.update(cx, |view, cx| view.focus(window, cx));
        self.variables_dialog = Some((panel.clone(), view));
    }

    /// The dialog's own buttons: save the rows onto the tab, or drop them.
    fn on_variables_event(
        &mut self,
        _: &Entity<VariablesView>,
        event: &VariablesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            VariablesEvent::Dismissed => {
                self.variables_dialog = None;
                window.close_dialog(cx);
                cx.notify();
            }
            VariablesEvent::Saved(variables) => {
                if let Some((panel, _)) = self.variables_dialog.clone() {
                    self.apply_variables(&panel, variables.clone(), cx);
                }
                self.variables_dialog = None;
                window.close_dialog(cx);
                cx.notify();
            }
        }
    }

    /// Store `variables` on `panel`'s editor and checkpoint the session, so
    /// restoring keeps them.
    pub(crate) fn apply_variables(
        &mut self,
        panel: &Entity<SessionPanel>,
        variables: Vec<Variable>,
        cx: &mut Context<Self>,
    ) {
        if let Some((editor, _)) = panel.read(cx).query_parts() {
            editor.update(cx, |editor, cx| editor.set_variables(variables, cx));
            // The tab's state changed: checkpoint it for the session
            // restore, the way a run checkpoints the buffer.
            cx.emit(SessionEvent::Changed);
        }
    }
}
