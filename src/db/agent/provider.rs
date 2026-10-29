//! Talking to an OpenAI-compatible chat-completions endpoint.
//!
//! One code path covers OpenAI, Ollama, LM Studio, OpenRouter and the gateways
//! that copy the shape. [`ChatProvider`] is the seam tests replace with a
//! script, so nothing above this file needs a network.
//!
//! A provider answers in two shapes. [`ChatProvider::complete`] is one whole
//! reply, the shape v1 used. [`ChatProvider::stream`] is the first-class
//! shape: a stream of [`StreamEvent`]s — text as it arrives, tool calls as
//! their arguments arrive — so the panel can show a tool call the moment the
//! model triggers it, long before the turn finishes. A provider that cannot
//! stream (or a test script) gets the streaming shape for free: the default
//! `stream` calls `complete` once and yields its reply as one
//! [`StreamEvent::Complete`]. The turn loop, not the panel, is what turns
//! those events into a reply, so every provider speaks the same language
//! above this file.

use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use serde_json::{Value, json};

/// A tool call the model made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The arguments exactly as the model wrote them: a JSON object, as text.
    pub arguments: String,
}

/// One message of the conversation sent to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    /// A tool's result. It is data for the model to read, never an instruction.
    Tool {
        id: String,
        content: String,
    },
}

impl Message {
    pub fn to_json(&self) -> Value {
        match self {
            Message::System(text) => json!({ "role": "system", "content": text }),
            Message::User(text) => json!({ "role": "user", "content": text }),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut message = json!({ "role": "assistant", "content": content });
                if !tool_calls.is_empty() {
                    message["tool_calls"] = tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": { "name": call.name, "arguments": call.arguments },
                            })
                        })
                        .collect();
                }
                message
            }
            Message::Tool { id, content } => {
                json!({ "role": "tool", "tool_call_id": id, "content": content })
            }
        }
    }
}

/// What the model answered with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// One piece of a streaming answer, in the order the model produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A fragment of the assistant's text. Fragments concatenate, in order,
    /// to the reply's content.
    ContentDelta(String),
    /// A fragment of one tool call. `index` is the call's position in the
    /// reply's tool-call list; `id` and `name` usually arrive with the first
    /// fragment and are `None` after that, while `arguments` is a fragment
    /// of the JSON arguments and concatenates, in order, to the whole.
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    /// A whole reply, from a provider that does not stream. The turn treats
    /// this exactly like the deltas that would have built the same reply.
    Complete(Reply),
    /// The stream ended. `finish_reason` is the server's, if it sent one
    /// (`stop`, `tool_calls`, `length`, …); a stream that simply closes has
    /// none, and that is not an error.
    Finished { finish_reason: Option<String> },
}

/// Why a request failed, in terms the panel can say in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The endpoint refused the `tools` array.
    ToolsUnsupported,
    /// The endpoint refused a streaming request. The turn falls back to
    /// [`ChatProvider::complete`] for that round, so this is a signal to the
    /// turn, not usually a message for the user.
    StreamUnsupported,
    /// The key was missing or refused.
    Auth,
    Unreachable(String),
    Timeout,
    Other(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::ToolsUnsupported => {
                write!(f, "the model endpoint does not support tool calls")
            }
            ProviderError::StreamUnsupported => {
                write!(f, "the model endpoint does not support streaming")
            }
            ProviderError::Auth => {
                write!(
                    f,
                    "the model endpoint refused the API key; check it in Settings"
                )
            }
            ProviderError::Unreachable(why) => {
                write!(f, "the model endpoint could not be reached: {why}")
            }
            ProviderError::Timeout => write!(f, "the model endpoint took too long to answer"),
            ProviderError::Other(why) => write!(f, "the model endpoint failed: {why}"),
        }
    }
}

impl ProviderError {
    /// Whether the endpoint refused the request because the conversation
    /// no longer fits its context window, rather than for any other
    /// reason. Only the endpoint's own wording can say so, so this
    /// matches the phrases those errors use; anything else is not one.
    pub fn is_context_length(&self) -> bool {
        let ProviderError::Other(message) = self else {
            return false;
        };
        let message = message.to_lowercase();
        [
            "context length",
            "context window",
            "maximum context",
            "max context",
            "too many tokens",
            "token limit",
            "context_length",
        ]
        .iter()
        .any(|needle| message.contains(needle))
    }
}

impl std::error::Error for ProviderError {}

