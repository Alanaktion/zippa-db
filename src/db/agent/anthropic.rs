//! Talking to Anthropic's Messages API.
//!
//! The rest of the agent speaks the chat-completions shape ([`Message`],
//! OpenAI-style tool definitions, [`StreamEvent`]s); this file translates
//! both ways, so the turn loop does not know which provider it is talking
//! to. The differences that matter:
//!
//! - The system prompt is a top-level field, and turns must alternate user
//!   and assistant, so tool results (a `user` turn of `tool_result` blocks)
//!   and any note the app adds mid-thread are merged into the user turn
//!   around them.
//! - A tool call's arguments are a JSON object (`input`), not text.
//! - The model thinks before it answers, and the blocks it thought in must be
//!   sent back unchanged with the tool calls they led to while the turn is
//!   still going. [`Message`] has nowhere to keep them, so the provider —
//!   which lives for one turn — records each reply that called a tool, whole,
//!   and sends that recording in place of the rebuilt message. Earlier turns
//!   go without their thinking, which the API accepts.
//! - A request that carries no tools cannot carry tool calls either, so when
//!   the turn has stopped offering tools (its tool budget is spent) every
//!   earlier call and result is sent as text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::{Map, Value, json};

use super::provider::{
    ChatProvider, Endpoint, EventParser, Message, ProviderError, Reply, StreamEvent, ToolCall,
    http_client, run_sse, send_for_text,
};

/// The API version every request asks for.
const API_VERSION: &str = "2026-10-21";

/// The most a reply may be, thinking included. Within every current
/// model's limit, and far beyond what one answer in the panel needs.
const MAX_TOKENS: u32 = 16_000;

/// Replies that called a tool, as the API sent them, keyed by the id of the
/// first call.
type Recorded = Arc<Mutex<HashMap<String, Value>>>;

pub struct AnthropicProvider {
    endpoint: Endpoint,
    client: reqwest::Client,
    recorded: Recorded,
}

impl AnthropicProvider {
    pub fn new(endpoint: Endpoint) -> Result<Self, ProviderError> {
        Ok(Self {
            endpoint,
            client: http_client()?,
            recorded: Recorded::default(),
        })
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        let request = self
            .client
            .post(self.endpoint.url("messages"))
            .header("anthropic-version", API_VERSION)
            .json(body);
        match self.endpoint.key() {
            Some(key) => request.header("x-api-key", key),
            None => request,
        }
    }

    fn body(&self, messages: &[Message], tools: &[Value], stream: bool) -> Value {
        let recorded = self
            .recorded
            .lock()
            .map(|recorded| recorded.clone())
            .unwrap_or_default();
        request_body(&self.endpoint.model, messages, tools, stream, &recorded)
    }
}

impl ChatProvider for AnthropicProvider {
    fn complete<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [Value],
    ) -> BoxFuture<'a, Result<Reply, ProviderError>> {
        Box::pin(async move {
            let body = self.body(messages, tools, false);
            let text = send_for_text(self.request(&body), &self.endpoint, classify).await?;
            let value: Value = serde_json::from_str(&text).map_err(|_| {
                ProviderError::Other("the endpoint did not answer with JSON".into())
            })?;
            if value.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
                return Err(refusal());
            }
            let content = value
                .get("content")
                .cloned()
                .ok_or_else(|| ProviderError::Other("the answer had no content".into()))?;
            let reply = reply_from_content(&content);
            record(&self.recorded, &reply, content);
            Ok(reply)
        })
    }

    fn stream<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [Value],
    ) -> BoxStream<'a, Result<StreamEvent, ProviderError>> {
        let body = self.body(messages, tools, true);
        let mut parser = AnthropicSse::new();
        parser.recorded = Some(self.recorded.clone());
        run_sse(self.request(&body), self.endpoint.clone(), parser, classify)
    }
}

fn classify(status: u16, _body: &str) -> Option<ProviderError> {
    // Anthropic always takes tools and streaming, so the only refusal the
    // turn handles specially is the key's.
    matches!(status, 401 | 403).then_some(ProviderError::Auth)
}

fn refusal() -> ProviderError {
    ProviderError::Other("the model declined to answer this request".into())
}

