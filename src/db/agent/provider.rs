//! Talking to an OpenAI-compatible chat-completions endpoint.
//!
//! One code path covers OpenAI, Ollama, LM Studio, OpenRouter and the gateways
//! that copy the shape. [`ChatProvider`] is the seam tests replace with a
//! script, so nothing above this file needs a network.

use std::time::Duration;

use futures::future::BoxFuture;
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

/// Why a request failed, in terms the panel can say in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The endpoint refused the `tools` array.
    ToolsUnsupported,
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

impl std::error::Error for ProviderError {}

/// Anything that can answer a conversation.
pub trait ChatProvider: Send + Sync {
    /// `tools` is the `tools` array of the request, empty to send none.
    fn complete<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [Value],
    ) -> BoxFuture<'a, Result<Reply, ProviderError>>;
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
            let mut body = json!({
                "model": self.endpoint.model,
                "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
            });
            if !tools.is_empty() {
                body["tools"] = Value::Array(tools.to_vec());
            }
            let mut request = self.client.post(self.url()).json(&body);
            if let Some(key) = self
                .endpoint
                .api_key
                .as_deref()
                .filter(|key| !key.is_empty())
            {
                request = request.bearer_auth(key);
            }
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
