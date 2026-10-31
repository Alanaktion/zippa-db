//! Reaching into the Agents panel from a test.

use futures::channel::oneshot;
use gpui_kit::{Context, Focusable as _};

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
                Item::Tool { name, .. } => format!("tool {name}"),
                Item::Notice { text, .. } => text.clone(),
            })
            .collect()
    }

    /// Tool items as (name, arguments, result, status), in order, where
    /// status is "waiting", "running", "done", "failed", or "stopped".
    pub(crate) fn tools_for_test(&self) -> Vec<(String, String, Option<String>, &'static str)> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Tool {
                    name,
                    arguments,
                    result,
                    status,
                    ..
                } => Some((
                    name.clone(),
                    arguments.clone(),
                    result.clone(),
                    match status {
                        super::ToolStatus::Waiting => "waiting",
                        super::ToolStatus::Running => "running",
                        super::ToolStatus::Done => "done",
                        super::ToolStatus::Failed => "failed",
                        super::ToolStatus::Stopped => "stopped",
                    },
                )),
                _ => None,
            })
            .collect()
    }

    /// Start a turn's items, as `start_turn` would: the user message, then
    /// the mark past it that bounds this turn's tool items.
    pub(crate) fn begin_turn_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.items.push(Item::User(text.to_string()));
        self.items_at_turn_start = self.items.len();
        cx.notify();
    }

    /// Apply one turn event, as the event listener would.
    pub(crate) fn apply_turn_event_for_test(
        &mut self,
        event: crate::db::agent::turn::TurnEvent,
        cx: &mut Context<Self>,
    ) {
        self.apply_turn_event(event, cx);
    }

    pub(crate) fn set_access_for_test(&mut self, access: AgentAccess, cx: &mut Context<Self>) {
        self.set_access(access, cx);
    }

    /// The modes a running turn's gates would read right now.
    pub(crate) fn live_modes_for_test(&self) -> (AgentAccess, crate::db::WriteApproval) {
        (self.live_modes.access(), self.live_modes.approval())
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

    pub(crate) fn say_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.items.push(Item::Assistant(text.to_string()));
        cx.notify();
    }

    /// The saved file this thread is written to, if it has one.
    pub(crate) fn thread_id_for_test(&self) -> Option<uuid::Uuid> {
        self.thread_id
    }

    /// Replace the thread with a saved one, as the Threads menu would.
    pub(crate) fn open_thread_for_test(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        self.open_thread(id, cx);
    }

    /// Delete the open thread's file, as the Threads menu would.
    pub(crate) fn delete_current_thread_for_test(&mut self, cx: &mut Context<Self>) {
        self.delete_current_thread(cx);
    }

    /// The messages area's vertical scroll offset: 0 at the top, negative
    /// going down, `-scroll_max_for_test` at the bottom.
    pub(crate) fn scroll_offset_for_test(&self) -> gpui_kit::Pixels {
        self.messages_scroll.offset().y
    }

    /// The farthest the messages area can scroll down, as last laid out.
    pub(crate) fn scroll_max_for_test(&self) -> gpui_kit::Pixels {
        self.messages_scroll.max_offset().y
    }

    /// Move the messages area, as a wheel or a drag would.
    pub(crate) fn set_scroll_offset_for_test(&mut self, y: gpui_kit::Pixels) {
        self.messages_scroll
            .set_offset(gpui_kit::point(gpui_kit::px(0.), y));
    }

    /// Type into the message box and put the focus there.
    pub(crate) fn type_for_test(
        &mut self,
        text: &str,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        self.composer
            .update(cx, |input, cx| input.set_value(text, window, cx));
        let handle = self.composer.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    pub(crate) fn composer_text_for_test(&self, cx: &gpui_kit::App) -> String {
        self.composer.read(cx).value().to_string()
    }

    pub(crate) fn set_running_for_test(&mut self, cx: &mut Context<Self>) {
        self.running = true;
        cx.notify();
    }

    /// Whether the render would draw the thread rather than the
    /// turned-off message.
    pub(crate) fn shows_thread_for_test(&self) -> bool {
        self.shows_thread()
    }

    /// Run `finish` for a turn outcome, as the turn task's listener would
    /// once its events drain.
    pub(crate) fn finish_for_test(
        &mut self,
        outcome: crate::db::agent::turn::TurnOutcome,
        cx: &mut Context<Self>,
    ) {
        let generation = self.generation;
        self.finish(generation, Ok(outcome), cx);
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
