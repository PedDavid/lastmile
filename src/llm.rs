//! A client for OpenAI-compatible APIs: OpenAI itself, or a self-hosted
//! server that speaks the same protocol (Ollama, vLLM, llama.cpp, LM
//! Studio). It only ever drafts rules: the model never reads a message on
//! the recurring path, and it only sees text an admin has read and sent.

use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{LlmApi, LlmConfig, LlmStructured};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Message {
        Message {
            role,
            content: content.into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("could not reach the model: {0}")]
    Request(String),
    #[error("the model's server answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unexpected reply from the model: {0}")]
    Reply(String),
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    model: String,
    api: LlmApi,
    structured: LlmStructured,
    key: Option<String>,
}

impl Client {
    /// `None` when no model is configured.
    pub fn from_config(cfg: &LlmConfig) -> anyhow::Result<Option<Client>> {
        let Some(model) = &cfg.llm_model else {
            return Ok(None);
        };
        let key = match &cfg.llm_api_key_file {
            Some(path) => Some(
                std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?
                    .trim()
                    .to_string(),
            )
            .filter(|k| !k.is_empty()),
            None => None,
        };
        let http = reqwest::Client::builder()
            // Following redirects could send the key somewhere else.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(cfg.llm_timeout.as_secs().max(1) as u64))
            .build()?;
        Ok(Some(Client {
            http,
            base: cfg.llm_base_url.as_str().trim_end_matches('/').to_string(),
            model: model.clone(),
            api: cfg.llm_api,
            structured: cfg.llm_structured,
            key,
        }))
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Where requests go, without the path: shown to the admin before
    /// anything is sent.
    pub fn host(&self) -> String {
        url::Url::parse(&self.base)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_else(|_| self.base.clone())
    }

    /// Whether the reply is constrained by the request (strict schema or a
    /// JSON object) rather than only by the prompt.
    pub fn schema_in_prompt(&self) -> bool {
        self.structured != LlmStructured::Schema
    }

    /// One request: `messages` in, the model's text out. `schema` is the
    /// JSON Schema of the reply, used when the server supports it.
    pub async fn complete(
        &self,
        messages: &[Message],
        schema_name: &str,
        schema: &Value,
    ) -> Result<Completion, LlmError> {
        let messages: Vec<Value> = messages
            .iter()
            .map(|m| json!({"role": m.role.as_str(), "content": m.content}))
            .collect();
        let (path, body) = match self.api {
            LlmApi::Chat => {
                let mut body = json!({"model": self.model, "messages": messages});
                match self.structured {
                    LlmStructured::Schema => {
                        body["response_format"] = json!({
                            "type": "json_schema",
                            "json_schema": {"name": schema_name, "strict": true, "schema": schema},
                        });
                    }
                    LlmStructured::Json => {
                        body["response_format"] = json!({"type": "json_object"});
                    }
                    LlmStructured::None => {}
                }
                ("/chat/completions", body)
            }
            LlmApi::Responses => {
                let mut body = json!({"model": self.model, "input": messages});
                match self.structured {
                    LlmStructured::Schema => {
                        body["text"] = json!({"format": {
                            "type": "json_schema",
                            "name": schema_name,
                            "strict": true,
                            "schema": schema,
                        }});
                    }
                    LlmStructured::Json => {
                        body["text"] = json!({"format": {"type": "json_object"}});
                    }
                    LlmStructured::None => {}
                }
                ("/responses", body)
            }
        };
        let mut request = self.http.post(format!("{}{path}", self.base)).json(&body);
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| LlmError::Request(e.without_url().to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| LlmError::Request(e.without_url().to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: api_error(&text),
            });
        }
        let reply: Value = serde_json::from_str(&text)
            .map_err(|_| LlmError::Reply("the reply is not JSON".into()))?;
        parse_reply(self.api, &reply)
    }
}

/// The error message out of an API error body, else its start.
fn api_error(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string));
    let message = message.unwrap_or_else(|| body.to_string());
    message.chars().take(300).collect()
}

