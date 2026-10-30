//! Saved Agents threads: one `.jsonl` file per thread, in a directory per
//! connection, under the config dir.
//!
//! This is opt-in (Settings › Agents): a thread can hold schema, row data,
//! and pasted secrets, so nothing is written unless the user asked for it.
//! A file is the thread's model messages, one JSON object per line, in the
//! order they were sent — system prompts are not saved, since each turn
//! rebuilds its own from the current context. A restored thread comes back
//! as a transcript to continue from: a running turn or a pending approval
//! ended when the app closed, and is never resumed.
//!
//! Retention is a count cap: saving a thread prunes the connection's
//! directory back to [`MAX_SAVED_THREADS`], oldest first, so history cannot
//! grow without bound. Deleting is per thread, from the panel's Threads
//! menu; there is no age expiry, since a thread's age says nothing about
//! whether its owner still wants it.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::provider::{Message, ToolCall};
use crate::db::store;

/// Threads kept per connection; the next save prunes anything older.
pub const MAX_SAVED_THREADS: usize = 50;

/// Characters of the first user message shown as a thread's title.
const TITLE_CHARS: usize = 60;

/// One line of a saved thread file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum SavedMessage {
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<SavedToolCall>,
    },
    Tool {
        id: String,
        content: String,
        /// A tool result that was an error, so the restored transcript
        /// shows it failed rather than finished.
        #[serde(default)]
        is_error: bool,
    },
}

/// A tool call on a saved assistant message: the id is what the matching
/// tool result answers, so it must survive the round trip for the thread
/// to stay continuable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// One saved thread, as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedThread {
    pub id: Uuid,
    pub title: String,
    pub updated_at: SystemTime,
}

/// Where one connection's threads live.
pub fn threads_dir(connection_id: Uuid) -> Result<PathBuf> {
    Ok(store::config_dir()?
        .join("agent-threads")
        .join(connection_id.to_string()))
}

fn thread_file(connection_id: Uuid, thread_id: Uuid) -> Result<PathBuf> {
    Ok(threads_dir(connection_id)?.join(format!("{thread_id}.jsonl")))
}

