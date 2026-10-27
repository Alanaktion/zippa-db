//! Reaching into the Agents panel from a test.

use futures::channel::oneshot;
use gpui_kit::Context;

use super::{AgentPanel, Item, Pending};
use crate::db::AgentAccess;
use crate::db::agent::tools::WriteRequest;

impl AgentPanel {
    /// Every line of the thread as text, in order.
    pub(crate) fn lines_for_test(&self) -> Vec<String> {
        self.items
            .iter()
            .map(|item| match item {
                Item::User(text) | Item::Assistant(text) => text.clone(),
                Item::Tool { event, .. } => format!("tool {}", event.name),
                Item::Notice { text, .. } => text.clone(),
            })
            .collect()
    }

    pub(crate) fn set_access_for_test(&mut self, access: AgentAccess, cx: &mut Context<Self>) {
        self.access = access;
        cx.notify();
    }

    /// Put a write in front of the user, as a turn would, and hand back what
    /// the answer will be sent to.
    pub(crate) fn offer_write_for_test(
        &mut self,
        request: WriteRequest,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<bool> {
        let (answer, response) = oneshot::channel();
        self.pending = Some(Pending { request, answer });
        cx.notify();
        response
    }

    pub(crate) fn set_running_for_test(&mut self, cx: &mut Context<Self>) {
        self.running = true;
        cx.notify();
    }

    pub(crate) fn pending_sql_for_test(&self) -> Option<String> {
        self.pending.as_ref().map(|p| p.request.sql.clone())
    }

    pub(crate) fn answer_for_test(&mut self, approved: bool, cx: &mut Context<Self>) {
        self.answer(approved, cx);
    }

    pub(crate) fn choose_approval_for_test(
        &mut self,
        approval: crate::db::WriteApproval,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        self.choose_approval(approval, window, cx);
    }
}