fn parse_reply(api: LlmApi, reply: &Value) -> Result<Completion, LlmError> {
    let (text, usage) = match api {
        LlmApi::Chat => {
            let message = &reply["choices"][0]["message"];
            if let Some(refusal) = message["refusal"].as_str().filter(|r| !r.is_empty()) {
                return Err(LlmError::Reply(format!("the model refused: {refusal}")));
            }
            let text = message["content"]
                .as_str()
                .ok_or_else(|| LlmError::Reply("no message content in the first choice".into()))?;
            (
                text.to_string(),
                (
                    reply["usage"]["prompt_tokens"].as_u64(),
                    reply["usage"]["completion_tokens"].as_u64(),
                ),
            )
        }
        LlmApi::Responses => {
            let mut text = String::new();
            for item in reply["output"].as_array().into_iter().flatten() {
                if item["type"] != "message" {
                    continue;
                }
                for part in item["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("output_text") => text.push_str(part["text"].as_str().unwrap_or("")),
                        Some("refusal") => {
                            return Err(LlmError::Reply(format!(
                                "the model refused: {}",
                                part["refusal"].as_str().unwrap_or("")
                            )));
                        }
                        _ => {}
                    }
                }
            }
            if text.is_empty() {
                return Err(LlmError::Reply("no output text in the reply".into()));
            }
            (
                text,
                (
                    reply["usage"]["input_tokens"].as_u64(),
                    reply["usage"]["output_tokens"].as_u64(),
                ),
            )
        }
    };
    Ok(Completion {
        text,
        input_tokens: usage.0.unwrap_or(0),
        output_tokens: usage.1.unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer, extra: &[&str]) -> Client {
        let uri = format!("{}/v1", server.uri());
        let mut args = vec!["lastmile", "--llm-model", "m", "--llm-base-url", &uri];
        args.extend_from_slice(extra);
        let cfg = crate::config::Config::try_parse_from(args).unwrap();
        Client::from_config(&cfg.llm).unwrap().unwrap()
    }

    fn msgs() -> Vec<Message> {
        vec![
            Message::new(Role::System, "be brief"),
            Message::new(Role::User, "hi"),
        ]
    }

    #[tokio::test]
    async fn chat_completions_with_a_strict_schema() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "model": "m",
                "messages": [{"role": "system", "content": "be brief"}, {"role": "user", "content": "hi"}],
                "response_format": {"type": "json_schema", "json_schema": {"name": "s", "strict": true}},
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "{\"a\":1}"}}],
                "usage": {"prompt_tokens": 12, "completion_tokens": 5},
            })))
            .mount(&server)
            .await;
        let c = client(&server, &[]);
        let out = c
            .complete(&msgs(), "s", &json!({"type": "object"}))
            .await
            .unwrap();
        assert_eq!(
            out,
            Completion {
                text: "{\"a\":1}".into(),
                input_tokens: 12,
                output_tokens: 5
            }
        );
        assert!(!c.schema_in_prompt());
    }

    #[tokio::test]
    async fn the_key_is_sent_as_a_bearer_token_and_only_if_there_is_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer sk-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "ok"}}]
            })))
            .mount(&server)
            .await;
        let dir = std::env::temp_dir().join(format!("lastmile-key-{}", std::process::id()));
        std::fs::write(&dir, "sk-secret\n").unwrap();
        let c = client(&server, &["--llm-api-key-file", dir.to_str().unwrap()]);
        c.complete(&msgs(), "s", &json!({})).await.unwrap();
        std::fs::remove_file(&dir).unwrap();
        // Without a key (self-hosted), no Authorization header: the mock
        // above would not match, and answers 404.
        let c = client(&server, &[]);
        let err = c.complete(&msgs(), "s", &json!({})).await.unwrap_err();
        assert!(matches!(err, LlmError::Status { status: 404, .. }), "{err}");
    }

    #[tokio::test]
    async fn json_and_none_modes_for_servers_without_schemas() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(
                json!({"response_format": {"type": "json_object"}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{}"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(&server, &["--llm-structured", "json"]);
        assert!(c.schema_in_prompt());
        c.complete(&msgs(), "s", &json!({})).await.unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{}"}}]
            })))
            .mount(&server)
            .await;
        let c = client(&server, &["--llm-structured", "none"]);
        c.complete(&msgs(), "s", &json!({})).await.unwrap();
        let sent: Value = server.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        assert!(sent.get("response_format").is_none(), "{sent}");
    }

    #[tokio::test]
    async fn the_responses_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_partial_json(json!({
                "model": "m",
                "input": [{"role": "user", "content": "hi"}],
                "text": {"format": {"type": "json_schema", "name": "s", "strict": true}},
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output": [
                    {"type": "reasoning", "summary": []},
                    {"type": "message", "content": [{"type": "output_text", "text": "{\"b\":"}, {"type": "output_text", "text": "2}"}]},
                ],
                "usage": {"input_tokens": 7, "output_tokens": 3},
            })))
            .mount(&server)
            .await;
        let c = client(&server, &["--llm-api", "responses"]);
        let out = c
            .complete(&[Message::new(Role::User, "hi")], "s", &json!({}))
            .await
            .unwrap();
        assert_eq!(
            (out.text.as_str(), out.input_tokens, out.output_tokens),
            ("{\"b\":2}", 7, 3)
        );
    }

    #[tokio::test]
    async fn failures_say_what_went_wrong() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "Incorrect API key provided"}
            })))
            .mount(&server)
            .await;
        let c = client(&server, &[]);
        let err = c.complete(&msgs(), "s", &json!({})).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "the model's server answered 401: Incorrect API key provided"
        );

        let refused = json!({"choices": [{"message": {"content": null, "refusal": "no"}}]});
        assert!(parse_reply(LlmApi::Chat, &refused).is_err());
        assert!(parse_reply(LlmApi::Chat, &json!({"choices": []})).is_err());
        assert!(parse_reply(LlmApi::Responses, &json!({"output": []})).is_err());
    }

    #[test]
    fn no_model_means_no_client() {
        let cfg = crate::config::Config::try_parse_from(["lastmile"]).unwrap();
        assert!(Client::from_config(&cfg.llm).unwrap().is_none());
    }

    #[tokio::test]
    async fn the_host_is_shown_without_the_path() {
        let server = MockServer::start().await;
        let c = client(&server, &[]);
        assert_eq!(c.host(), server.uri());
        assert_eq!(c.model(), "m");
    }
}
