//! The Agents panel: a chat beside the session, scoped to its connection.
//!
//! The thread lives here. Chat history can hold schema, row data and pasted
//! secrets, so it is never written to `workspace.json`; with thread history
//! on (Settings › Agents) it is also saved per connection as `.jsonl` files
//! (see [`crate::db::agent::threads`]), listed in the header's Threads menu
//! by recency. The session owns one panel, so a thread follows its
//! connection through database switches and reconnects and ends when the
//! connection does.
//!
//! The panel makes no model call until the user sends a message. A send is a
//! [`AgentEvent::Send`] the session answers with the open tabs and the active
//! buffer ([`AgentPanel::start_turn`]); the turn itself runs on the database
//! runtime (`db::agent::turn`). A write the model asks for comes back as a
//! request the panel shows, with its exact SQL, and waits on.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use futures::future::BoxFuture;
use gpui_kit::assets::IconName;
use gpui_kit::base::TextView;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::{ScrollableElement, ScrollbarAxis};
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle,
    SharedString, Window, actions, div,
};

use crate::db::agent::compaction;
use crate::db::agent::context::{self, ContextInfo};
use crate::db::agent::provider::{Endpoint, HttpProvider, Message, ProviderError};
use crate::db::agent::threads;
use crate::db::agent::tools::{Approver, LiveModes, ToolContext, UiAction, WriteRequest};
use crate::db::agent::turn::{self, TurnEvent, TurnOutcome};
use crate::db::{
    AgentAccess, Connection, DatabaseObject, WriteApproval, is_risky_agent, runtime, store,
};
use crate::settings::Settings;
use crate::ui::completion::SharedCatalog;
use uuid::Uuid;

#[cfg(test)]
mod test_support;

/// Lines the message box grows to before it scrolls.
const COMPOSER_LINES: usize = 6;

actions!(zippa_db, [ToggleAgents, ApproveWrite, RejectWrite]);

pub enum AgentEvent {
    /// The user sent a message; the session answers with the context.
    Send(String),
    OpenTable(DatabaseObject),
    ProposeQuery(String),
    /// A turn's write changed the schema; the session re-reads its catalog.
    RefreshCatalog,
}

/// Where a tool item is in its life: triggered and waiting its turn,
/// running, finished (with a result), failed (with an error result), or
/// stopped without a result because the turn ended or the user stopped it.
/// The summary shows this in words, never colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolStatus {
    Waiting,
    Running,
    Done,
    Failed,
    Stopped,
}

impl ToolStatus {
    fn summary(self) -> &'static str {
        match self {
            ToolStatus::Waiting => " — waiting",
            ToolStatus::Running => " — running",
            ToolStatus::Done => "",
            ToolStatus::Failed => " — failed",
            ToolStatus::Stopped => " — stopped",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ToolStatus::Waiting => ", waiting",
            ToolStatus::Running => ", running",
            ToolStatus::Done => "",
            ToolStatus::Failed => ", failed",
            ToolStatus::Stopped => ", stopped",
        }
    }
}

/// One line of the thread as the panel draws it.
enum Item {
    User(String),
    Assistant(String),
    /// A tool call, drawn as soon as the model triggers it and filled in
    /// as it runs: `result` is `None` until it finishes. `index` is the
    /// call's position in the turn as a whole, counting across replies, so
    /// a placeholder, its running state, and its result address the same
    /// item.
    Tool {
        index: usize,
        name: String,
        arguments: String,
        result: Option<String>,
        status: ToolStatus,
        open: bool,
    },
    /// Something the panel itself says; `error` ones are prefixed in words.
    Notice {
        text: String,
        error: bool,
    },
}

/// A write waiting for Approve or Reject.
struct Pending {
    request: WriteRequest,
    answer: oneshot::Sender<bool>,
}

type Asks = mpsc::UnboundedSender<(WriteRequest, oneshot::Sender<bool>)>;

/// The turn's way of asking the panel, from the database runtime.
struct Bridge(Asks);

impl Approver for Bridge {
    fn approve(&self, request: WriteRequest) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let (answer, response) = oneshot::channel();
            if self.0.unbounded_send((request, answer)).is_err() {
                return false;
            }
            // A dropped answer — Stop, or the panel closing — is a no.
            response.await.unwrap_or(false)
        })
    }
}