/// Anything that can answer a conversation.
pub trait ChatProvider: Send + Sync {
    /// `tools` is the `tools` array of the request, empty to send none.
    fn complete<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [Value],
    ) -> BoxFuture<'a, Result<Reply, ProviderError>>;

    /// The same answer as a stream of [`StreamEvent`]s. The default calls
    /// [`ChatProvider::complete`] once and yields its reply as one
    /// [`StreamEvent::Complete`], so a provider only overrides this when it
    /// can genuinely stream — today, that is [`HttpProvider`] over SSE.
    fn stream<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [Value],
    ) -> BoxStream<'a, Result<StreamEvent, ProviderError>> {
        let future = self.complete(messages, tools);
        Box::pin(stream::once(async move {
            future.await.map(StreamEvent::Complete)
        }))
    }
}

/// Where to send requests: the global settings plus the keychain's key.
#[derive(Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub model: String,
    /// Absent for a local endpoint that wants none. Never logged, never shown.
    pub api_key: Option<String>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish()
    }
}

const TIMEOUT: Duration = Duration::from_secs(120);

/// The real thing, over HTTP.
pub struct HttpProvider {
    endpoint: Endpoint,
    client: reqwest::Client,
}

impl HttpProvider {
    pub fn new(endpoint: Endpoint) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        Ok(Self { endpoint, client })
    }

    fn url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.endpoint.base_url.trim_end_matches('/')
        )
    }

    /// The request body both shapes send, with `stream` added by the
    /// streaming shape only.
    fn body(&self, messages: &[Message], tools: &[Value], stream: bool) -> Value {
        let mut body = json!({
            "model": self.endpoint.model,
            "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
        }
        if stream {
            body["stream"] = Value::Bool(true);
        }
        body
    }

    fn authed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self
            .endpoint
            .api_key
            .as_deref()
            .filter(|key| !key.is_empty())
        {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }

    /// Keep the key out of anything shown, whatever the server echoed.
    fn scrub(&self, text: &str) -> String {
        let mut text = text.to_string();
        if let Some(key) = self
            .endpoint
            .api_key
            .as_deref()
            .filter(|key| !key.is_empty())
        {
            text = text.replace(key, "<key>");
        }
        text.chars().take(300).collect()
    }
}

impl ChatProvider for HttpProvider {
    fn complete<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [Value],
    ) -> BoxFuture<'a, Result<Reply, ProviderError>> {
        Box::pin(async move {
            let body = self.body(messages, tools, false);
            let request = self.authed(self.client.post(self.url()).json(&body));
            let response = request.send().await.map_err(|error| {
                if error.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Unreachable(self.scrub(&error.without_url().to_string()))
                }
            })?;
            let status = response.status();
            let text = response
                .text()
                .await
                .map_err(|error| ProviderError::Other(self.scrub(&error.to_string())))?;
            if !status.is_success() {
                return Err(classify_failure(status.as_u16(), &text, !tools.is_empty())
                    .unwrap_or_else(|| {
                        ProviderError::Other(format!("{status}: {}", self.scrub(&text)))
                    }));
            }
            parse_reply(&text)
        })
    }

    fn stream<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [Value],
    ) -> BoxStream<'a, Result<StreamEvent, ProviderError>> {
        let body = self.body(messages, tools, true);
        let sent_tools = !tools.is_empty();
        let client = self.client.clone();
        let url = self.url();
        let request = self.authed(client.post(url).json(&body));
        // The SSE reader runs on its own task and feeds events down a
        // channel; dropping the stream drops the receiver, which ends the
        // reader on its next send, so a stopped turn does not leave a
        // request running. The task itself is not aborted — it exits as
        // soon as its next send fails — which is why every send is checked.
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let scrub = {
            let endpoint = self.endpoint.clone();
            move |text: &str| {
                let mut text = text.to_string();
                if let Some(key) = endpoint.api_key.as_deref().filter(|key| !key.is_empty()) {
                    text = text.replace(key, "<key>");
                }
                text.chars().take(300).collect()
            }
        };
        tokio::spawn(async move {
            let send = |event: Result<StreamEvent, ProviderError>| tx.unbounded_send(event).is_ok();
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    let error = if error.is_timeout() {
                        ProviderError::Timeout
                    } else {
                        ProviderError::Unreachable(scrub(&error.without_url().to_string()))
                    };
                    send(Err(error));
                    return;
                }
            };
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                let error = classify_failure(status.as_u16(), &text, sent_tools)
                    .or_else(|| classify_stream_failure(status.as_u16(), &text))
                    .unwrap_or_else(|| ProviderError::Other(format!("{status}: {}", scrub(&text))));
                send(Err(error));
                return;
            }
            let mut bytes = response.bytes_stream();
            let mut parser = SseParser::new();
            // Bytes, not text, cross chunk boundaries: a chunk can split a
            // multi-byte UTF-8 character, and decoding each chunk on its
            // own would turn each half into U+FFFD — silently corrupting
            // replies and tool arguments. So incomplete trailing sequences
            // stay in `pending` until the next chunk completes them.
            let mut pending: Vec<u8> = Vec::new();
            use futures::StreamExt as _;
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        send(Err(ProviderError::Other(scrub(&error.to_string()))));
                        return;
                    }
                };
                pending.extend_from_slice(&chunk);
                let text = decode_valid_prefix(&mut pending);
                for event in parser.feed(&text) {
                    let event = event.map_err(|error| match error {
                        ProviderError::Other(message) => ProviderError::Other(scrub(&message)),
                        error => error,
                    });
                    let is_error = event.is_err();
                    if !send(event) {
                        return;
                    }
                    if is_error {
                        return;
                    }
                }
            }
            if !pending.is_empty() {
                // Whatever is left is the end of the stream, so decode it
                // lossily rather than dropping it.
                let text = String::from_utf8_lossy(&pending).into_owned();
                for event in parser.feed(&text) {
                    let event = event.map_err(|error| match error {
                        ProviderError::Other(message) => ProviderError::Other(scrub(&message)),
                        error => error,
                    });
                    let is_error = event.is_err();
                    if !send(event) {
                        return;
                    }
                    if is_error {
                        return;
                    }
                }
            }
            for event in parser.finish() {
                let event = event.map_err(|error| match error {
                    ProviderError::Other(message) => ProviderError::Other(scrub(&message)),
                    error => error,
                });
                let is_error = event.is_err();
                if !send(event) {
                    return;
                }
                if is_error {
                    return;
                }
            }
        });
        Box::pin(rx)
    }
}

