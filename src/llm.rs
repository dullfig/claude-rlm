use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// LLM provider configuration, loaded from environment variables.
///
/// Environment variables:
///   CONTEXTMEM_LLM_PROVIDER  - "anthropic" or "openai" (default: "anthropic")
///   CONTEXTMEM_LLM_API_KEY   - API key (required for cloud providers, optional for local)
///   CONTEXTMEM_LLM_MODEL     - Model name (default: provider-specific)
///   CONTEXTMEM_LLM_BASE_URL  - Base URL override (for Ollama, OpenRouter, etc.)
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub provider: Provider,
    pub api_key: Option<String>,
    pub model: String,
    pub base_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Provider {
    Anthropic,
    OpenAICompat,
}

impl LlmConfig {
    /// Load configuration from environment variables.
    /// Returns None if no LLM is configured (no API key and no local endpoint).
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("CONTEXTMEM_LLM_API_KEY").ok();
        let provider_str = std::env::var("CONTEXTMEM_LLM_PROVIDER")
            .unwrap_or_else(|_| "anthropic".to_string());

        let provider = match provider_str.to_lowercase().as_str() {
            "openai" | "ollama" | "openrouter" => Provider::OpenAICompat,
            _ => Provider::Anthropic,
        };

        let (default_model, default_url) = match provider {
            Provider::Anthropic => (
                "claude-haiku-4-5-20251001".to_string(),
                "https://api.anthropic.com".to_string(),
            ),
            Provider::OpenAICompat => (
                "llama3".to_string(),
                "http://localhost:11434".to_string(),
            ),
        };

        let model = std::env::var("CONTEXTMEM_LLM_MODEL").unwrap_or(default_model);
        let base_url = std::env::var("CONTEXTMEM_LLM_BASE_URL").unwrap_or(default_url);

        // For Anthropic, require an API key
        // For OpenAI-compat (Ollama), API key is optional (local)
        if provider == Provider::Anthropic && api_key.is_none() {
            return None;
        }

        Some(Self {
            provider,
            api_key,
            model,
            base_url,
        })
    }

    /// Send a prompt to the configured LLM and return the response text.
    pub fn complete(&self, system: &str, user_message: &str) -> Result<String> {
        match self.provider {
            Provider::Anthropic => self.complete_anthropic(system, user_message),
            Provider::OpenAICompat => self.complete_openai(system, user_message),
        }
    }

    /// Call the Anthropic Messages API.
    fn complete_anthropic(&self, system: &str, user_message: &str) -> Result<String> {
        let api_key = self.api_key.as_deref()
            .ok_or_else(|| anyhow!("CONTEXTMEM_LLM_API_KEY required for Anthropic provider"))?;

        let client = reqwest::blocking::Client::new();
        let url = format!("{}/v1/messages", self.base_url);

        let body = AnthropicRequest {
            model: &self.model,
            max_tokens: 2048,
            system,
            messages: vec![AnthropicMessage {
                role: "user",
                content: user_message,
            }],
        };

        let resp = client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("Anthropic API error {}: {}", status, body));
        }

        let resp: AnthropicResponse = resp.json()?;
        resp.content
            .into_iter()
            .find(|b| b.content_type == "text")
            .map(|b| b.text)
            .ok_or_else(|| anyhow!("No text content in Anthropic response"))
    }

    /// Call an OpenAI-compatible Chat Completions API (works with Ollama, OpenRouter, etc.)
    fn complete_openai(&self, system: &str, user_message: &str) -> Result<String> {
        let client = reqwest::blocking::Client::new();

        // Ollama uses /api/chat, but most OpenAI-compat use /v1/chat/completions
        // Detect Ollama by checking if base_url contains localhost:11434
        let url = if self.base_url.contains("localhost:11434")
            || self.base_url.contains("127.0.0.1:11434")
        {
            format!("{}/api/chat", self.base_url)
        } else {
            format!("{}/v1/chat/completions", self.base_url)
        };

        let body = OpenAIRequest {
            model: &self.model,
            messages: vec![
                OpenAIMessage {
                    role: "system",
                    content: system,
                },
                OpenAIMessage {
                    role: "user",
                    content: user_message,
                },
            ],
            temperature: 0.3,
        };

        let mut req = client
            .post(&url)
            .header("content-type", "application/json")
            .timeout(std::time::Duration::from_secs(60));

        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req.json(&body).send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("OpenAI-compat API error {}: {}", status, body));
        }

        // Ollama returns a slightly different format, but both have choices[0].message.content
        let resp: OpenAIResponse = resp.json()?;
        resp.choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow!("No choices in OpenAI-compat response"))
    }
}

// --- Anthropic API types ---

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<AnthropicMessage<'a>>,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

// --- OpenAI-compatible API types ---

#[derive(Serialize)]
struct OpenAIRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAIMessage<'a>>,
    temperature: f32,
}

#[derive(Serialize)]
struct OpenAIMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Deserialize)]
struct OpenAIChoice {
    message: OpenAIChoiceMessage,
}

#[derive(Deserialize)]
struct OpenAIChoiceMessage {
    content: String,
}
