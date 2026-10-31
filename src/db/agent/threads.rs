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

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
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
/// thread's tool results, one flag per [`Message::Tool`] in order, saying
/// which were errors, since the message itself does not carry that.
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

/// Which saved call each tool result answers, as `(assistant line, call
/// position)` -> tool line. A result answers the first still-open call with
/// its id in the most recent assistant message that has one: ids are only
/// unique within a reply (a provider that sends none gets `call_0`,
/// `call_1`, … each round), so an earlier turn's unanswered `call_0` must
/// not claim a later turn's result.
pub fn answers(saved: &[SavedMessage]) -> HashMap<(usize, usize), usize> {
    let mut open: Vec<(usize, usize, &str)> = Vec::new();
    let mut answered = HashMap::new();
    for (line_index, line) in saved.iter().enumerate() {
        match line {
            SavedMessage::Assistant { tool_calls, .. } => {
                for (position, call) in tool_calls.iter().enumerate() {
                    open.push((line_index, position, &call.id));
                }
            }
            SavedMessage::Tool { id, .. } => {
                let Some(latest) = open
                    .iter()
                    .rev()
                    .find(|(_, _, call_id)| call_id == id)
                    .map(|(line, _, _)| *line)
                else {
                    continue;
                };
                if let Some(slot) = open
                    .iter()
                    .position(|(line, _, call_id)| *line == latest && call_id == id)
                {
                    let (line, position, _) = open.remove(slot);
                    answered.insert((line, position), line_index);
                }
            }
            SavedMessage::User { .. } => {}
        }
    }
    answered
}

/// Turn file lines back into model messages. A call with no result — a turn
/// cut off before its tools answered — cannot go back to the model, which
/// requires every call answered, so it is stripped from its assistant
/// message, and a result that answers no call is dropped for the same
/// reason; a message left with neither words nor calls is dropped too.
pub fn to_messages(saved: &[SavedMessage]) -> Vec<Message> {
    let answered = answers(saved);
    let kept_results: std::collections::HashSet<usize> = answered.values().copied().collect();
    let mut messages = Vec::new();
    for (line_index, line) in saved.iter().enumerate() {
        match line {
            SavedMessage::User { content } => messages.push(Message::User(content.clone())),
            SavedMessage::Assistant {
                content,
                tool_calls,
            } => {
                let calls: Vec<ToolCall> = tool_calls
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| answered.contains_key(&(line_index, *position)))
                    .map(|(_, call)| ToolCall {
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
            SavedMessage::Tool { id, content, .. } => {
                if kept_results.contains(&line_index) {
                    messages.push(Message::Tool {
                        id: id.clone(),
                        content: content.clone(),
                    });
                }
            }
        }
    }
    messages
}

/// The error flags for the thread [`to_messages`] rebuilds, one per tool
/// result it keeps, in order — what the panel saves back alongside it.
pub fn tool_errors(saved: &[SavedMessage]) -> Vec<bool> {
    let kept: std::collections::HashSet<usize> = answers(saved).values().copied().collect();
    saved
        .iter()
        .enumerate()
        .filter_map(|(line_index, line)| match line {
            SavedMessage::Tool { is_error, .. } if kept.contains(&line_index) => Some(*is_error),
            _ => None,
        })
        .collect()
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

/// List a connection's threads, newest first. Only as much of each file is
/// read as its title needs — up to its first user message — so opening the
/// menu stays cheap however long the saved transcripts are. A file whose
/// lines up to there do not parse is left out rather than listed under a
/// title it may not have.
pub fn list(connection_id: Uuid) -> Result<Vec<SavedThread>> {
    let mut threads = Vec::new();
    for (id, path, updated_at) in thread_files(&threads_dir(connection_id)?) {
        let Some(title) = read_title(&path) else {
            continue;
        };
        threads.push(SavedThread {
            id,
            title,
            updated_at,
        });
    }
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
    Ok(threads)
}

/// Every `<uuid>.jsonl` in `dir`, with its modification time.
fn thread_files(dir: &Path) -> Vec<(Uuid, PathBuf, SystemTime)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                return None;
            }
            let id = Uuid::parse_str(path.file_stem()?.to_str()?).ok()?;
            let updated_at = entry.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((id, path, updated_at))
        })
        .collect()
}

/// A thread file's title, reading lines only until the first user message.
fn read_title(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut read = Vec::new();
    for line in BufReader::new(file).lines() {
        let line: SavedMessage = serde_json::from_str(&line.ok()?).ok()?;
        let is_user = matches!(line, SavedMessage::User { .. });
        read.push(line);
        if is_user {
            break;
        }
    }
    Some(title_of(&read))
}

/// Write a thread's lines, restricted to the owner like every other file
/// that can hold a pasted secret, then prune the directory back to the cap.
/// Returns whether this was the connection's first saved thread, so the
/// panel can warn once that history is now being kept on this machine.
///
/// `dir` is [`threads_dir`], resolved by the caller before it hands the
/// write to a background thread, and `ticket` is drawn when the snapshot
/// was taken, so an older save that lands late cannot replace a newer one.
pub fn save(
    dir: &Path,
    thread_id: Uuid,
    saved: &[SavedMessage],
    ticket: store::Ticket,
) -> Result<bool> {
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let first = thread_files(dir).is_empty();
    let mut text = String::new();
    for line in saved {
        text.push_str(&serde_json::to_string(line)?);
        text.push('\n');
    }
    // Through a temporary file and a rename, so a crash mid-write never
    // leaves a half-written thread in place of a whole one.
    store::write_atomic(&dir.join(format!("{thread_id}.jsonl")), &text, ticket)?;
    prune(dir, thread_id)?;
    Ok(first)
}

