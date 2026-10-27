//! One turn of the conversation: the user's message, then the model and its
//! tools in turn until it answers in words.

use serde_json::Value;

use super::provider::{ChatProvider, Message, ProviderError};
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
    pub hit_limit: bool,
    pub error: Option<ProviderError>,
}

/// Run a turn. `history` is the thread so far, system prompt first.
pub async fn run(
    provider: &dyn ChatProvider,
    context: &ToolContext,
    approver: &dyn Approver,
    history: &[Message],
    user_text: &str,
    tools_supported: bool,
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

    loop {
        let reply = match provider.complete(&thread, &tool_json).await {
            Ok(reply) => reply,
            Err(ProviderError::ToolsUnsupported) if !tool_json.is_empty() => {
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
            Err(error) => {
                outcome.error = Some(error);
                return outcome;
            }
        };

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

        for call in reply.tool_calls {
            let result = if calls_made >= MAX_TOOL_CALLS {
                outcome.hit_limit = true;
                tools::ToolOutput {
                    text: "tool-call limit reached for this turn; answer with what you have".into(),
                    is_error: true,
                    ui: None,
                }
            } else {
                calls_made += 1;
                tools::call(context, approver, &call.name, &call.arguments).await
            };
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
                id: call.id,
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::BoxFuture;

    use super::*;
    use crate::db::agent::provider::{Reply, ToolCall};
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