/// A streaming refusal that is not a tools refusal: the endpoint took the
/// request but will not stream it. The turn falls back to `complete` for
/// that round rather than failing the turn.
fn classify_stream_failure(status: u16, body: &str) -> Option<ProviderError> {
    if matches!(status, 400 | 404 | 422 | 501) && body.to_lowercase().contains("stream") {
        Some(ProviderError::StreamUnsupported)
    } else {
        None
    }
}

/// One SSE event's worth of a chat-completions stream, as the server sends
/// it: `data: {...}` lines separated by a blank line, with `[DONE]` at the
/// end. Kept separate from the HTTP code so the parsing is unit-tested
/// without a network.
///
/// An event may be split across chunks — a chunk can end mid-line or
/// mid-event — so `feed` keeps the incomplete tail in `buffer` until the
/// next chunk completes it. A server that ignores `stream: true` and answers
/// with one whole JSON reply instead is handled by `finish`, which reads a
/// buffered complete reply as [`StreamEvent::Complete`].
/// The valid UTF-8 prefix of `pending`, as text, with that prefix drained
/// out of `pending`. A trailing incomplete sequence stays for the next
/// chunk; a genuinely invalid byte is decoded lossily (as U+FFFD) and
/// drained, so one bad byte cannot hold the rest of the stream hostage.
fn decode_valid_prefix(pending: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(error) => match error.error_len() {
            // Incomplete: the tail is a character still arriving.
            None => error.valid_up_to(),
            // Invalid: decode up to and including the bad byte, and keep
            // whatever follows for the next chunk.
            Some(len) => error.valid_up_to() + len,
        },
    };
    let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
    pending.drain(..valid);
    text
}

pub struct SseParser {
    buffer: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Events completed by `text`. Incomplete tails stay buffered.
    /// An in-stream error payload is an `Err` item, not a skipped event.
    pub fn feed(&mut self, text: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        self.buffer.push_str(text);
        self.buffer = self.buffer.replace("\r\n", "\n");
        let mut events = Vec::new();
        while let Some(end) = self.buffer.find("\n\n") {
            let raw: String = self.buffer.drain(..end + 2).collect();
            events.extend(parse_sse_event(&raw));
        }
        events
    }