/// Turn model messages into file lines. `tool_errors` runs alongside the
/// tool results in the thread, in order — a tool item the panel settled as
/// stopped has no message and so consumes no flag — saying which results
/// were errors, since [`Message::Tool`] itself does not carry that.
pub fn to_saved(
    messages: &[Message],
    tool_errors: impl IntoIterator<Item = bool>,
) -> Vec<SavedMessage> {
    let mut errors = tool_errors.into_iter();
    messages
        .iter()
        .filter_map(|message| match message {
            Message::System(_) => None,
            Message::User(text) => Some(SavedMessage::User {
                content: text.clone(),
            }),
            Message::Assistant {
                content,
                tool_calls,
            } => Some(SavedMessage::Assistant {
                content: content.clone(),
                tool_calls: tool_calls
                    .iter()
                    .map(|call| SavedToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect(),
            }),
            Message::Tool { id, content } => Some(SavedMessage::Tool {
                id: id.clone(),
                content: content.clone(),
                is_error: errors.next().unwrap_or(false),
            }),
        })
        .collect()
}

/// Turn file lines back into model messages. A call with no result — a turn
/// cut off before its tools answered — cannot go back to the model, which
/// requires every call answered, so it is stripped from its assistant
/// message; a message left with neither words nor calls is dropped.
pub fn to_messages(saved: &[SavedMessage]) -> Vec<Message> {
    let mut answered: Vec<&str> = Vec::new();
    for line in saved {
        if let SavedMessage::Tool { id, .. } = line {
            answered.push(id);
        }
    }
    let mut messages = Vec::new();
    for line in saved {
        match line {
            SavedMessage::User { content } => messages.push(Message::User(content.clone())),
            SavedMessage::Assistant {
                content,
                tool_calls,
            } => {
                let calls: Vec<ToolCall> = tool_calls
                    .iter()
                    .filter(|call| answered.iter().any(|id| id == &call.id))
                    .map(|call| ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect();
                if content.is_some() || !calls.is_empty() {
                    messages.push(Message::Assistant {
                        content: content.clone(),
                        tool_calls: calls,
                    });
                }
            }
            SavedMessage::Tool { id, content, .. } => messages.push(Message::Tool {
                id: id.clone(),
                content: content.clone(),
            }),
        }
    }
    messages
}

/// A thread's title: its first user message, on one line, truncated.
pub fn title_of(saved: &[SavedMessage]) -> String {
    let Some(first) = saved.iter().find_map(|line| match line {
        SavedMessage::User { content } => Some(content),
        _ => None,
    }) else {
        return "Untitled thread".to_string();
    };
    let single: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = single.chars().take(TITLE_CHARS).collect();
    if single.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    title
}

/// Read one thread file back into lines.
pub fn load(connection_id: Uuid, thread_id: Uuid) -> Result<Vec<SavedMessage>> {
    let path = thread_file(connection_id, thread_id)?;
    let text =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    text.lines()
        .map(|line| {
            serde_json::from_str(line)
                .with_context(|| format!("could not parse a line of {}", path.display()))
        })
        .collect()
}

/// List a connection's threads, newest first. Files that do not parse are
/// left out rather than listed under a title they may not have.
pub fn list(connection_id: Uuid) -> Result<Vec<SavedThread>> {
    let dir = threads_dir(connection_id)?;
    let mut threads = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(threads);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(stem) else {
            continue;
        };
        if path.extension().is_some_and(|ext| ext != "jsonl") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(lines) = text
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<Vec<SavedMessage>, _>>()
        else {
            continue;
        };
        let Ok(updated_at) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        threads.push(SavedThread {
            id,
            title: title_of(&lines),
            updated_at,
        });
    }
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
    Ok(threads)
}

/// Write a thread's lines, restricted to the owner like every other file
/// that can hold a pasted secret, then prune the directory back to the cap.
/// Returns whether this was the connection's first saved thread, so the
/// panel can warn once that history is now being kept on this machine.
pub fn save(connection_id: Uuid, thread_id: Uuid, saved: &[SavedMessage]) -> Result<bool> {
    let dir = threads_dir(connection_id)?;
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let first = fs::read_dir(&dir)
        .map(|entries| entries.flatten().next().is_none())
        .unwrap_or(true);
    let path = thread_file(connection_id, thread_id)?;
    let mut text = String::new();
    for line in saved {
        text.push_str(&serde_json::to_string(line)?);
        text.push('\n');
    }
    // Through a temporary file and a rename, so a crash mid-write never
    // leaves a half-written thread in place of a whole one.
    let temporary = dir.join(format!("{thread_id}.jsonl.tmp"));
    store::write_restricted(&temporary, &text)?;
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("could not save {}", path.display()));
    }
    prune(connection_id, thread_id)?;
    Ok(first)
}

/// Drop every thread past the cap, oldest first, keeping `keep` whatever
/// its age — the thread just saved is the newest by write time anyway, but
/// a clock that jumped backwards must not prune the thread in hand.
fn prune(connection_id: Uuid, keep: Uuid) -> Result<()> {
    // `list` already orders newest first, so skipping the cap drops the
    // oldest.
    let threads = list(connection_id)?;
    if threads.len() <= MAX_SAVED_THREADS {
        return Ok(());
    }
    for thread in threads.into_iter().skip(MAX_SAVED_THREADS) {
        if thread.id != keep {
            delete(connection_id, thread.id)?;
        }
    }
    Ok(())
}

