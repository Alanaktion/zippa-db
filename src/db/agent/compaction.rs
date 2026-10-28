//! Context management for long threads: what the model is sent when the
//! conversation has grown past what an endpoint can take.
//!
//! The thread lives in memory only and grows without bound — the system
//! prompt is re-snapshotted every turn and every tool result is kept and
//! re-sent on the next round — so a long thread, especially one with
//! 100-row results, walks into the model's context limit and fails there
//! with a confusing provider error. This module is the graceful path:
//! before the limit is hit, the oldest bulk tool results are replaced by
//! short summaries, and when even that cannot shrink the thread enough,
//! the panel says so plainly instead of sending a request that cannot
//! succeed.
//!
//! What is never touched: user messages, assistant answers, and anything
//! safety-relevant — a write approval or a rejection is a `run_write`
//! result, not bulk data, so it is kept verbatim, as is every tool result
//! that is not a `run_select` / `explain_query` result in the JSON shape
//! those tools produce. And a tool result is only ever replaced, never
//! dropped, so an assistant message's tool calls always keep a result
//! after them: a provider that receives a call without one answers with
//! an error of its own.
//!
//! The panel keeps its own thread whole; only the copy sent to the model
//! is compacted, and the panel says when that happened, so nothing here
//! is silent.

use serde_json::Value;

use super::provider::{Message, ToolCall};

/// Characters of system prompt plus thread the panel will send to the
/// model before compacting anything. Sized for the small end of what
/// endpoints offer — about 25k tokens at four characters a token — since
/// passing the limit fails the whole turn, while compacting early only
/// summarises old tool output, which is the cheap part of the thread.
/// Compaction deliberately does not depend on a provider's `usage`
/// figures: endpoints that omit them are common, and the ones that
/// report them do so after the turn that already failed.
pub const MAX_PROMPT_CHARS: usize = 100_000;

/// Tools whose results are bulk data, summarised oldest-first when the
/// thread is too long.
const COMPACTABLE_TOOLS: [&str; 2] = ["run_select", "explain_query"];

/// What the panel sends instead of the thread as stored.
pub struct Compacted {
    pub messages: Vec<Message>,
    /// Tool results replaced by summaries.
    pub compacted: usize,
}

/// Whether the thread fits, after compacting everything it may.
pub enum Fit {
    Fits(Compacted),
    /// Even with every compactable result summarised, the thread is still
    /// over budget: user messages, assistant answers and safety-relevant
    /// history are not shrunk, so the only honest answer is a new thread.
    TooBig,
}

/// The thread as it should be sent, given the system prompt it goes with.
/// The system prompt itself counts against the budget; the thread returned
/// never includes it.
pub fn compact(system_prompt: &str, thread: &[Message]) -> Fit {
    let mut messages = thread.to_vec();
    let mut total = system_prompt.chars().count() + messages.iter().map(message_len).sum::<usize>();
    if total <= MAX_PROMPT_CHARS {
        return Fit::Fits(Compacted {
            messages,
            compacted: 0,
        });
    }

    // The most recent exchange — from the last user message on — is what
    // the next answer depends on, so it is never compacted. A thread with
    // no user message has no exchange to protect.
    let protected = thread
        .iter()
        .rposition(|message| matches!(message, Message::User(_)))
        .unwrap_or(0);

    let mut compacted = 0;
    for (index, summary) in summaries(&messages, protected) {
        if total <= MAX_PROMPT_CHARS {
            break;
        }
        if let Message::Tool { content, .. } = &mut messages[index] {
            total -= content.chars().count();
            total += summary.chars().count();
            *content = summary;
            compacted += 1;
        }
    }
    if total <= MAX_PROMPT_CHARS {
        Fit::Fits(Compacted {
            messages,
            compacted,
        })
    } else {
        Fit::TooBig
    }
}

/// The summaries for compactable tool results before `protected`, oldest
/// first, as (index of the result, summary) pairs. A result qualifies
/// when its call is a bulk-read tool and its content is the JSON shape
/// those tools produce — columns plus rows — so an error message or any
/// other text is kept verbatim rather than summarised away.
fn summaries(messages: &[Message], protected: usize) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < protected {
        let Message::Assistant { tool_calls, .. } = &messages[index] else {
            index += 1;
            continue;
        };
        let mut next = index + 1;
        for call in tool_calls {
            let Some(Message::Tool { content, .. }) = messages.get(next) else {
                break;
            };
            if COMPACTABLE_TOOLS.contains(&call.name.as_str())
                && let Some(summary) = summarize(content)
            {
                found.push((next, summary));
            }
            next += 1;
        }
        index = next;
    }
    found
}