pub struct AgentPanel {
    connection: Arc<Connection>,
    catalog: SharedCatalog,
    access: AgentAccess,
    approval: WriteApproval,
    /// The modes as currently shown, shared with a running turn: a
    /// write-approval change, or lowered access, reaches the turn's next
    /// tool call through this (see `ToolContext`).
    live_modes: LiveModes,
    /// What goes back to the model each turn, system prompt excluded.
    thread: Vec<Message>,
    /// Whether each tool result in `thread` was an error, one flag per
    /// [`Message::Tool`] in order, kept beside the thread rather than read
    /// back off the drawn items: a stopped turn's tools stay drawn but its
    /// messages never join the thread.
    tool_errors: Vec<bool>,
    /// The saved file this thread is written to, when thread history is on.
    /// `None` until the first save: a thread that never finished a turn
    /// has no file yet.
    thread_id: Option<Uuid>,
    items: Vec<Item>,
    /// Where `items` ended when the running turn's user message was pushed,
    /// so `finish` can tell whether the turn showed anything at all.
    items_at_turn_start: usize,
    composer: Entity<TextareaState>,
    running: bool,
    /// Bumped by every send and Stop, so a turn that was stopped cannot
    /// write into the thread it was cut from.
    generation: u64,
    abort: Option<tokio::task::AbortHandle>,
    tools_supported: bool,
    /// Whether the endpoint streams, as the panel last learned it. A turn
    /// that falls back to whole replies sets this false, so later turns do
    /// not waste a request asking for a stream that will be refused.
    stream_supported: bool,
    pending: Option<Pending>,
    focus: FocusHandle,
    /// The messages area's scroll state, so new output can follow the
    /// bottom while the user is already there.
    messages_scroll: ScrollHandle,
}

impl EventEmitter<AgentEvent> for AgentPanel {}

