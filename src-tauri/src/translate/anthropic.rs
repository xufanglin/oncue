use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    TranslateContext, TranslateError, TranslateProvider,
    chunker::{build_prompt, parse_numbered_response},
};

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self {
            base_url,
            api_key,
            model,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
}

#[async_trait]
impl TranslateProvider for AnthropicProvider {
    async fn translate(
        &self,
        chunk: &[String],
        context: &TranslateContext,
    ) -> Result<Vec<String>, TranslateError> {
        if chunk.is_empty() {
            return Ok(Vec::new());
        }
        let (system, user) = build_prompt(chunk, context);
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: 4096,
            system: &system,
            messages: vec![Message {
                role: "user",
                content: &user,
            }],
            temperature: 0.0,
        };

        let url = format!("{}/v1/messages", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(TranslateError::Auth(resp.text().await.unwrap_or_default()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(TranslateError::RateLimited);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<ErrorBody>(&body)
                .map(|e| e.error.message)
                .unwrap_or(body);
            return Err(TranslateError::ProviderError(format!("{status}: {msg}")));
        }

        let parsed: MessagesResponse = resp.json().await?;
        let text = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        parse_numbered_response(&text, chunk.len())
            .map_err(|e| TranslateError::Malformed(e.to_string()))
    }

    async fn ping(&self) -> Result<(), TranslateError> {
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: 16,
            system: "",
            messages: vec![Message {
                role: "user",
                content: "ping",
            }],
            temperature: 0.0,
        };
        let url = format!("{}/v1/messages", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(TranslateError::Auth(resp.text().await.unwrap_or_default()));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(TranslateError::ProviderError(format!("{status}: {body}")));
        }
        Ok(())
    }
}