/// Delete one thread file; deleting what is already gone is not an error.
pub fn delete(connection_id: Uuid, thread_id: Uuid) -> Result<()> {
    let path = thread_file(connection_id, thread_id)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not delete {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::store::set_config_dir_for_test;

    struct ScratchDir(PathBuf);

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> ScratchDir {
        let dir = std::env::temp_dir().join(format!(
            "zippa-db-thread-tests-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock should run")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("the scratch dir should exist");
        set_config_dir_for_test(dir.clone());
        ScratchDir(dir)
    }

    fn sample() -> Vec<Message> {
        vec![
            Message::User("what tables are here?".into()),
            Message::Assistant {
                content: Some("Let me look.".into()),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "list_tables".into(),
                    arguments: "{}".into(),
                }],
            },
            Message::Tool {
                id: "call-1".into(),
                content: "items, orders".into(),
            },
            Message::Assistant {
                content: Some("Two tables.".into()),
                tool_calls: vec![],
            },
        ]
    }

    #[test]
    fn a_thread_round_trips_with_tool_results_intact() {
        let _dir = scratch();
        let connection = Uuid::new_v4();
        let thread = Uuid::new_v4();

        let saved = to_saved(&sample(), [false]);
        save(connection, thread, &saved).expect("the thread should save");
        let back = load(connection, thread).expect("the thread should load");
        assert_eq!(back, saved);
        assert_eq!(to_messages(&back), sample());
    }

    #[test]
    fn a_failed_tool_result_stays_failed() {
        let _dir = scratch();
        let connection = Uuid::new_v4();
        let thread = Uuid::new_v4();

        let saved = to_saved(&sample(), [true]);
        save(connection, thread, &saved).expect("the thread should save");
        let back = load(connection, thread).expect("the thread should load");
        assert!(matches!(back[2], SavedMessage::Tool { is_error: true, .. }));
    }

    #[test]
    fn system_prompts_are_not_saved() {
        let mut messages = vec![Message::System("be helpful".into())];
        messages.extend(sample());
        let saved = to_saved(&messages, [false]);
        assert!(
            saved
                .iter()
                .all(|line| !matches!(line, SavedMessage::Assistant {
                content: Some(content),
                ..
            } if content == "be helpful")),
            "the system prompt must not be written to disk: {saved:?}"
        );
        assert_eq!(saved.len(), sample().len());
    }

    #[test]
    fn a_call_with_no_result_is_stripped_on_restore() {
        let saved = vec![
            SavedMessage::User {
                content: "go".into(),
            },
            SavedMessage::Assistant {
                content: None,
                tool_calls: vec![SavedToolCall {
                    id: "call-9".into(),
                    name: "run_sql".into(),
                    arguments: "{}".into(),
                }],
            },
        ];
        assert_eq!(
            to_messages(&saved),
            vec![Message::User("go".into())],
            "a dangling call must not poison the next turn"
        );
    }

    #[test]
    fn titles_come_from_the_first_user_message() {
        let saved = to_saved(&sample(), [false]);
        assert_eq!(title_of(&saved), "what tables are here?");
        let long = SavedMessage::User {
            content: "word ".repeat(20),
        };
        let title = title_of(std::slice::from_ref(&long));
        assert!(title.ends_with('…'), "a long title is truncated: {title}");
        assert_eq!(title.chars().count(), TITLE_CHARS + 1);
    }

    #[test]
    fn threads_list_newest_first_and_delete_removes() {
        let _dir = scratch();
        let connection = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        save(connection, first, &to_saved(&sample(), [false]))
            .expect("the first thread should save");
        save(connection, second, &to_saved(&sample(), [false]))
            .expect("the second thread should save");
        let listed = list(connection).expect("the threads should list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second, "the newest thread lists first");
        assert_eq!(listed[1].id, first);

        delete(connection, second).expect("the thread should delete");
        let listed = list(connection).expect("the threads should list");
        assert_eq!(
            listed.iter().map(|thread| thread.id).collect::<Vec<_>>(),
            vec![first]
        );
        delete(connection, second).expect("deleting twice is not an error");
    }

    #[test]
    fn saving_past_the_cap_prunes_the_oldest() {
        let _dir = scratch();
        let connection = Uuid::new_v4();
        let mut ids = Vec::new();
        for _ in 0..=MAX_SAVED_THREADS {
            let id = Uuid::new_v4();
            save(connection, id, &to_saved(&sample(), [false])).expect("each thread should save");
            ids.push(id);
        }
        let listed = list(connection).expect("the threads should list");
        assert_eq!(listed.len(), MAX_SAVED_THREADS);
        assert!(
            !listed.iter().any(|thread| thread.id == ids[0]),
            "the oldest thread is pruned"
        );
        assert!(
            listed
                .iter()
                .any(|thread| thread.id == ids[MAX_SAVED_THREADS]),
            "the newest thread is kept"
        );
    }
}