/// Keep a reply that called a tool, so its thinking goes back with it.
fn record(recorded: &Recorded, reply: &Reply, content: Value) {
    if let Some(first) = reply.tool_calls.first()
        && let Ok(mut recorded) = recorded.lock()
    {
        recorded.insert(first.id.clone(), content);
    }
}

/// Read a reply's `content` blocks: its text, joined, and its tool calls.
fn reply_from_content(content: &Value) -> Reply {
    let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
    let text: String = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    let tool_calls = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .enumerate()
        .filter_map(|(index, block)| {
            Some(ToolCall {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("call_{index}"), String::from),
                name: block.get("name")?.as_str()?.to_string(),
                arguments: block
                    .get("input")
                    .map_or_else(|| "{}".to_string(), Value::to_string),
            })
        })
        .collect();
    Reply {
        content: (!text.is_empty()).then_some(text),
        tool_calls,
    }
}

/// An OpenAI-style tool definition as Anthropic takes it.
fn tool_definition(tool: &Value) -> Value {
    let function = tool.get("function").unwrap_or(tool);
    json!({
        "name": function.get("name").cloned().unwrap_or(Value::Null),
        "description": function.get("description").cloned().unwrap_or(Value::Null),
        "input_schema": function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
    })
}

/// The request body for `messages`, in the Messages API's shape.
pub fn request_body(
    model: &str,
    messages: &[Message],
    tools: &[Value],
    stream: bool,
    recorded: &HashMap<String, Value>,
) -> Value {
    let native_tools = !tools.is_empty();
    let mut system: Vec<&str> = Vec::new();
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let text = |text: &str| json!({ "type": "text", "text": text });
    let push = push_turn;

    for message in messages {
        match message {
            // The prompt the thread opens with is the system prompt; a note
            // the app adds later is part of the user's side of the thread.
            Message::System(prompt) if turns.is_empty() => {
                system.push(prompt);
            }
            Message::System(note) => push(&mut turns, "user", vec![text(note)]),
            Message::User(said) if said.is_empty() => {}
            Message::User(said) => push(&mut turns, "user", vec![text(said)]),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let replay = tool_calls
                    .first()
                    .filter(|_| native_tools)
                    .and_then(|first| recorded.get(&first.id))
                    .and_then(Value::as_array);
                if let Some(blocks) = replay {
                    push(&mut turns, "assistant", blocks.clone());
                    continue;
                }
                let mut blocks = Vec::new();
                if let Some(content) = content.as_deref().filter(|text| !text.is_empty()) {
                    blocks.push(text(content));
                }
                for call in tool_calls {
                    if native_tools {
                        let input = serde_json::from_str::<Value>(&call.arguments)
                            .ok()
                            .filter(Value::is_object)
                            .unwrap_or_else(|| Value::Object(Map::new()));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": input,
                        }));
                    } else {
                        blocks.push(text(&format!(
                            "[Called the {} tool with {}]",
                            call.name, call.arguments
                        )));
                    }
                }
                push(&mut turns, "assistant", blocks);
            }
            Message::Tool { id, content } => {
                let block = if native_tools {
                    json!({ "type": "tool_result", "tool_use_id": id, "content": content })
                } else {
                    text(&format!("[Tool result]\n{content}"))
                };
                push(&mut turns, "user", vec![block]);
            }
        }
    }
    // The conversation has to open with the user.
    if turns.first().is_some_and(|(role, _)| *role == "assistant") {
        turns.insert(0, ("user", vec![text("(Earlier conversation follows.)")]));
    }

    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "messages": turns
            .into_iter()
            .map(|(role, content)| json!({ "role": role, "content": content }))
            .collect::<Vec<_>>(),
    });
    if !system.is_empty() {
        body["system"] = Value::String(system.join("\n\n"));
    }
    if native_tools {
        body["tools"] = tools.iter().map(tool_definition).collect();
    }
    if stream {
        body["stream"] = Value::Bool(true);
    }
    body
}

