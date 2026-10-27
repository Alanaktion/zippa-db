//! The Agents panel: a chat beside the session, scoped to its connection.
//!
//! The thread lives here, in memory only — chat history can hold schema, row
//! data and pasted secrets, so it is never written to `workspace.json` or
//! anywhere else. The session owns one panel, so a thread follows its
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
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Window, actions, div,
};

use crate::db::agent::context::{self, ContextInfo};
use crate::db::agent::provider::{Endpoint, HttpProvider, Message, ProviderError};
use crate::db::agent::tools::{Approver, ToolContext, UiAction, WriteRequest};
use crate::db::agent::turn::{self, ToolEvent, TurnOutcome};
use crate::db::{
    AgentAccess, Connection, DatabaseObject, WriteApproval, is_risky_agent, runtime, store,
};
use crate::settings::Settings;
use crate::ui::completion::SharedCatalog;

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
}

/// One line of the thread as the panel draws it.
enum Item {
    User(String),
    Assistant(String),
    Tool {
        event: ToolEvent,
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
    /// What goes back to the model each turn, system prompt excluded.
    thread: Vec<Message>,
    items: Vec<Item>,
    composer: Entity<TextareaState>,
    running: bool,
    /// Bumped by every send and Stop, so a turn that was stopped cannot
    /// write into the thread it was cut from.
    generation: u64,
    abort: Option<tokio::task::AbortHandle>,
    tools_supported: bool,
    pending: Option<Pending>,
    focus: FocusHandle,
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
            connection,
            catalog,
            thread: Vec::new(),
            items: Vec::new(),
            composer,
            running: false,
            generation: 0,
            abort: None,
            tools_supported: true,
            pending: None,
            focus: cx.focus_handle(),
        }
    }

    /// Point the panel at the connection a database switch or reconnect
    /// opened. The thread stays; the next turn re-snapshots the context.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
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
        let mut history = vec![Message::System(prompt)];
        history.extend(self.thread.iter().cloned());

        let tool_context = ToolContext {
            connection: self.connection.clone(),
            catalog,
            access: self.access,
            approval: self.approval,
        };
        let tools_supported = self.tools_supported;
        let (asks, mut requests) = mpsc::unbounded();

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
            turn::run(
                &provider,
                &tool_context,
                &Bridge(asks),
                &history,
                &text,
                tools_supported,
            )
            .await
        });
        self.abort = task.abort_handle();

        // Requests the turn makes for approval arrive here until it ends and
        // its sender is dropped.
        cx.spawn(async move |this, cx| {
            while let Some((request, answer)) = requests.next().await {
                let shown = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.pending = Some(Pending { request, answer });
                    cx.notify();
                    true
                });
                if !matches!(shown, Ok(true)) {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| this.finish(generation, outcome, cx))
                .ok();
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
            self.notice("Stopped.", false);
            cx.notify();
            return;
        };

        self.thread.extend(outcome.messages);
        if outcome.tools_unsupported {
            self.tools_supported = false;
            self.notice(
                "This endpoint does not support tool calls, so the assistant can only answer in \
                 words. SQL it writes can be copied into a query tab.",
                false,
            );
        }
        for event in outcome.events {
            self.items.push(Item::Tool { event, open: false });
        }
        if let Some(answer) = outcome.answer {
            self.items.push(Item::Assistant(answer));
        }
        if outcome.hit_limit {
            self.notice(
                "Stopped calling tools: the limit for one message was reached.",
                false,
            );
        }
        if let Some(error) = outcome.error {
            self.notice(&error_text(&error), true);
        }
        for action in outcome.ui {
            match action {
                UiAction::OpenTable(object) => cx.emit(AgentEvent::OpenTable(object)),
                UiAction::ProposeQuery(sql) => cx.emit(AgentEvent::ProposeQuery(sql)),
            }
        }
        cx.notify();
    }

    fn notice(&mut self, text: &str, error: bool) {
        self.items.push(Item::Notice {
            text: text.to_string(),
            error,
        });
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
        self.notice("Stopped.", false);
        cx.notify();
    }

    /// Forget the conversation. The connection and settings stay.
    pub fn new_thread(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        self.thread.clear();
        self.items.clear();
        self.tools_supported = true;
        cx.notify();
    }

    fn answer(&mut self, approved: bool, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending.take() {
            pending.answer.send(approved).ok();
            cx.notify();
        }
    }

    /// Set the access level and remember it on the saved connection.
    fn set_access(&mut self, access: AgentAccess, cx: &mut Context<Self>) {
        self.access = access;
        self.remember(cx);
        cx.notify();
    }

    fn set_approval(&mut self, approval: WriteApproval, cx: &mut Context<Self>) {
        self.approval = approval;
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
                        PopupMenuItem::new(format!(
                            "{} — {}",
                            access.label(),
                            access.description()
                        ))
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
                        PopupMenuItem::new(format!(
                            "{} — {}",
                            approval.label(),
                            approval.description()
                        ))
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
            Item::Tool { event, open } => {
                let open = *open;
                let preview: String = event.arguments.chars().take(40).collect();
                let summary = format!(
                    "{} {}{}{}",
                    if open { "▾" } else { "▸" },
                    event.name,
                    if preview.trim() == "{}" || preview.is_empty() {
                        String::new()
                    } else {
                        format!(" {preview}")
                    },
                    if event.is_error { " — failed" } else { "" },
                );
                v_flex()
                    .child(
                        Button::new(SharedString::from(format!("agent-tool-{ix}")))
                            .custom(crate::ui::subtle_button(cx))
                            .xsmall()
                            .label(summary)
                            .accessibility_label(format!(
                                "Tool call {}{}",
                                event.name,
                                if event.is_error { ", failed" } else { "" }
                            ))
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
                                .child(format!("{}\n→ {}", event.arguments, event.result)),
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
        if !self.access.enabled() {
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
                v_flex()
                    .id("agents-messages")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .gap_2()
                    .when(empty, |this| {
                        this.child(div().text_sm().text_color(muted).child(
                            "Ask about this database. Nothing is sent to the model until you \
                             send a message.",
                        ))
                    })
                    .children(items)
                    .children(self.render_pending(cx))
                    .when(running && self.pending.is_none(), |this| {
                        this.child(div().text_xs().text_color(muted).child("Working…"))
                    }),
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
