//! One turn of the conversation: the user's message, then the model and its
//! tools in turn until it answers in words.
//!
//! A turn is also a stream of [`TurnEvent`]s, in the order things happen:
//! text as it arrives, a tool call the moment the model triggers it, then
//! that call running, then its result. The panel draws from those events, so
//! a tool call is a placeholder with its arguments first and a result
//! second — never a finished list that appears all at once at the end of
//! the turn. [`TurnOutcome`] still records the whole turn, so a caller that
//! does not listen to events (tests, mostly) sees exactly the same result.

use futures::StreamExt;
use futures::channel::mpsc;
use serde_json::Value;

use super::provider::{ChatProvider, Message, ProviderError, StreamEvent, ToolCall};
use super::tools::{self, Approver, ToolContext, UiAction};

/// Tool calls one turn may make before the model is told to answer with what
/// it has.
pub const MAX_TOOL_CALLS: usize = 50;

/// One tool call as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEvent {
    pub name: String,
    pub arguments: String,
    pub result: String,
    pub is_error: bool,
}

/// One thing that happened during a turn, in order. `index` is a tool
/// call's position in the turn as a whole, counting across replies, so a
/// placeholder, its running state, and its result all address the same
/// item even when a later reply triggers its own first tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    /// A fragment of the assistant's text for the current round. Fragments
    /// concatenate to that round's content; a new round starts a new item.
    AssistantDelta(String),
    /// The model triggered a tool call. The panel draws a placeholder for
    /// it now; `arguments` may still be arriving (streaming) or complete
    /// (a non-streaming provider).
    ToolPending {
        index: usize,
        name: String,
        arguments: String,
    },
    /// The tool call's arguments so far, replacing the placeholder's. Only
    /// streaming providers send this; a non-streaming provider's arguments
    /// are already whole in [`TurnEvent::ToolPending`].
    ToolArguments { index: usize, arguments: String },
    /// The call started running. A call beyond the turn's tool budget never
    /// runs, so it goes straight from pending to finished with an error.
    ToolRunning { index: usize },
    /// The call finished. The panel fills the placeholder's result in.
    ToolFinished {
        index: usize,
        result: String,
        is_error: bool,
    },
}

/// What a turn produced.
#[derive(Debug, Default)]
pub struct TurnOutcome {
    /// Messages to add to the thread, in order, starting with the user's.
    pub messages: Vec<Message>,
    pub events: Vec<ToolEvent>,
    pub ui: Vec<UiAction>,
    /// The model's closing words.
    pub answer: Option<String>,
    /// The endpoint turned the `tools` array down, so this turn (and the
    /// following ones) went without.
    pub tools_unsupported: bool,
    /// The endpoint turned streaming down, so this turn fell back to whole
    /// replies. The panel remembers this and stops asking for a stream.
    pub stream_unsupported: bool,
    pub hit_limit: bool,
    pub error: Option<ProviderError>,
}

/// Run a turn. `history` is the thread so far, system prompt first.
///
/// Test entry point: it runs with whole replies and no event listener.
/// The panel uses [`run_with_events`] instead.
#[cfg(test)]
pub async fn run(
    provider: &dyn ChatProvider,
    context: &ToolContext,
    approver: &dyn Approver,
    history: &[Message],
    user_text: &str,
    tools_supported: bool,
) -> TurnOutcome {
    run_with_events(
        provider,
        context,
        approver,
        history,
        user_text,
        tools_supported,
        true,
        None,
    )
    .await
}