    /// Events in whatever is left when the stream closes: a final event
    /// without its trailing blank line, or a whole JSON reply from a server
    /// that ignored `stream: true`.
    pub fn finish(mut self) -> Vec<Result<StreamEvent, ProviderError>> {
        let raw = std::mem::take(&mut self.buffer);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let events = parse_sse_event(&raw);
        if !events.is_empty() {
            return events;
        }
        match parse_reply(trimmed) {
            Ok(reply) => vec![Ok(StreamEvent::Complete(reply))],
            Err(_) => Vec::new(),
        }
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

/// The events one raw SSE event (data lines, blank line already consumed by
/// the caller) carries, in order: its deltas first, then `Finished` if it
/// carried a finish reason. A server that reports a failure inside a 200
/// stream as `data: {"error": ...}` is an `Err` item: without it the stream
/// would close on an empty reply and the user could not tell the request
/// had failed. The message is unscrubbed here; the reader scrubs it.
fn parse_sse_event(raw: &str) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut events = Vec::new();
    let mut finish_reason = None;
    for line in raw.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.strip_prefix(' ').unwrap_or(data);
        if data == "[DONE]" {
            events.push(Ok(StreamEvent::Finished {
                finish_reason: None,
            }));
            return events;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("the model endpoint reported an error")
                .to_string();
            events.push(Err(ProviderError::Other(message)));
            return events;
        }
        let delta = match value.pointer("/choices/0/delta") {
            Some(delta) => delta,
            None => continue,
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            events.push(Ok(StreamEvent::ContentDelta(text.to_string())));
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                events.push(Ok(StreamEvent::ToolCallDelta {
                    index: call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                    id: call.get("id").and_then(Value::as_str).map(String::from),
                    name: call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(String::from),
                    arguments: call
                        .get("function")
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }));
            }
        }
        if let Some(reason) = value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            finish_reason = Some(reason.to_string());
        }
    }
    if let Some(finish_reason) = finish_reason {
        events.push(Ok(StreamEvent::Finished {
            finish_reason: Some(finish_reason),
        }));
    }
    events
}

/// Read an error response as one of the cases the panel handles specially.
pub fn classify_failure(status: u16, body: &str, sent_tools: bool) -> Option<ProviderError> {
    if status == 401 || status == 403 {
        return Some(ProviderError::Auth);
    }
    let lower = body.to_lowercase();
    if sent_tools
        && matches!(status, 400 | 404 | 422 | 501)
        && (lower.contains("tool") || lower.contains("function"))
    {
        return Some(ProviderError::ToolsUnsupported);
    }
    None
}