/// Add `blocks` to the thread, joining the last turn when it has the same
/// role: the API wants user and assistant turns to alternate.
fn push_turn(turns: &mut Vec<(&'static str, Vec<Value>)>, role: &'static str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match turns.last_mut() {
        Some((last, existing)) if *last == role => existing.extend(blocks),
        _ => turns.push((role, blocks)),
    }
}

/// A Messages API event stream, read into [`StreamEvent`]s.
///
/// Every event's `data:` line carries its own `type`, so the `event:` lines
/// are not needed. Content blocks are rebuilt as they arrive — text, tool
/// calls, and the thinking that must go back with them — so a reply that
/// called a tool can be recorded whole when the message ends.
pub struct AnthropicSse {
    buffer: String,
    blocks: Vec<Value>,
    /// Each tool-use block's partial JSON input, by block index.
    inputs: HashMap<usize, String>,
    /// Each tool-use block's position among the reply's tool calls.
    tool_positions: HashMap<usize, usize>,
    stop_reason: Option<String>,
    recorded: Option<Recorded>,
}

impl AnthropicSse {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            blocks: Vec::new(),
            inputs: HashMap::new(),
            tool_positions: HashMap::new(),
            stop_reason: None,
            recorded: None,
        }
    }

    fn event(&mut self, value: &Value) -> Vec<Result<StreamEvent, ProviderError>> {
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "content_block_start" => {
                let block = value.get("content_block").cloned().unwrap_or(Value::Null);
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, Value::Null);
                }
                self.blocks[index] = block.clone();
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let position = self.tool_positions.len();
                        self.tool_positions.insert(index, position);
                        self.inputs.insert(index, String::new());
                        vec![Ok(StreamEvent::ToolCallDelta {
                            index: position,
                            id: block.get("id").and_then(Value::as_str).map(String::from),
                            name: block.get("name").and_then(Value::as_str).map(String::from),
                            arguments: String::new(),
                        })]
                    }
                    Some("text") => match block.get("text").and_then(Value::as_str) {
                        Some(text) if !text.is_empty() => {
                            vec![Ok(StreamEvent::ContentDelta(text.to_string()))]
                        }
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                }
            }
            "content_block_delta" => {
                let Some(delta) = value.get("delta") else {
                    return Vec::new();
                };
                let fragment = |field: &str| {
                    delta
                        .get(field)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                };
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let text = fragment("text");
                        self.append(index, "text", &text);
                        if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![Ok(StreamEvent::ContentDelta(text))]
                        }
                    }
                    "input_json_delta" => {
                        let partial = fragment("partial_json");
                        self.inputs.entry(index).or_default().push_str(&partial);
                        match self.tool_positions.get(&index) {
                            Some(&position) if !partial.is_empty() => {
                                vec![Ok(StreamEvent::ToolCallDelta {
                                    index: position,
                                    id: None,
                                    name: None,
                                    arguments: partial,
                                })]
                            }
                            _ => Vec::new(),
                        }
                    }
                    "thinking_delta" => {
                        self.append(index, "thinking", &fragment("thinking"));
                        Vec::new()
                    }
                    "signature_delta" => {
                        if let Some(block) = self.blocks.get_mut(index)
                            && block.is_object()
                        {
                            block["signature"] = Value::String(fragment("signature"));
                        }
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                let Some(input) = self.inputs.get(&index) else {
                    return Vec::new();
                };
                let parsed = if input.trim().is_empty() {
                    Value::Object(Map::new())
                } else {
                    serde_json::from_str(input).unwrap_or_else(|_| Value::Object(Map::new()))
                };
                if let Some(block) = self.blocks.get_mut(index)
                    && block.is_object()
                {
                    block["input"] = parsed;
                }
                // A call with no arguments still needs some, as text.
                match self.tool_positions.get(&index) {
                    Some(&position) if input.trim().is_empty() => {
                        vec![Ok(StreamEvent::ToolCallDelta {
                            index: position,
                            id: None,
                            name: None,
                            arguments: "{}".into(),
                        })]
                    }
                    _ => Vec::new(),
                }
            }
            "message_delta" => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_string());
                    if reason == "refusal" {
                        return vec![Err(refusal())];
                    }
                }
                Vec::new()
            }
            "message_stop" => {
                self.record();
                vec![Ok(StreamEvent::Finished {
                    finish_reason: self.stop_reason.as_deref().map(finish_reason),
                })]
            }
            "error" => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the model endpoint reported an error");
                vec![Err(ProviderError::Other(message.to_string()))]
            }
            _ => Vec::new(),
        }
    }

    fn append(&mut self, index: usize, field: &str, text: &str) {
        if let Some(block) = self.blocks.get_mut(index)
            && block.is_object()
        {
            let mut joined = block
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            joined.push_str(text);
            block[field] = Value::String(joined);
        }
    }

    /// Keep the finished reply if it called a tool.
    fn record(&mut self) {
        let Some(recorded) = &self.recorded else {
            return;
        };
        let content = Value::Array(
            std::mem::take(&mut self.blocks)
                .into_iter()
                .filter(|block| !block.is_null())
                .collect(),
        );
        let reply = reply_from_content(&content);
        record(recorded, &reply, content);
    }
}