/// Run a turn, sending [`TurnEvent`]s to `events` as they happen, if one is
/// given. `stream_supported` is the panel's remembered answer to whether
/// the endpoint streams; a turn that learns otherwise says so in
/// [`TurnOutcome::stream_unsupported`].
#[allow(clippy::too_many_arguments)]
pub async fn run_with_events(
    provider: &dyn ChatProvider,
    context: &ToolContext,
    approver: &dyn Approver,
    history: &[Message],
    user_text: &str,
    tools_supported: bool,
    stream_supported: bool,
    events: Option<mpsc::UnboundedSender<TurnEvent>>,
) -> TurnOutcome {
    let definitions = tools::definitions(context.access);
    let mut tool_json: Vec<Value> = if tools_supported {
        definitions.iter().map(|tool| tool.to_json()).collect()
    } else {
        Vec::new()
    };

    let mut outcome = TurnOutcome::default();
    let mut thread: Vec<Message> = history.to_vec();
    let user = Message::User(user_text.to_string());
    thread.push(user.clone());
    outcome.messages.push(user);
    let mut calls_made = 0;
    let mut stream_supported = stream_supported;
    // Tool indices in events are global to the turn, not per reply: a
    // second reply's first tool call is not the first reply's first tool
    // call, and the panel addresses placeholders by that index.
    let mut tool_base = 0;

    let emit = |event: TurnEvent| {
        if let Some(events) = &events {
            let _ = events.unbounded_send(event);
        }
    };

    loop {
        let base = tool_base;
        let round =
            match collect_reply(provider, &thread, &tool_json, stream_supported, base, &emit).await
            {
                Ok(round) => round,
                Err(RoundError::DegradeTools) => {
                    // Degrade to a conversation: SQL comes back in words and the
                    // user sends it to a tab.
                    outcome.tools_unsupported = true;
                    tool_json.clear();
                    let note = Message::System(
                        "Tools are not available. Answer in words and put any SQL in a code block \
                     for the user to run themselves."
                            .into(),
                    );
                    thread.push(note.clone());
                    outcome.messages.push(note);
                    continue;
                }
                Err(RoundError::StreamUnsupported) => {
                    // Fall back to whole replies for this round and the rest of
                    // the turn; the panel remembers it for later turns.
                    outcome.stream_unsupported = true;
                    stream_supported = false;
                    continue;
                }
                Err(RoundError::Failed(error)) => {
                    outcome.error = Some(error);
                    return outcome;
                }
            };

        let reply = round.reply;
        let assistant = Message::Assistant {
            content: reply.content.clone(),
            tool_calls: reply.tool_calls.clone(),
        };
        thread.push(assistant.clone());
        outcome.messages.push(assistant);

        if reply.tool_calls.is_empty() {
            outcome.answer = reply.content;
            return outcome;
        }
        // Advance by the calls seen this round, not by the calls kept:
        // a malformed call dropped below keeps its placeholder's index,
        // so the next round cannot reuse it.
        tool_base = base + round.positions;

        for (call, index) in reply.tool_calls.iter().zip(round.tool_indices) {
            let result = if calls_made >= MAX_TOOL_CALLS {
                outcome.hit_limit = true;
                tools::ToolOutput {
                    text: "tool-call limit reached for this turn; answer with what you have".into(),
                    is_error: true,
                    ui: None,
                }
            } else {
                calls_made += 1;
                emit(TurnEvent::ToolRunning { index });
                tools::call(context, approver, &call.name, &call.arguments).await
            };
            emit(TurnEvent::ToolFinished {
                index,
                result: result.text.clone(),
                is_error: result.is_error,
            });
            if let Some(action) = result.ui.clone() {
                outcome.ui.push(action);
            }
            outcome.events.push(ToolEvent {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: result.text.clone(),
                is_error: result.is_error,
            });
            let message = Message::Tool {
                id: call.id.clone(),
                content: result.text,
            };
            thread.push(message.clone());
            outcome.messages.push(message);
        }

        if outcome.hit_limit {
            // One more answer, with no tools to reach for.
            tool_json.clear();
        }
    }
}

/// Why a round of the turn did not produce a reply.
enum RoundError {
    /// The endpoint refused tools; the turn degrades to words.
    DegradeTools,
    /// The endpoint refused streaming; the turn retries with whole replies.
    StreamUnsupported,
    /// Anything else; the turn ends with this error.
    Failed(ProviderError),
}

/// One tool call being assembled from a stream's fragments.
#[derive(Default)]
struct AssemblingCall {
    id: String,
    name: String,
    arguments: String,
    pending_emitted: bool,
}

/// One provider round, with the turn-global indices of its tool calls.
/// `positions` is the number of distinct calls the round saw, before any
/// malformed ones were dropped, so the next round's base cannot reuse a
/// dropped placeholder's index.
struct RoundReply {
    reply: super::provider::Reply,
    tool_indices: Vec<usize>,
    positions: usize,
}

