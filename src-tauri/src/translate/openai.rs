use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    TranslateContext, TranslateError, TranslateProvider,
    chunker::{build_prompt, parse_numbered_response},
};

pub struct OpenAiProvider {
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
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
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatRespMessage,
}

#[derive(Deserialize)]
struct ChatRespMessage {
    content: String,
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
impl TranslateProvider for OpenAiProvider {
    async fn translate(
        &self,
        chunk: &[String],
        context: &TranslateContext,
    ) -> Result<Vec<String>, TranslateError> {
        if chunk.is_empty() {
            return Ok(Vec::new());
        }
        let (system, user) = build_prompt(chunk, context);
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: &system,
                },
                ChatMessage {
                    role: "user",
                    content: &user,
                },
            ],
            temperature: 0.0,
        };

        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            let msg = resp.text().await.unwrap_or_default();
            return Err(TranslateError::Auth(msg));
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

        let parsed: ChatResponse = resp.json().await?;
        let content = parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| TranslateError::Malformed("no choices in response".into()))?;

        parse_numbered_response(&content, chunk.len())
            .map_err(|e| TranslateError::Malformed(e.to_string()))
    }

    async fn ping(&self) -> Result<(), TranslateError> {
        let body = ChatRequest {
            model: &self.model,
            messages: vec![ChatMessage {
                role: "user",
                content: "ping",
            }],
            temperature: 0.0,
        };
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
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