impl AgentPanel {
    pub fn new(
        connection: Arc<Connection>,
        catalog: SharedCatalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Enter sends and Shift+Enter starts a new line; the box grows with
        // what is typed, up to a few lines.
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, COMPOSER_LINES)
                .submit_on_enter(true)
                .placeholder("Ask about this database")
        });
        cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.send(window, cx);
                }
            },
        )
        .detach();
        Self {
            access: connection.config.agent_access,
            approval: connection.config.agent_approval,
            live_modes: LiveModes::new(
                connection.config.agent_access,
                connection.config.agent_approval,
            ),
            connection,
            catalog,
            thread: Vec::new(),
            tool_errors: Vec::new(),
            thread_id: None,
            items: Vec::new(),
            items_at_turn_start: 0,
            composer,
            running: false,
            generation: 0,
            abort: None,
            tools_supported: true,
            stream_supported: true,
            pending: None,
            focus: cx.focus_handle(),
            messages_scroll: ScrollHandle::default(),
        }
    }

    /// Point the panel at the connection a database switch or reconnect
    /// opened. The thread stays; the next turn re-snapshots the context.
    /// A different connection gets a different thread file, so the saved
    /// id is dropped rather than carried onto another connection's history.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        if connection.config.id != self.connection.config.id {
            self.thread_id = None;
        }
        self.connection = connection;
        cx.notify();
    }

    #[cfg(test)]
    pub fn access(&self) -> AgentAccess {
        self.access
    }

    #[cfg(test)]
    pub fn approval(&self) -> WriteApproval {
        self.approval
    }

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() || self.running || !self.access.enabled() {
            return;
        }
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(AgentEvent::Send(text));
    }

    /// Run a turn for `text`. Called by the session, which knows the tabs.
    pub fn start_turn(
        &mut self,
        text: String,
        open_tabs: Vec<String>,
        active_buffer: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.running || !self.access.enabled() {
            return;
        }
        self.items.push(Item::User(text.clone()));
        self.items_at_turn_start = self.items.len();
        // Sending is the user's own move to the bottom of the thread.
        self.messages_scroll.scroll_to_bottom();

        let settings = Settings::global(cx);
        let (base_url, model) = (
            settings.agent_base_url.clone(),
            settings.agent_model.clone(),
        );
        if base_url.is_empty() || model.is_empty() {
            self.notice(
                "Set the model endpoint and model name in Settings › Agents first.",
                true,
            );
            cx.notify();
            return;
        }

        let catalog = self.catalog.get();
        let prompt = context::system_prompt(&ContextInfo {
            engine: self.connection.config.engine,
            database: self.connection.database(),
            access: self.access,
            approval: self.approval,
            safety: self.connection.config.safety,
            catalog: &catalog,
            open_tabs,
            active_buffer,
        });
        // The thread as sent is the compacted one: old bulk tool results
        // become summaries before the model's context limit is hit, and a
        // thread that cannot shrink far enough is refused here, in words,
        // rather than failing at the endpoint. The panel's own thread
        // stays whole.
        let compacted = match compaction::compact(&prompt, &self.thread, &text) {
            compaction::Fit::Fits(compacted) => {
                if compacted.compacted > 0 {
                    self.notice(
                        "Earlier tool results were compacted to fit the model's context.",
                        false,
                    );
                }
                compacted.messages
            }
            compaction::Fit::TooBig => {
                self.notice(
                    "This thread is too long for the model even with earlier results compacted. \
                     Start a new thread with the New button above and continue there.",
                    true,
                );
                cx.notify();
                return;
            }
        };
        let mut history = vec![Message::System(prompt)];
        history.extend(compacted);

        let tool_context = ToolContext {
            connection: self.connection.clone(),
            catalog,
            access: self.access,
            live: self.live_modes.clone(),
        };
        let tools_supported = self.tools_supported;
        let stream_supported = self.stream_supported;
        let (asks, mut requests) = mpsc::unbounded();
        // Turn events arrive here as they happen — text fragments, tool
        // placeholders, running states, results — and the listener below
        // draws them at once, rather than waiting for the turn to finish.
        let (events_tx, mut events) = mpsc::unbounded();

        self.generation += 1;
        let generation = self.generation;
        self.running = true;

        let task = runtime::spawn(async move {
            let api_key = store::api_key().ok().flatten();
            let provider = match HttpProvider::new(Endpoint {
                base_url,
                model,
                api_key,
            }) {
                Ok(provider) => provider,
                Err(error) => {
                    return TurnOutcome {
                        error: Some(error),
                        ..TurnOutcome::default()
                    };
                }
            };
            turn::run_with_events(
                &provider,
                &tool_context,
                &Bridge(asks),
                &history,
                &text,
                tools_supported,
                stream_supported,
                Some(events_tx),
            )
            .await
        });
        self.abort = task.abort_handle();

        // Turn events arrive here until the turn ends and its sender is
        // dropped. A stopped turn's events are guarded by generation, like
        // the approval requests below. `finish` runs only after this loop
        // drains: finishing as soon as the turn task resolves could set
        // `running = false` before the final assistant delta was drawn,
        // and a send in that gap would drop the rest of the old turn.
        // The sender is dropped when the turn returns or is aborted, so
        // this loop always ends.
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let shown = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.apply_turn_event(event, cx);
                    true
                });
                if !matches!(shown, Ok(true)) {
                    break;
                }
            }
            let outcome = task.await;
            this.update(cx, |this, cx| this.finish(generation, outcome, cx))
                .ok();
        })
        .detach();

        // Requests the turn makes for approval arrive here until it ends and
        // its sender is dropped.
        cx.spawn(async move |this, cx| {
            while let Some((request, answer)) = requests.next().await {
                let shown = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.pending = Some(Pending { request, answer });
                    this.scroll_to_bottom_if_following();
                    cx.notify();
                    true
                });
                if !matches!(shown, Ok(true)) {
                    break;
                }
            }
        })
        .detach();

        cx.notify();
    }

    fn finish(
        &mut self,
        generation: u64,
        outcome: Result<TurnOutcome, tokio::sync::oneshot::error::RecvError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.running = false;
        self.abort = None;
        self.pending = None;
        let Ok(outcome) = outcome else {
            self.settle_tools("stopped");
            self.notice("Stopped.", false);
            cx.notify();
            return;
        };
        // A turn that ended without a tool's result — a stream failed
        // after its placeholder, say — leaves that item terminal, not
        // waiting or running for the rest of the thread.
        self.settle_tools("not run");

        // The turn's events are its tool results, one each and in the same
        // order as its `Message::Tool`s.
        let results = outcome
            .messages
            .iter()
            .filter(|message| matches!(message, Message::Tool { .. }))
            .count();
        let mut errors = outcome.events.iter().map(|event| event.is_error);
        self.tool_errors
            .extend((0..results).map(|_| errors.next().unwrap_or(false)));
        self.thread.extend(outcome.messages);
        self.save_thread(cx);
        if outcome.tools_unsupported {
            self.tools_supported = false;
            self.notice(
                "This endpoint does not support tool calls, so the assistant can only answer in \
                 words. SQL it writes can be copied into a query tab.",
                false,
            );
        }
        if outcome.stream_unsupported {
            // Silent: the next turn simply uses whole replies, and its
            // tool placeholders still arrive as the tools run.
            self.stream_supported = false;
        }
        // No items are pushed here: assistant text and tool calls already
        // arrived item by item through the turn-events listener above, and
        // pushing the outcome's copy again would duplicate them.
        if outcome.hit_limit {
            self.notice(
                "Stopped calling tools: the limit for one message was reached.",
                false,
            );
        }
        if let Some(error) = &outcome.error {
            if error.is_context_length() {
                // A provider error that only says the context ran out is
                // the failure compaction exists to avoid; say what to do
                // instead of repeating the endpoint's wording.
                self.notice(
                    "The model ran out of context for this thread. Start a new thread with the \
                     New button above and continue there.",
                    true,
                );
            } else {
                self.notice(&error_text(error), true);
            }
        }
        // A turn that answered with nothing and ran no tools would otherwise
        // vanish: no assistant item, no tool item, nothing to read. Say so,
        // distinctly from an error and from "Stopped." A notice pushed by
        // the turn itself — compaction, the tool limit — does not count:
        // it explains the turn, it is not the answer.
        if outcome.error.is_none()
            && outcome
                .answer
                .as_deref()
                .is_none_or(|answer| answer.trim().is_empty())
            && !self.items[self.items_at_turn_start.min(self.items.len())..]
                .iter()
                .any(|item| match item {
                    Item::Assistant(text) => !text.trim().is_empty(),
                    Item::Tool { .. } => true,
                    _ => false,
                })
        {
            self.notice("The assistant returned no answer.", false);
        }
        for action in outcome.ui {
            match action {
                UiAction::OpenTable(object) => cx.emit(AgentEvent::OpenTable(object)),
                UiAction::ProposeQuery(sql) => cx.emit(AgentEvent::ProposeQuery(sql)),
                UiAction::RefreshCatalog => cx.emit(AgentEvent::RefreshCatalog),
            }
        }
        cx.notify();
    }

    /// Draw one turn event at once. Text appends to the current assistant
    /// item (a new round starts a new one, because a tool item sits between
    /// rounds); a tool event finds its item by index, creating it if an
    /// event arrived out of order, so a placeholder, its running state, and
    /// its result never become three separate items.
    fn apply_turn_event(&mut self, event: TurnEvent, cx: &mut Context<Self>) {
        match event {
            TurnEvent::AssistantDelta(text) => {
                if text.is_empty() {
                    return;
                }
                match self.items.last_mut() {
                    Some(Item::Assistant(existing)) => existing.push_str(&text),
                    _ => self.items.push(Item::Assistant(text)),
                }
            }
            TurnEvent::ToolPending {
                index,
                name,
                arguments,
            } => {
                if self.tool_item_mut(index).is_none() {
                    self.items.push(Item::Tool {
                        index,
                        name,
                        arguments,
                        result: None,
                        status: ToolStatus::Waiting,
                        open: false,
                    });
                }
            }
            TurnEvent::ToolArguments { index, arguments } => {
                if let Some(Item::Tool {
                    arguments: existing,
                    ..
                }) = self.tool_item_mut(index)
                {
                    *existing = arguments;
                }
            }
            TurnEvent::ToolRunning { index } => {
                if let Some(Item::Tool { status, .. }) = self.tool_item_mut(index)
                    && *status == ToolStatus::Waiting
                {
                    *status = ToolStatus::Running;
                }
            }
            TurnEvent::ToolFinished {
                index,
                result,
                is_error,
            } => match self.tool_item_mut(index) {
                Some(Item::Tool {
                    result: existing,
                    status,
                    ..
                }) => {
                    *existing = Some(result);
                    *status = if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Done
                    };
                }
                _ => self.items.push(Item::Tool {
                    index,
                    name: String::new(),
                    arguments: String::new(),
                    result: Some(result),
                    status: if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Done
                    },
                    open: false,
                }),
            },
        }
        self.scroll_to_bottom_if_following();
        cx.notify();
    }

    /// The tool item for `index`, if one is already drawn. Tool indices
    /// restart at 0 each turn, so only this turn's items count: searching
    /// the whole thread would match an earlier turn's tool at the same
    /// index, and the new call would fill in that old item instead of
    /// drawing its own.
    fn tool_item_mut(&mut self, index: usize) -> Option<&mut Item> {
        let start = self.items_at_turn_start.min(self.items.len());
        self.items[start..].iter_mut().find(
            |item| matches!(item, Item::Tool { index: item_index, .. } if *item_index == index),
        )
    }

    /// Give every tool item that never finished a terminal state, with
    /// `reason` as its result if it has none. A placeholder from a failed
    /// stream is "not run"; a tool cut off by Stop is "stopped".
    fn settle_tools(&mut self, reason: &str) {
        for item in self.items.iter_mut() {
            if let Item::Tool { result, status, .. } = item
                && matches!(status, ToolStatus::Waiting | ToolStatus::Running)
            {
                *status = ToolStatus::Stopped;
                if result.is_none() {
                    *result = Some(reason.to_string());
                }
            }
        }
    }

    fn notice(&mut self, text: &str, error: bool) {
        self.items.push(Item::Notice {
            text: text.to_string(),
            error,
        });
        self.scroll_to_bottom_if_following();
    }

    /// Follow new output to the bottom, but only while the user is already
    /// there: scrolling up to read must not be yanked back by streaming.
    /// The handle lands on the bottom at the next prepaint, so it stays
    /// right even as the new content is still being laid out.
    fn scroll_to_bottom_if_following(&mut self) {
        let max = self.messages_scroll.max_offset().y;
        if self.messages_scroll.offset().y <= -max + gpui_kit::px(1.) {
            self.messages_scroll.scroll_to_bottom();
        }
    }

    /// Stop the model request and any tool call in flight.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.running {
            return;
        }
        self.generation += 1;
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
        // Dropping the answer is a rejection if a write was waiting.
        self.pending = None;
        self.running = false;
        self.settle_tools("stopped");
        self.notice("Stopped.", false);
        cx.notify();
    }

    /// Forget the conversation. The connection and settings stay.
    pub fn new_thread(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        self.thread.clear();
        self.tool_errors.clear();
        self.thread_id = None;
        self.items.clear();
        self.tools_supported = true;
        self.stream_supported = true;
        cx.notify();
    }

    /// Write the thread to its file, when thread history is on. A first
    /// save for the connection warns in the transcript that history is now
    /// kept on this machine, since it may hold pasted secrets. A failure
    /// says so in the transcript rather than silently dropping history.
    fn save_thread(&mut self, cx: &mut Context<Self>) {
        if !Settings::global(cx).agent_save_threads || self.thread.is_empty() {
            return;
        }
        let saved = threads::to_saved(&self.thread, self.tool_errors.iter().copied());
        let id = *self.thread_id.get_or_insert_with(Uuid::new_v4);
        // The directory is resolved here, not on the background thread, and
        // the ticket is drawn with the snapshot, so a slow write cannot land
        // over a later one.
        let dir = match threads::threads_dir(self.connection.config.id) {
            Ok(dir) => dir,
            Err(error) => {
                self.notice(&format!("Thread history was not saved: {error:#}"), true);
                return;
            }
        };
        let ticket = store::ticket();
        let write = cx.background_spawn(async move { threads::save(&dir, id, &saved, ticket) });
        cx.spawn(async move |panel, cx| {
            let result = write.await;
            panel
                .update(cx, |panel, cx| {
                    match result {
                        Ok(true) => panel.notice(
                            "Thread history is now saved on this machine, one file per thread, \
                             and may contain pasted secrets. The Threads menu lists them.",
                            false,
                        ),
                        Ok(false) => return,
                        Err(error) => {
                            panel.notice(&format!("Thread history was not saved: {error:#}"), true)
                        }
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Replace the thread with a saved one: messages and tool events come
    /// back intact, while a running turn or a waiting approval is not
    /// resumed — both ended when the app closed, and a turn in flight is
    /// stopped first so its ending cannot write into the restored thread.
    fn open_thread(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.stop(cx);
        match threads::load(self.connection.config.id, id) {
            Ok(saved) => {
                self.thread = threads::to_messages(&saved);
                self.tool_errors = threads::tool_errors(&saved);
                self.items = items_from_saved(&saved);
                self.thread_id = Some(id);
                self.tools_supported = true;
                self.stream_supported = true;
            }
            Err(error) => self.notice(&format!("Could not open that thread: {error:#}"), true),
        }
        cx.notify();
    }

    /// Ask before deleting the open thread's file: it cannot be undone.
    fn confirm_delete_current_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread_id.is_none() {
            return;
        }
        let panel = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let panel = panel.clone();
            alert
                .title("Delete this thread?")
                .description("Its saved file is removed from this machine. This cannot be undone.")
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Delete")
                        .cancel_text("Keep")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    panel.update(cx, |panel, cx| panel.delete_current_thread(cx));
                    true
                })
        });
    }

    /// Delete the open thread's file and start over. Other threads are
    /// deleted by opening one first; a thread list is for resuming, not
    /// for bulk management.
    fn delete_current_thread(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.thread_id else {
            return;
        };
        match threads::delete(self.connection.config.id, id) {
            Ok(()) => self.new_thread(cx),
            Err(error) => {
                self.notice(&format!("Thread was not deleted: {error:#}"), true);
                cx.notify();
            }
        }
    }

    fn answer(&mut self, approved: bool, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending.take() {
            pending.answer.send(approved).ok();
            cx.notify();
        }
    }

    /// Set the access level and remember it on the saved connection. The
    /// live modes move with it: lowering it gates a running turn's next
    /// tool call, while raising it waits for the next turn, as do the
    /// tools offered.
    fn set_access(&mut self, access: AgentAccess, cx: &mut Context<Self>) {
        self.access = access;
        self.live_modes.set_access(access);
        self.remember(cx);
        cx.notify();
    }

    fn set_approval(&mut self, approval: WriteApproval, cx: &mut Context<Self>) {
        self.approval = approval;
        self.live_modes.set_approval(approval);
        self.remember(cx);
        cx.notify();
    }

    /// Choose a write-approval mode. Auto-approve on a connection that reads
    /// as production asks first, so the preference itself is a decision.
    fn choose_approval(
        &mut self,
        approval: WriteApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let config = &self.connection.config;
        let risky = approval == WriteApproval::Auto
            && is_risky_agent(&config.name, config.color, self.access, approval);
        if !risky {
            self.set_approval(approval, cx);
            return;
        }
        let panel = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let panel = panel.clone();
            alert
                .title("Auto-approve writes on a production connection?")
                .description(
                    "The assistant's INSERT, UPDATE and DELETE statements will run without \
                     asking, on a connection that looks like production.",
                )
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Auto-approve")
                        .cancel_text("Keep asking")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    panel.update(cx, |panel, cx| panel.set_approval(approval, cx));
                    true
                })
        });
    }

    fn remember(&self, cx: &mut Context<Self>) {
        crate::ui::welcome::remember_agent_settings(
            self.connection.config.id,
            self.access,
            self.approval,
            cx,
        );
    }

    /// Whether the thread is drawn. Turning access off mid-turn does not
    /// hide a turn already running: it keeps showing until it ends (its
    /// next tool call is refused), and `send` and `start_turn` refuse the
    /// next turn while disabled.
    fn shows_thread(&self) -> bool {
        self.access.enabled() || self.running
    }

    /// The saved threads for this connection, newest first, as a menu: a
    /// row reopens its transcript, and the open thread can be deleted. The
    /// list is read when the menu opens, not on every draw.
    fn render_threads_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.entity();
        let connection_id = self.connection.config.id;
        let current = self.thread_id;
        Button::new("agent-threads")
            .ghost()
            .xsmall()
            .label("Threads")
            .accessibility_label("Saved agent threads")
            .tooltip("Open a saved thread")
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let threads = threads::list(connection_id).unwrap_or_default();
                let mut menu = menu;
                if threads.is_empty() {
                    menu = menu.item(option_item(
                        "No saved threads yet",
                        "turn on thread history in Settings › Agents",
                    ));
                }
                for thread in threads {
                    let panel = panel.clone();
                    let id = thread.id;
                    let checked = current == Some(id);
                    menu =
                        menu.item(thread_row(thread.title, checked).on_click(move |_, _, cx| {
                            panel.update(cx, |panel, cx| panel.open_thread(id, cx));
                        }));
                }
                if current.is_some() {
                    let panel = panel.clone();
                    menu = menu.item(
                        option_item("Delete current thread", "removes its file").on_click(
                            move |_, window, cx| {
                                panel.update(cx, |panel, cx| {
                                    panel.confirm_delete_current_thread(window, cx)
                                });
                            },
                        ),
                    );
                }
                menu
            })
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.entity();
        let current_access = self.access;
        let current_approval = self.approval;
        let access_panel = panel.clone();
        let approval_panel = panel.clone();
        let theme = cx.theme();

        let access = Button::new("agent-access")
            .ghost()
            .xsmall()
            .label(self.access.label())
            .accessibility_label(format!("Agent access level: {}", self.access.label()))
            .tooltip("What the assistant may do on this connection")
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                AgentAccess::ALL.into_iter().fold(menu, |menu, access| {
                    let panel = access_panel.clone();
                    menu.item(
                        option_item(access.label(), access.description())
                            .checked(current_access == access)
                            .on_click(move |_, _, cx| {
                                panel.update(cx, |panel, cx| panel.set_access(access, cx));
                            }),
                    )
                })
            });

        let approval = Button::new("agent-approval")
            .ghost()
            .xsmall()
            .label(self.approval.label())
            .accessibility_label(format!("Write approval: {}", self.approval.label()))
            .tooltip("When the assistant's writes need your approval")
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                WriteApproval::ALL.into_iter().fold(menu, |menu, approval| {
                    let panel = approval_panel.clone();
                    menu.item(
                        option_item(approval.label(), approval.description())
                            .checked(current_approval == approval)
                            .on_click(move |_, window, cx| {
                                panel.update(cx, |panel, cx| {
                                    panel.choose_approval(approval, window, cx)
                                });
                            }),
                    )
                })
            });

        v_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.status_bar)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .overflow_hidden()
                            .child("Agent")
                            .child(access)
                            .when(self.access.writes_rows(), |this| this.child(approval)),
                    )
                    .child(self.render_threads_button(cx))
                    .child(
                        Button::new("agent-new-thread")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .accessibility_label("New thread")
                            .tooltip("New thread: start over with an empty conversation")
                            .on_click(cx.listener(|this, _, _, cx| this.new_thread(cx))),
                    ),
            )
    }

    /// One message's words, as Markdown: models answer in it, and a reader can
    /// select and copy from the result.
    fn message_text(&self, ix: usize, text: &str, _cx: &App) -> impl IntoElement {
        div().text_sm().child(TextView::markdown(
            SharedString::from(format!("agent-message-{ix}")),
            text.to_string(),
        ))
    }

    /// A message's sender, with a button that copies its raw Markdown.
    fn message_header(
        &self,
        ix: usize,
        sender: &'static str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let raw = text.to_string();
        h_flex()
            .justify_between()
            .child(div().text_xs().text_color(muted).child(sender))
            .child(
                Button::new(SharedString::from(format!("agent-copy-{ix}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .accessibility_label(format!("Copy {} message", sender.to_lowercase()))
                    .tooltip("Copy the message as Markdown")
                    .on_click(cx.listener(move |_, _, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(raw.clone()));
                        crate::ui::notify_info(window, cx, "Copied");
                    })),
            )
    }

    fn render_item(&self, ix: usize, item: &Item, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        match item {
            Item::User(text) => v_flex()
                .gap_0p5()
                .px_2()
                .py_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .child(self.message_header(ix, "You", text, cx))
                .child(self.message_text(ix, text, cx))
                .into_any_element(),
            Item::Assistant(text) => v_flex()
                .gap_0p5()
                .child(self.message_header(ix, "Assistant", text, cx))
                .child(self.message_text(ix, text, cx))
                .into_any_element(),
            Item::Notice { text, error } => div()
                .text_xs()
                .text_color(if *error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(if *error && !text.starts_with("Error") {
                    format!("Error: {text}")
                } else {
                    text.clone()
                })
                .into_any_element(),
            Item::Tool {
                name,
                arguments,
                result,
                status,
                open,
                ..
            } => {
                let open = *open;
                let status = *status;
                let status_text = status.summary();
                let preview: String = arguments.chars().take(40).collect();
                let summary = format!(
                    "{} {}{}{}",
                    if open { "▾" } else { "▸" },
                    name,
                    if preview.trim() == "{}" || preview.is_empty() {
                        String::new()
                    } else {
                        format!(" {preview}")
                    },
                    status_text,
                );
                let expanded = match result {
                    Some(result) => format!("{arguments}\n→ {result}"),
                    None => format!(
                        "{arguments}\n→ {}",
                        match status {
                            ToolStatus::Running => "running…",
                            ToolStatus::Stopped => "stopped",
                            _ => "waiting…",
                        }
                    ),
                };
                v_flex()
                    .items_start()
                    .child(
                        Button::new(SharedString::from(format!("agent-tool-{ix}")))
                            .custom(crate::ui::subtle_button(cx))
                            .xsmall()
                            .label(summary)
                            .accessibility_label(format!("Tool call {name}{}", status.label()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Item::Tool { open, .. }) = this.items.get_mut(ix) {
                                    *open = !*open;
                                    cx.notify();
                                }
                            })),
                    )
                    .when(open, |this| {
                        this.child(
                            div()
                                .pl_3()
                                .text_size(gpui_kit::px(11.))
                                .text_color(theme.muted_foreground)
                                .font_family("monospace")
                                .child(expanded),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn render_pending(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let pending = self.pending.as_ref()?;
        let theme = cx.theme();
        let kind = match pending.request.kind {
            crate::db::agent::gate::Kind::Dml => "Insert, update, or delete",
            _ => "Schema or permission change",
        };
        Some(
            v_flex()
                .id("agent-approval-card")
                .key_context("AgentApproval")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &ApproveWrite, _, cx| this.answer(true, cx)))
                .on_action(cx.listener(|this, _: &RejectWrite, _, cx| this.answer(false, cx)))
                .gap_1()
                .p_2()
                .border_1()
                .border_color(theme.warning)
                .rounded_md()
                .child(
                    div()
                        .text_sm()
                        .child("Approval needed: the assistant wants to run this"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{kind} · access {} · {}",
                            pending.request.access.label(),
                            pending.request.approval.label()
                        )),
                )
                .child(
                    div()
                        .p_1()
                        .text_xs()
                        .font_family("monospace")
                        .bg(theme.muted)
                        .child(pending.request.sql.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("agent-approve")
                                .primary()
                                .xsmall()
                                .label("Approve")
                                .tooltip_with_action(
                                    "Run this one statement",
                                    &ApproveWrite,
                                    Some("AgentApproval"),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.answer(true, cx))),
                        )
                        .child(
                            Button::new("agent-reject")
                                .outline()
                                .xsmall()
                                .label("Reject")
                                .tooltip_with_action(
                                    "Do not run it",
                                    &RejectWrite,
                                    Some("AgentApproval"),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.answer(false, cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// Draw a saved transcript: user and assistant words, tool calls with
/// their results. Each result goes to the call it answers by id (see
/// [`threads::answers`]); a call that never got one did not run, and is
/// shown stopped rather than left waiting.
fn items_from_saved(saved: &[threads::SavedMessage]) -> Vec<Item> {
    let answered = threads::answers(saved);
    let mut items = Vec::new();
    let mut index = 0;
    for (line_index, line) in saved.iter().enumerate() {
        match line {
            threads::SavedMessage::User { content } => items.push(Item::User(content.clone())),
            threads::SavedMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(text) = content
                    && !text.is_empty()
                {
                    items.push(Item::Assistant(text.clone()));
                }
                for (position, call) in tool_calls.iter().enumerate() {
                    let answer = answered.get(&(line_index, position)).and_then(|tool_line| {
                        match &saved[*tool_line] {
                            threads::SavedMessage::Tool {
                                content, is_error, ..
                            } => Some((content.clone(), *is_error)),
                            _ => None,
                        }
                    });
                    let (result, status) = match answer {
                        Some((content, true)) => (content, ToolStatus::Failed),
                        Some((content, false)) => (content, ToolStatus::Done),
                        None => ("not run".to_string(), ToolStatus::Stopped),
                    };
                    items.push(Item::Tool {
                        index,
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        result: Some(result),
                        status,
                        open: false,
                    });
                    index += 1;
                }
            }
            threads::SavedMessage::Tool { .. } => {}
        }
    }
    items
}

/// A saved thread's row: its title, with a check on the open one.
fn thread_row(title: String, checked: bool) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        h_flex()
            .w_full()
            .gap_4()
            .justify_between()
            .child(div().child(title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if checked { "open" } else { "" }),
            )
    })
    .checked(checked)
}

/// A dropdown choice: its name, then what it means in small muted text,
/// pushed to the right edge.
fn option_item(label: &'static str, description: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        h_flex()
            .w_full()
            .gap_4()
            .justify_between()
            .child(div().child(label))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
    })
}

fn error_text(error: &ProviderError) -> String {
    format!("Error: {error}")
}

impl Focusable for AgentPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.pending.is_some() {
            self.focus.clone()
        } else {
            self.composer.read(cx).focus_handle(cx)
        }
    }
}