/// Collect one provider round into a reply, forwarding its text and tool
/// calls as [`TurnEvent`]s while they arrive.
///
/// With streaming, text deltas and tool-call fragments are forwarded as
/// they come in, and a tool call's placeholder is emitted on its first
/// fragment — before its arguments are whole and before it runs. With
/// whole replies, the reply's text is forwarded once and every tool call
/// gets its placeholder, with whole arguments, as soon as the reply lands.
/// Either way the execution loop below is what runs the calls, in order,
/// so placeholders and results always line up by index.
async fn collect_reply(
    provider: &dyn ChatProvider,
    thread: &[Message],
    tool_json: &[Value],
    stream_supported: bool,
    tool_base: usize,
    emit: &(dyn Fn(TurnEvent) + Send + Sync),
) -> Result<RoundReply, RoundError> {
    if !stream_supported {
        return collect_complete(provider, thread, tool_json, tool_base, emit).await;
    }
    let mut content = String::new();
    let mut calls: Vec<AssemblingCall> = Vec::new();
    // Server index -> dense position in `calls`, assigned on first sight.
    // A server may number its calls sparsely (or start above 0); indexing
    // `calls` by the server index would allocate by that index and, after
    // filtering, shift every later call's placeholder away from its result.
    let mut positions: Vec<usize> = Vec::new();
    let mut saw_event = false;
    let mut stream = provider.stream(thread, tool_json);
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::ContentDelta(text)) => {
                saw_event = true;
                if !text.is_empty() {
                    content.push_str(&text);
                    emit(TurnEvent::AssistantDelta(text));
                }
            }
            Ok(StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            }) => {
                saw_event = true;
                let index = match positions.iter().position(|&seen| seen == index) {
                    Some(position) => position,
                    None => {
                        positions.push(index);
                        calls.push(AssemblingCall::default());
                        calls.len() - 1
                    }
                };
                let call = &mut calls[index];
                if let Some(id) = id {
                    call.id = id;
                }
                if let Some(name) = name {
                    call.name = name;
                }
                call.arguments.push_str(&arguments);
                if !call.pending_emitted {
                    call.pending_emitted = true;
                    emit(TurnEvent::ToolPending {
                        index: tool_base + index,
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                } else if !arguments.is_empty() {
                    emit(TurnEvent::ToolArguments {
                        index: tool_base + index,
                        arguments: call.arguments.clone(),
                    });
                }
            }
            Ok(StreamEvent::Complete(reply)) => {
                // A non-streaming provider's whole reply, via the default
                // stream: treat it exactly like a complete() round.
                if let Some(text) = &reply.content
                    && !text.is_empty()
                {
                    emit(TurnEvent::AssistantDelta(text.clone()));
                }
                for (index, call) in reply.tool_calls.iter().enumerate() {
                    emit(TurnEvent::ToolPending {
                        index: tool_base + index,
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                }
                let positions = reply.tool_calls.len();
                return Ok(RoundReply {
                    tool_indices: (0..positions).map(|index| tool_base + index).collect(),
                    positions,
                    reply,
                });
            }
            Ok(StreamEvent::Finished { .. }) => break,
            Err(ProviderError::StreamUnsupported) if !saw_event => {
                return Err(RoundError::StreamUnsupported);
            }
            Err(ProviderError::ToolsUnsupported) if !tool_json.is_empty() => {
                return Err(RoundError::DegradeTools);
            }
            Err(error) => return Err(RoundError::Failed(error)),
        }
    }
    let positions = calls.len();
    let mut tool_calls = Vec::new();
    let mut tool_indices = Vec::new();
    for (position, call) in calls.into_iter().enumerate() {
        if call.name.is_empty() && call.arguments.is_empty() {
            continue;
        }
        tool_indices.push(tool_base + position);
        tool_calls.push(ToolCall {
            id: if call.id.is_empty() {
                format!("call_{position}")
            } else {
                call.id
            },
            name: call.name,
            arguments: call.arguments,
        });
    }
    Ok(RoundReply {
        reply: super::provider::Reply {
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls,
        },
        tool_indices,
        positions,
    })
}

/// One round via [`ChatProvider::complete`], with the same events a
/// streaming round would have produced by the time its reply landed.
async fn collect_complete(
    provider: &dyn ChatProvider,
    thread: &[Message],
    tool_json: &[Value],
    tool_base: usize,
    emit: &(dyn Fn(TurnEvent) + Send + Sync),
) -> Result<RoundReply, RoundError> {
    match provider.complete(thread, tool_json).await {
        Ok(reply) => {
            if let Some(text) = &reply.content
                && !text.is_empty()
            {
                emit(TurnEvent::AssistantDelta(text.clone()));
            }
            for (index, call) in reply.tool_calls.iter().enumerate() {
                emit(TurnEvent::ToolPending {
                    index: tool_base + index,
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
            }
            let positions = reply.tool_calls.len();
            Ok(RoundReply {
                tool_indices: (0..positions).map(|index| tool_base + index).collect(),
                positions,
                reply,
            })
        }
        Err(ProviderError::ToolsUnsupported) if !tool_json.is_empty() => {
            Err(RoundError::DegradeTools)
        }
        Err(error) => Err(RoundError::Failed(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::BoxFuture;

    use super::*;
    use crate::db::agent::provider::{Reply, StreamEvent, ToolCall};
    use crate::db::catalog::Catalog;
    use crate::db::{AgentAccess, Connection, ConnectionConfig};

    /// Answers from a script and counts how often it was asked.
    struct Script {
        replies: Mutex<Vec<Result<Reply, ProviderError>>>,
        asked: AtomicUsize,
        saw_tools: Mutex<Vec<usize>>,
    }

    impl Script {
        fn new(replies: Vec<Result<Reply, ProviderError>>) -> Self {
            Self {
                replies: Mutex::new(replies),
                asked: AtomicUsize::new(0),
                saw_tools: Mutex::default(),
            }
        }
    }

    impl ChatProvider for Script {
        fn complete<'a>(
            &'a self,
            _: &'a [Message],
            tools: &'a [Value],
        ) -> BoxFuture<'a, Result<Reply, ProviderError>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.saw_tools.lock().unwrap().push(tools.len());
            let next = self.replies.lock().unwrap().remove(0);
            Box::pin(async move { next })
        }
    }

    fn call(name: &str, arguments: &str) -> Reply {
        Reply {
            content: None,
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: name.into(),
                arguments: arguments.into(),
            }],
        }
    }

    fn say(text: &str) -> Reply {
        Reply {
            content: Some(text.into()),
            tool_calls: Vec::new(),
        }
    }

    async fn context(access: AgentAccess) -> ToolContext {
        let database = crate::db::tests::TempDatabase::new().await;
        let connection = Connection::open(
            ConnectionConfig {
                agent_access: access,
                ..database.config()
            },
            None,
        )
        .await
        .unwrap();
        // Keep the file alive for the length of the test.
        std::mem::forget(database);
        ToolContext {
            connection: std::sync::Arc::new(connection),
            catalog: std::sync::Arc::new(Catalog::default()),
            access,
            approval: crate::db::WriteApproval::Ask,
        }
    }

    fn run_blocking<F: std::future::Future>(future: F) -> F::Output {
        crate::db::runtime::block_on(future)
    }

    /// Answers from a script of stream events, one round per call, and
    /// from whole replies when the turn falls back to `complete`.
    struct StreamingScript {
        rounds: Mutex<Vec<Vec<Result<StreamEvent, ProviderError>>>>,
        replies: Mutex<Vec<Result<Reply, ProviderError>>>,
    }

    impl StreamingScript {
        fn new(
            rounds: Vec<Vec<Result<StreamEvent, ProviderError>>>,
            replies: Vec<Result<Reply, ProviderError>>,
        ) -> Self {
            Self {
                rounds: Mutex::new(rounds),
                replies: Mutex::new(replies),
            }
        }
    }

    impl ChatProvider for StreamingScript {
        fn complete<'a>(
            &'a self,
            _: &'a [Message],
            _: &'a [Value],
        ) -> BoxFuture<'a, Result<Reply, ProviderError>> {
            let next = self.replies.lock().unwrap().remove(0);
            Box::pin(async move { next })
        }

        fn stream<'a>(
            &'a self,
            _: &'a [Message],
            _: &'a [Value],
        ) -> futures::stream::BoxStream<'a, Result<StreamEvent, ProviderError>> {
            let round = self.rounds.lock().unwrap().remove(0);
            Box::pin(futures::stream::iter(round))
        }
    }

    fn drain_events(
        mut receiver: futures::channel::mpsc::UnboundedReceiver<TurnEvent>,
    ) -> Vec<TurnEvent> {
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }

    #[test]
    fn streaming_events_arrive_before_the_result() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = StreamingScript::new(
                vec![
                    vec![
                        Ok(StreamEvent::ContentDelta("Checking".into())),
                        Ok(StreamEvent::ToolCallDelta {
                            index: 0,
                            id: Some("1".into()),
                            name: Some("list_tables".into()),
                            arguments: "{}".into(),
                        }),
                        Ok(StreamEvent::Finished {
                            finish_reason: Some("tool_calls".into()),
                        }),
                    ],
                    vec![
                        Ok(StreamEvent::ContentDelta("Done".into())),
                        Ok(StreamEvent::Finished {
                            finish_reason: Some("stop".into()),
                        }),
                    ],
                ],
                vec![],
            );
            let (events_tx, events_rx) = futures::channel::mpsc::unbounded();
            let outcome = run_with_events(
                &provider,
                &context,
                &tools::NeverApprove,
                &[],
                "go",
                true,
                true,
                Some(events_tx),
            )
            .await;
            assert_eq!(outcome.answer.as_deref(), Some("Done"));
            assert_eq!(outcome.events.len(), 1);
            let events = drain_events(events_rx);
            assert_eq!(
                events,
                vec![
                    TurnEvent::AssistantDelta("Checking".into()),
                    TurnEvent::ToolPending {
                        index: 0,
                        name: "list_tables".into(),
                        arguments: "{}".into(),
                    },
                    TurnEvent::ToolRunning { index: 0 },
                    TurnEvent::ToolFinished {
                        index: 0,
                        result: outcome.events[0].result.clone(),
                        is_error: false,
                    },
                    TurnEvent::AssistantDelta("Done".into()),
                ]
            );
        });
    }

    #[test]
    fn tool_indices_count_across_replies_not_per_reply() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let tool = |name: &str| {
                vec![
                    Ok(StreamEvent::ToolCallDelta {
                        index: 0,
                        id: Some("1".into()),
                        name: Some(name.into()),
                        arguments: "{}".into(),
                    }),
                    Ok(StreamEvent::Finished {
                        finish_reason: Some("tool_calls".into()),
                    }),
                ]
            };
            let provider = StreamingScript::new(
                vec![
                    tool("list_tables"),
                    tool("search_schema"),
                    vec![Ok(StreamEvent::ContentDelta("done".into()))],
                ],
                vec![],
            );
            let (events_tx, events_rx) = futures::channel::mpsc::unbounded();
            let outcome = run_with_events(
                &provider,
                &context,
                &tools::NeverApprove,
                &[],
                "go",
                true,
                true,
                Some(events_tx),
            )
            .await;
            assert_eq!(outcome.events.len(), 2);
            let events = drain_events(events_rx);
            let pendings: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    TurnEvent::ToolPending { index, name, .. } => Some((*index, name.clone())),
                    _ => None,
                })
                .collect();
            assert_eq!(
                pendings,
                vec![
                    (0, "list_tables".to_string()),
                    (1, "search_schema".to_string())
                ],
                "the second reply's first call is index 1, not a second index 0"
            );
        });
    }

    #[test]
    fn sparse_server_indices_map_to_dense_positions() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = StreamingScript::new(
                vec![
                    vec![
                        Ok(StreamEvent::ToolCallDelta {
                            index: 5,
                            id: Some("1".into()),
                            name: Some("list_tables".into()),
                            arguments: "{}".into(),
                        }),
                        Ok(StreamEvent::ToolCallDelta {
                            index: 9,
                            id: Some("2".into()),
                            name: Some("list_tables".into()),
                            arguments: "{}".into(),
                        }),
                        Ok(StreamEvent::Finished {
                            finish_reason: Some("tool_calls".into()),
                        }),
                    ],
                    vec![
                        Ok(StreamEvent::ToolCallDelta {
                            index: 0,
                            id: Some("3".into()),
                            name: Some("list_tables".into()),
                            arguments: "{}".into(),
                        }),
                        Ok(StreamEvent::Finished {
                            finish_reason: Some("tool_calls".into()),
                        }),
                    ],
                    vec![Ok(StreamEvent::ContentDelta("done".into()))],
                ],
                vec![],
            );
            let (events_tx, events_rx) = futures::channel::mpsc::unbounded();
            let outcome = run_with_events(
                &provider,
                &context,
                &tools::NeverApprove,
                &[],
                "go",
                true,
                true,
                Some(events_tx),
            )
            .await;
            assert_eq!(outcome.events.len(), 3);
            let events = drain_events(events_rx);
            let pendings: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    TurnEvent::ToolPending { index, .. } => Some(*index),
                    _ => None,
                })
                .collect();
            // Dense positions, not server indices, and the second round
            // starts after the two calls the first round saw — not after
            // server index 9.
            assert_eq!(pendings, vec![0, 1, 2]);
            let finished: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    TurnEvent::ToolFinished { index, .. } => Some(*index),
                    _ => None,
                })
                .collect();
            assert_eq!(finished, vec![0, 1, 2]);
        });
    }

    #[test]
    fn streaming_tool_arguments_accumulate() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = StreamingScript::new(
                vec![
                    vec![
                        Ok(StreamEvent::ToolCallDelta {
                            index: 0,
                            id: Some("1".into()),
                            name: Some("list_tables".into()),
                            arguments: "{".into(),
                        }),
                        Ok(StreamEvent::ToolCallDelta {
                            index: 0,
                            id: None,
                            name: None,
                            arguments: "}".into(),
                        }),
                        Ok(StreamEvent::Finished {
                            finish_reason: Some("tool_calls".into()),
                        }),
                    ],
                    vec![Ok(StreamEvent::ContentDelta("done".into()))],
                ],
                vec![],
            );
            let (events_tx, events_rx) = futures::channel::mpsc::unbounded();
            let outcome = run_with_events(
                &provider,
                &context,
                &tools::NeverApprove,
                &[],
                "go",
                true,
                true,
                Some(events_tx),
            )
            .await;
            assert_eq!(outcome.events[0].arguments, "{}");
            let events = drain_events(events_rx);
            assert!(events.contains(&TurnEvent::ToolArguments {
                index: 0,
                arguments: "{}".into(),
            }));
        });
    }

    #[test]
    fn a_stream_that_is_refused_falls_back_to_whole_replies() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = StreamingScript::new(
                vec![vec![Err(ProviderError::StreamUnsupported)]],
                vec![Ok(call("list_tables", "{}")), Ok(say("done"))],
            );
            let outcome = run_with_events(
                &provider,
                &context,
                &tools::NeverApprove,
                &[],
                "go",
                true,
                true,
                None,
            )
            .await;
            assert!(outcome.stream_unsupported);
            assert_eq!(outcome.answer.as_deref(), Some("done"));
            assert_eq!(outcome.events.len(), 1);
        });
    }

    #[test]
    fn tools_then_an_answer() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = Script::new(vec![
                Ok(call("list_tables", "{}")),
                Ok(say("There are none.")),
            ]);
            let outcome = run(
                &provider,
                &context,
                &tools::NeverApprove,
                &[],
                "what tables?",
                true,
            )
            .await;
            assert_eq!(outcome.answer.as_deref(), Some("There are none."));
            assert_eq!(outcome.events.len(), 1);
            assert_eq!(provider.asked.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn a_tool_above_the_access_level_is_refused_in_code() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = Script::new(vec![
                Ok(call("run_select", r#"{"sql":"select 1"}"#)),
                Ok(say("ok")),
            ]);
            let outcome = run(
                &provider,
                &context,
                &tools::NeverApprove,
                &[],
                "rows?",
                true,
            )
            .await;
            assert!(outcome.events[0].is_error);
            assert!(outcome.events[0].result.contains("not available"));
        });
    }

    #[test]
    fn the_tool_call_cap_ends_the_turn() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let mut replies: Vec<_> = (0..MAX_TOOL_CALLS + 2)
                .map(|_| Ok(call("list_tables", "{}")))
                .collect();
            replies.push(Ok(say("done")));
            let provider = Script::new(replies);
            let outcome = run(&provider, &context, &tools::NeverApprove, &[], "go", true).await;
            assert!(outcome.hit_limit);
            assert_eq!(outcome.answer.as_deref(), Some("done"));
            let real = outcome.events.iter().filter(|e| !e.is_error).count();
            assert_eq!(real, MAX_TOOL_CALLS);
        });
    }

    #[test]
    fn an_endpoint_without_tools_degrades_to_words() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = Script::new(vec![
                Err(ProviderError::ToolsUnsupported),
                Ok(say("select 1")),
            ]);
            let outcome = run(&provider, &context, &tools::NeverApprove, &[], "hi", true).await;
            assert!(outcome.tools_unsupported);
            assert_eq!(outcome.answer.as_deref(), Some("select 1"));
            assert_eq!(*provider.saw_tools.lock().unwrap().last().unwrap(), 0);
        });
    }

    #[test]
    fn a_failure_is_reported_not_retried() {
        run_blocking(async {
            let context = context(AgentAccess::Schema).await;
            let provider = Script::new(vec![Err(ProviderError::Auth)]);
            let outcome = run(&provider, &context, &tools::NeverApprove, &[], "hi", true).await;
            assert_eq!(outcome.error, Some(ProviderError::Auth));
            assert_eq!(provider.asked.load(Ordering::SeqCst), 1);
        });
    }
}