/// Read the first choice of a chat-completions response.
pub fn parse_reply(text: &str) -> Result<Reply, ProviderError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| ProviderError::Other("the endpoint did not answer with JSON".into()))?;
    let message = value
        .pointer("/choices/0/message")
        .ok_or_else(|| ProviderError::Other("the answer had no message".into()))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(String::from);
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .filter_map(|(index, call)| {
                    let function = call.get("function")?;
                    Some(ToolCall {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .map_or_else(|| format!("call_{index}"), String::from),
                        name: function.get("name")?.as_str()?.to_string(),
                        arguments: function
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Reply {
        content,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_with_tool_calls_is_read() {
        let reply = parse_reply(
            r#"{"choices":[{"message":{"content":null,"tool_calls":[
                {"id":"a","type":"function","function":{"name":"list_tables","arguments":"{}"}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(reply.content, None);
        assert_eq!(reply.tool_calls[0].name, "list_tables");
        assert_eq!(reply.tool_calls[0].id, "a");
    }

    #[test]
    fn failures_are_told_apart() {
        assert_eq!(classify_failure(401, "", true), Some(ProviderError::Auth));
        assert_eq!(
            classify_failure(400, "this model does not support tools", true),
            Some(ProviderError::ToolsUnsupported)
        );
        // Without tools in the request, "tool" in a 400 proves nothing.
        assert_eq!(classify_failure(400, "tools", false), None);
        assert_eq!(classify_failure(500, "boom", true), None);
        assert!(parse_reply("not json").is_err());
    }

    #[test]
    fn a_context_length_error_is_recognised() {
        assert!(
            ProviderError::Other("400: this model's maximum context length is 8192 tokens".into())
                .is_context_length()
        );
        assert!(
            ProviderError::Other("400: request exceeds the context window".into())
                .is_context_length()
        );
        assert!(!ProviderError::Other("500: boom".into()).is_context_length());
        assert!(!ProviderError::Auth.is_context_length());
    }

    #[test]
    fn the_key_is_never_shown() {
        let provider = HttpProvider::new(Endpoint {
            base_url: "http://localhost:1".into(),
            model: "m".into(),
            api_key: Some("sk-secret".into()),
        })
        .unwrap();
        assert!(!provider.scrub("bad key sk-secret").contains("sk-secret"));
        let debug = format!("{:?}", provider.endpoint);
        assert!(!debug.contains("sk-secret"));
    }

    #[test]
    fn sse_events_are_parsed_in_order() {
        let mut parser = SseParser::new();
        let events: Vec<_> = parser
            .feed(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"list_tables\",\"arguments\":\"{\\\"\"}}]}}]}\n\n",
            )
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::ContentDelta(text) if text == "Hel"));
        match &events[1] {
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                assert_eq!(*index, 0);
                assert_eq!(id.as_deref(), Some("a"));
                assert_eq!(name.as_deref(), Some("list_tables"));
                assert_eq!(arguments, "{\"");
            }
            event => panic!("unexpected event {event:?}"),
        }
    }

    #[test]
    fn sse_events_split_across_chunks_are_held_until_complete() {
        let mut parser = SseParser::new();
        assert!(
            parser
                .feed("data: {\"choices\":[{\"delta\":{\"content\":\"Hel")
                .is_empty()
        );
        let events: Vec<_> = parser
            .feed("lo\"}}]}\n\n")
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::ContentDelta(text) if text == "Hello"));
    }

    #[test]
    fn sse_done_and_a_whole_reply_without_streaming_are_both_read() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: [DONE]\n\n");
        assert_eq!(
            events,
            vec![Ok(StreamEvent::Finished {
                finish_reason: None
            })]
        );

        let mut parser = SseParser::new();
        parser.feed("{\"choices\":[{\"message\":{\"content\":\"hi\",\"tool_calls\":[]}}]}");
        let events: Vec<_> = parser.finish().into_iter().map(Result::unwrap).collect();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], StreamEvent::Complete(reply) if reply.content.as_deref() == Some("hi"))
        );
    }

    #[test]
    fn utf8_split_across_chunks_is_not_corrupted() {
        // "a" plus the first byte of "é" (0xC3 0xA9).
        let mut pending = vec![0x61, 0xC3];
        assert_eq!(decode_valid_prefix(&mut pending), "a");
        assert_eq!(pending, vec![0xC3]);
        pending.push(0xA9);
        assert_eq!(decode_valid_prefix(&mut pending), "é");
        assert!(pending.is_empty());
    }

    #[test]
    fn an_invalid_byte_is_replaced_not_held() {
        // The bad byte is decoded (as U+FFFD) at once; what follows it
        // stays for the next chunk, so it can still join a character
        // arriving later.
        let mut pending = vec![0x61, 0xFF, 0x62];
        assert_eq!(decode_valid_prefix(&mut pending), "a\u{FFFD}");
        assert_eq!(pending, vec![0x62]);
        assert_eq!(decode_valid_prefix(&mut pending), "b");
        assert!(pending.is_empty());
    }

    #[test]
    fn an_in_stream_error_is_reported_not_dropped() {
        let mut parser = SseParser::new();
        let events = parser.feed("data: {\"error\":{\"message\":\"model overloaded\"}}\n\n");
        assert_eq!(
            events,
            vec![Err(ProviderError::Other("model overloaded".into()))]
        );

        let mut parser = SseParser::new();
        let events = parser.feed("data: {\"error\":\"boom\"}\n\n");
        assert_eq!(events, vec![Err(ProviderError::Other("boom".into()))]);
    }

    #[test]
    fn stream_refusals_are_told_apart_from_tool_refusals() {
        assert_eq!(
            classify_stream_failure(400, "streaming is not supported"),
            Some(ProviderError::StreamUnsupported)
        );
        assert_eq!(classify_stream_failure(400, "bad model"), None);
        assert_eq!(classify_stream_failure(500, "streaming broke"), None);
    }

    #[test]
    fn messages_serialize_in_the_chat_shape() {
        let message = Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "x".into(),
                arguments: "{}".into(),
            }],
        };
        assert_eq!(message.to_json()["tool_calls"][0]["function"]["name"], "x");
        assert_eq!(
            Message::Tool {
                id: "1".into(),
                content: "r".into()
            }
            .to_json()["tool_call_id"],
            "1"
        );
    }
}