impl Render for AgentPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        if !self.shows_thread() {
            return v_flex()
                .size_full()
                .child(self.render_header(cx))
                .child(div().p_3().text_sm().text_color(muted).child(
                    "The Agents panel is turned off for this connection. Raise its \
                             access level above to use it.",
                ))
                .into_any_element();
        }

        let items: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| self.render_item(ix, item, cx))
            .collect();
        let empty = self.items.is_empty();
        let running = self.running;

        v_flex()
            .id("agents-panel")
            .size_full()
            .child(self.render_header(cx))
            .child(
                div()
                    .id("agents-messages")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("agents-messages-area")
                            .size_full()
                            .flex()
                            .flex_col()
                            .track_scroll(&self.messages_scroll)
                            .overflow_y_scroll()
                            .child(
                                v_flex()
                                    .p_2()
                                    .gap_2()
                                    .when(empty, |this| {
                                        this.child(div().text_sm().text_color(muted).child(
                                            "Ask about this database. Nothing is sent to the model \
                                             until you send a message.",
                                        ))
                                    })
                                    .children(items)
                                    .children(self.render_pending(cx))
                                    .when(running && self.pending.is_none(), |this| {
                                        this.child(
                                            div().text_xs().text_color(muted).child("Working…"),
                                        )
                                    }),
                            ),
                    )
                    .scrollbar(&self.messages_scroll, ScrollbarAxis::Vertical),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Textarea::new(&self.composer).small()),
                    )
                    .child(if running {
                        Button::new("agent-stop")
                            .outline()
                            .small()
                            .icon(IconName::Square)
                            .accessibility_label("Stop")
                            .tooltip("Stop the request and any tool call")
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                    } else {
                        Button::new("agent-send")
                            .primary()
                            .small()
                            .icon(IconName::SendHorizontal)
                            .accessibility_label("Send")
                            .tooltip("Send message")
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
                    }),
            )
            .into_any_element()
    }
}