/// A result's summary: how many rows it held and which columns, or `None`
/// when the content is not the JSON shape the read tools produce.
fn summarize(content: &str) -> Option<String> {
    let value: Value = serde_json::from_str(content).ok()?;
    let columns = value.get("columns")?.as_array()?;
    let rows = value.get("rows")?.as_array()?;
    let names: Vec<&str> = columns.iter().filter_map(Value::as_str).collect();
    let shown: Vec<&str> = names.iter().take(20).copied().collect();
    let mut summary = format!("[earlier result compacted: {} rows", rows.len());
    if !shown.is_empty() {
        summary.push_str(&format!(", columns {}", shown.join(", ")));
        if names.len() > shown.len() {
            summary.push_str(&format!(", and {} more", names.len() - shown.len()));
        }
    }
    summary.push(']');
    Some(summary)
}

fn message_len(message: &Message) -> usize {
    match message {
        Message::System(text) | Message::User(text) => text.chars().count(),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            content.as_deref().map_or(0, |text| text.chars().count())
                + tool_calls
                    .iter()
                    .map(|call: &ToolCall| {
                        call.id.chars().count()
                            + call.name.chars().count()
                            + call.arguments.chars().count()
                    })
                    .sum::<usize>()
        }
        Message::Tool { id, content } => id.chars().count() + content.chars().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::agent::provider::ToolCall;

    fn user(text: &str) -> Message {
        Message::User(text.into())
    }

    fn assistant_text(text: &str) -> Message {
        Message::Assistant {
            content: Some(text.into()),
            tool_calls: Vec::new(),
        }
    }

    fn assistant_calls(calls: &[(&str, &str)]) -> Message {
        Message::Assistant {
            content: None,
            tool_calls: calls
                .iter()
                .map(|(id, name)| ToolCall {
                    id: (*id).into(),
                    name: (*name).into(),
                    arguments: "{}".into(),
                })
                .collect(),
        }
    }

    fn tool(id: &str, content: &str) -> Message {
        Message::Tool {
            id: id.into(),
            content: content.into(),
        }
    }

    /// A `run_select`-shaped result: columns plus rows, with one long cell
    /// per row so the content is large without many rows.
    fn result(columns: &[&str], rows: usize, cell: usize) -> String {
        let columns: Vec<Value> = columns
            .iter()
            .map(|name| Value::String((*name).into()))
            .collect();
        let rows: Vec<Value> = (0..rows)
            .map(|_| Value::Array(vec![Value::String("x".repeat(cell))]))
            .collect();
        Value::Object(
            [
                ("columns".to_string(), Value::Array(columns)),
                ("rows".to_string(), Value::Array(rows)),
            ]
            .into_iter()
            .collect(),
        )
        .to_string()
    }

    fn fits(thread: &[Message]) -> Compacted {
        match compact("", thread) {
            Fit::Fits(compacted) => compacted,
            Fit::TooBig => panic!("expected the thread to fit"),
        }
    }

    #[test]
    fn a_short_thread_is_sent_as_stored() {
        let thread = vec![
            user("how many items?"),
            assistant_calls(&[("1", "run_select")]),
            tool("1", &result(&["n"], 2, 10)),
            assistant_text("There are 2."),
        ];
        let compacted = fits(&thread);
        assert_eq!(compacted.compacted, 0);
        assert_eq!(compacted.messages, thread);
    }

    #[test]
    fn the_oldest_results_compact_and_the_latest_exchange_stays() {
        let old = result(&["id", "name"], 3, 20_000);
        let new = result(&["id"], 3, 20_000);
        let thread = vec![
            user("first question"),
            assistant_calls(&[("1", "run_select")]),
            tool("1", &old),
            assistant_text("first answer"),
            user("second question"),
            assistant_calls(&[("2", "run_select")]),
            tool("2", &new),
        ];
        let compacted = fits(&thread);
        assert_eq!(compacted.compacted, 1);
        // The old result is a summary naming its rows and columns; the
        // call still has its result after it, under the same id.
        assert_eq!(
            compacted.messages[2],
            tool("1", "[earlier result compacted: 3 rows, columns id, name]")
        );
        assert_eq!(compacted.messages[6], tool("2", &new));
        // Everything else is verbatim, user messages included.
        assert_eq!(compacted.messages[0], user("first question"));
        assert_eq!(compacted.messages[3], assistant_text("first answer"));
        assert_eq!(compacted.messages[4], user("second question"));
    }

    #[test]
    fn write_approvals_and_rejections_are_never_compacted() {
        let rejected = "the user rejected this statement and it did not run; do not retry it";
        let approved = "2 rows affected";
        let big = result(&["id"], 3, 20_000);
        let thread = vec![
            user("how many items?"),
            assistant_calls(&[("1", "run_select")]),
            tool("1", &big),
            assistant_text("first answer"),
            user("delete the old rows"),
            assistant_calls(&[("2", "run_write")]),
            tool("2", rejected),
            assistant_text("I did not delete them."),
            user("delete the other rows"),
            assistant_calls(&[("3", "run_write")]),
            tool("3", approved),
            assistant_text("Done."),
            user("and how many items?"),
            assistant_calls(&[("4", "run_select")]),
            tool("4", &big),
        ];
        // The last exchange is protected, so the second big result stays
        // and the first big result is what compacts — while both write
        // results stay verbatim even though the thread is over budget.
        let compacted = fits(&thread);
        assert_eq!(compacted.compacted, 1);
        assert!(
            matches!(&compacted.messages[2], Message::Tool { content, .. } if content.starts_with("[earlier result compacted"))
        );
        assert_eq!(compacted.messages[6], tool("2", rejected));
        assert_eq!(compacted.messages[10], tool("3", approved));
        assert_eq!(compacted.messages[14], tool("4", &big));
    }

    #[test]
    fn a_result_that_is_not_json_is_kept_verbatim() {
        let error = "relation \"items\" does not exist";
        let big = result(&["id"], 3, 60_000);
        let thread = vec![
            user("first question"),
            assistant_calls(&[("1", "run_select")]),
            tool("1", error),
            assistant_calls(&[("2", "run_select")]),
            tool("2", &big),
            user("second question"),
            assistant_text("still thinking"),
        ];
        let compacted = fits(&thread);
        assert_eq!(compacted.compacted, 1);
        assert_eq!(compacted.messages[2], tool("1", error));
        assert!(
            matches!(&compacted.messages[4], Message::Tool { content, .. } if content.starts_with("[earlier result compacted"))
        );
    }

    #[test]
    fn a_summary_names_rows_and_columns() {
        let big = result(&["id", "name"], 3, 40_000);
        let thread = vec![
            user("first question"),
            assistant_calls(&[("1", "run_select")]),
            tool("1", &big),
            user("second question"),
            assistant_text("answer"),
        ];
        let compacted = fits(&thread);
        assert_eq!(compacted.compacted, 1);
        assert_eq!(
            compacted.messages[2],
            tool("1", "[earlier result compacted: 3 rows, columns id, name]")
        );
    }

    #[test]
    fn a_thread_with_nothing_compactable_is_too_big() {
        // Two long user messages and nothing a summary could shrink.
        let thread = vec![user(&"u".repeat(60_000)), user(&"v".repeat(60_000))];
        assert!(matches!(compact("", &thread), Fit::TooBig));
        // A system prompt alone over budget is too big the same way.
        assert!(matches!(
            compact(&"s".repeat(MAX_PROMPT_CHARS + 1), &[]),
            Fit::TooBig
        ));
    }

    #[test]
    fn the_system_prompt_counts_against_the_budget() {
        let big = result(&["id"], 3, 20_000);
        let thread = vec![
            user("first question"),
            assistant_calls(&[("1", "run_select")]),
            tool("1", &big),
            user("second question"),
            assistant_text("answer"),
        ];
        // Without a system prompt this thread is under budget; with a
        // large one it is not, and the old result compacts.
        assert_eq!(fits(&thread).compacted, 0);
        match compact(&"s".repeat(70_000), &thread) {
            Fit::Fits(compacted) => assert_eq!(compacted.compacted, 1),
            Fit::TooBig => panic!("expected the thread to fit after compaction"),
        }
    }
}