/// Drop every thread past the cap, oldest first, keeping `keep` whatever
/// its age — the thread just saved is the newest by write time anyway, but
/// a clock that jumped backwards must not prune the thread in hand. Reads
/// only file names and times, never a transcript.
fn prune(dir: &Path, keep: Uuid) -> Result<()> {
    let mut files = thread_files(dir);
    if files.len() <= MAX_SAVED_THREADS {
        return Ok(());
    }
    files.sort_by_key(|(_, _, updated_at)| std::cmp::Reverse(*updated_at));
    for (id, path, _) in files.into_iter().skip(MAX_SAVED_THREADS) {
        if id != keep {
            remove(&path)?;
        }
    }
    Ok(())
}

/// Delete one thread file; deleting what is already gone is not an error.
pub fn delete(connection_id: Uuid, thread_id: Uuid) -> Result<()> {
    remove(&thread_file(connection_id, thread_id)?)
}

/// Delete every saved thread for a connection — its [`threads_dir`] —
/// for when the connection itself is deleted: its history may hold pasted
/// secrets, and nothing could list or open it any more. The directory is
/// resolved by the caller so the removal can run on a background thread.
pub fn delete_dir(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not delete {}", dir.display())),
    }
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
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

    fn save_now(connection: Uuid, thread: Uuid, saved: &[SavedMessage]) -> Result<bool> {
        save(
            &threads_dir(connection).expect("the config dir should resolve"),
            thread,
            saved,
            store::ticket(),
        )
    }

    fn call(id: &str) -> SavedToolCall {
        SavedToolCall {
            id: id.into(),
            name: "list_tables".into(),
            arguments: "{}".into(),
        }
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
        save_now(connection, thread, &saved).expect("the thread should save");
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
        save_now(connection, thread, &saved).expect("the thread should save");
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

        save_now(connection, first, &to_saved(&sample(), [false]))
            .expect("the first thread should save");
        save_now(connection, second, &to_saved(&sample(), [false]))
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
            save_now(connection, id, &to_saved(&sample(), [false]))
                .expect("each thread should save");
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

    #[test]
    fn a_repeated_call_id_is_answered_in_its_own_turn() {
        // Providers that send no id get `call_0` every round, so a cut-off
        // call in one turn shares its id with a later, answered one.
        let saved = vec![
            SavedMessage::User {
                content: "first".into(),
            },
            SavedMessage::Assistant {
                content: None,
                tool_calls: vec![call("call_0")],
            },
            SavedMessage::User {
                content: "second".into(),
            },
            SavedMessage::Assistant {
                content: None,
                tool_calls: vec![call("call_0")],
            },
            SavedMessage::Tool {
                id: "call_0".into(),
                content: "items".into(),
                is_error: true,
            },
        ];
        let answered = answers(&saved);
        assert_eq!(
            answered.get(&(3, 0)),
            Some(&4),
            "the later call owns the result"
        );
        assert!(
            !answered.contains_key(&(1, 0)),
            "the cut-off call stays open"
        );
        let messages = to_messages(&saved);
        assert_eq!(
            messages
                .iter()
                .filter(|message| matches!(message, Message::Assistant { .. }))
                .count(),
            1,
            "the cut-off call's empty message is dropped: {messages:?}"
        );
        assert_eq!(tool_errors(&saved), vec![true]);
    }

    #[test]
    fn a_result_that_answers_no_call_is_dropped() {
        let saved = vec![
            SavedMessage::User {
                content: "go".into(),
            },
            SavedMessage::Tool {
                id: "stray".into(),
                content: "orphan".into(),
                is_error: false,
            },
        ];
        assert_eq!(to_messages(&saved), vec![Message::User("go".into())]);
        assert!(tool_errors(&saved).is_empty());
    }

    #[test]
    fn deleting_a_connections_threads_removes_them_all() {
        let _dir = scratch();
        let connection = Uuid::new_v4();
        save_now(connection, Uuid::new_v4(), &to_saved(&sample(), [false]))
            .expect("the thread should save");
        delete_dir(&threads_dir(connection).expect("the config dir should resolve"))
            .expect("the threads should delete");
        assert!(
            list(connection)
                .expect("the threads should list")
                .is_empty()
        );
        delete_dir(&threads_dir(connection).expect("the config dir should resolve"))
            .expect("deleting twice is not an error");
    }

    #[test]
    fn a_title_does_not_need_the_rest_of_the_file() {
        let _dir = scratch();
        let connection = Uuid::new_v4();
        let thread = Uuid::new_v4();
        save_now(connection, thread, &to_saved(&sample(), [false]))
            .expect("the thread should save");
        let path = thread_file(connection, thread).expect("the path should resolve");
        let mut text = fs::read_to_string(&path).expect("the file should read");
        text.push_str("not json\n");
        fs::write(&path, text).expect("the file should write");
        let listed = list(connection).expect("the threads should list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "what tables are here?");
    }
}