impl Default for AnthropicSse {
    fn default() -> Self {
        Self::new()
    }
}

impl EventParser for AnthropicSse {
    fn feed(&mut self, text: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        self.buffer.push_str(text);
        self.buffer = self.buffer.replace("\r\n", "\n");
        let mut events = Vec::new();
        while let Some(end) = self.buffer.find("\n\n") {
            let raw: String = self.buffer.drain(..end + 2).collect();
            for line in raw.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                if let Ok(value) = serde_json::from_str::<Value>(data.trim_start()) {
                    events.extend(self.event(&value));
                }
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<Result<StreamEvent, ProviderError>> {
        let rest = std::mem::take(&mut self.buffer);
        if rest.trim().is_empty() {
            Vec::new()
        } else {
            self.feed(&format!("{rest}\n\n"))
        }
    }
}

/// The chat-completions name for a Messages API stop reason.
fn finish_reason(stop_reason: &str) -> String {
    match stop_reason {
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        "end_turn" | "stop_sequence" => "stop",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "list_tables",
                "description": "List the tables",
                "parameters": { "type": "object", "properties": {} },
            }
        })]
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "list_tables".into(),
            arguments: "{\"schema\":\"public\"}".into(),
        }
    }

    #[test]
    fn the_thread_becomes_alternating_turns_with_a_top_level_system_prompt() {
        let messages = vec![
            Message::System("be careful".into()),
            Message::User("what tables?".into()),
            Message::Assistant {
                content: Some("Looking.".into()),
                tool_calls: vec![call("a"), call("b")],
            },
            Message::Tool {
                id: "a".into(),
                content: "users".into(),
            },
            Message::Tool {
                id: "b".into(),
                content: "orders".into(),
            },
            Message::System("tools are gone".into()),
        ];
        let body = request_body("m", &messages, &tools(), true, &HashMap::new());
        assert_eq!(body["system"], "be careful");
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["name"], "list_tables");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1]["role"], "assistant");
        assert_eq!(turns[1]["content"][1]["type"], "tool_use");
        assert_eq!(turns[1]["content"][1]["input"]["schema"], "public");
        // Both results and the later note share one user turn, results first.
        assert_eq!(turns[2]["role"], "user");
        assert_eq!(turns[2]["content"][0]["type"], "tool_result");
        assert_eq!(turns[2]["content"][1]["tool_use_id"], "b");
        assert_eq!(turns[2]["content"][2]["text"], "tools are gone");
    }

    #[test]
    fn a_recorded_reply_is_sent_back_unchanged_with_its_thinking() {
        let recorded_content = json!([
            { "type": "thinking", "thinking": "", "signature": "sig" },
            { "type": "tool_use", "id": "a", "name": "list_tables", "input": {} },
        ]);
        let mut recorded = HashMap::new();
        recorded.insert("a".to_string(), recorded_content.clone());
        let messages = vec![
            Message::User("go".into()),
            Message::Assistant {
                content: None,
                tool_calls: vec![call("a")],
            },
            Message::Tool {
                id: "a".into(),
                content: "users".into(),
            },
        ];
        let body = request_body("m", &messages, &tools(), false, &recorded);
        assert_eq!(body["messages"][1]["content"], recorded_content);

        // With no tools on offer, calls and results go as text instead.
        let body = request_body("m", &messages, &[], false, &recorded);
        assert!(body.get("tools").is_none());
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns[1]["content"][0]["type"], "text");
        assert!(
            turns[1]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("list_tables")
        );
        assert_eq!(turns[2]["content"][0]["type"], "text");
    }

    #[test]
    fn a_thread_that_opens_with_the_assistant_gets_a_user_turn_first() {
        let messages = vec![Message::Assistant {
            content: Some("hello".into()),
            tool_calls: Vec::new(),
        }];
        let body = request_body("m", &messages, &[], false, &HashMap::new());
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert!(body.get("system").is_none());
    }

    #[test]
    fn a_whole_reply_is_read() {
        let reply = reply_from_content(&json!([
            { "type": "thinking", "thinking": "", "signature": "s" },
            { "type": "text", "text": "Here " },
            { "type": "text", "text": "goes." },
            { "type": "tool_use", "id": "t1", "name": "run_query", "input": { "sql": "select 1" } },
        ]));
        assert_eq!(reply.content.as_deref(), Some("Here goes."));
        assert_eq!(reply.tool_calls[0].id, "t1");
        assert_eq!(reply.tool_calls[0].name, "run_query");
        let arguments: Value = serde_json::from_str(&reply.tool_calls[0].arguments).unwrap();
        assert_eq!(arguments["sql"], "select 1");
    }

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|event| format!("event: x\ndata: {event}\n\n"))
            .collect()
    }

    #[test]
    fn a_stream_is_read_and_a_tool_reply_is_recorded_with_its_thinking() {
        let recorded = Recorded::default();
        let mut parser = AnthropicSse::new();
        parser.recorded = Some(recorded.clone());
        let stream = sse(&[
            json!({ "type": "message_start", "message": {} }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "hm" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Hi" } }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "t1", "name": "list_tables", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"a\":" } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "1}" } }),
            json!({ "type": "content_block_stop", "index": 2 }),
            json!({ "type": "content_block_start", "index": 3, "content_block": { "type": "tool_use", "id": "t2", "name": "list_tables", "input": {} } }),
            json!({ "type": "content_block_stop", "index": 3 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" } }),
            json!({ "type": "message_stop" }),
        ]);
        // Split mid-event, as a network would.
        let (first, second) = stream.split_at(stream.len() / 2);
        let mut events: Vec<StreamEvent> =
            parser.feed(first).into_iter().map(Result::unwrap).collect();
        events.extend(parser.feed(second).into_iter().map(Result::unwrap));
        events.extend(parser.finish().into_iter().map(Result::unwrap));

        assert_eq!(events[0], StreamEvent::ContentDelta("Hi".into()));
        assert_eq!(
            events[1],
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("t1".into()),
                name: Some("list_tables".into()),
                arguments: String::new(),
            }
        );
        let arguments: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCallDelta {
                    index: 0,
                    arguments,
                    ..
                } => Some(arguments.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(arguments, "{\"a\":1}");
        // The second call had no input, so it is given an empty object.
        assert!(events.contains(&StreamEvent::ToolCallDelta {
            index: 1,
            id: None,
            name: None,
            arguments: "{}".into(),
        }));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finished {
                finish_reason: Some("tool_calls".into())
            })
        );

        let recorded = recorded.lock().unwrap();
        let content = &recorded["t1"];
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "hm");
        assert_eq!(content[0]["signature"], "sig");
        assert_eq!(content[1]["text"], "Hi");
        assert_eq!(content[2]["input"]["a"], 1);
        assert_eq!(content[3]["input"], json!({}));
    }

    #[test]
    fn errors_and_refusals_in_a_stream_are_reported() {
        let mut parser = AnthropicSse::new();
        let events = parser.feed(&sse(&[json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" }
        })]));
        assert_eq!(events, vec![Err(ProviderError::Other("Overloaded".into()))]);

        let mut parser = AnthropicSse::new();
        let events = parser.feed(&sse(&[json!({
            "type": "message_delta",
            "delta": { "stop_reason": "refusal" }
        })]));
        assert_eq!(events, vec![Err(refusal())]);

        assert_eq!(classify(401, ""), Some(ProviderError::Auth));
        // Anthropic takes tools, so a 400 that mentions them is not a refusal
        // of tools.
        assert_eq!(classify(400, "tool_use ids must be unique"), None);
    }
}
