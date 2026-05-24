use super::decision::{decision_tool_definition, StrategyDecision};
use anyhow::{bail, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-4-7";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicClient {
    client:  Client,
    api_key: String,
}

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model:      &'a str,
    max_tokens: u32,
    system:     &'a str,
    messages:   Vec<Message<'a>>,
    tools:      Vec<Value>,
    tool_choice: Value,
}

#[derive(Serialize)]
struct Message<'a> {
    role:    &'a str,
    content: &'a str,
}

#[derive(Deserialize, Debug)]
struct MessagesResponse {
    content:      Vec<ContentBlock>,
    stop_reason:  String,
    usage:        Usage,
}

#[derive(Deserialize, Debug)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    // For tool_use blocks
    name:  Option<String>,
    input: Option<Value>,
    // For text blocks
    text:  Option<String>,
}

#[derive(Deserialize, Debug)]
struct Usage {
    input_tokens:  u32,
    output_tokens: u32,
}

// ── Client implementation ─────────────────────────────────────────────────────

impl AnthropicClient {
    pub fn new(api_key: String) -> Self {
        AnthropicClient {
            client: Client::new(),
            api_key,
        }
    }

    pub async fn call_with_tools(
        &self,
        system:  &str,
        user_msg: &str,
    ) -> Result<StrategyDecision> {
        let tool_def = decision_tool_definition();

        let request = MessagesRequest {
            model:      MODEL,
            max_tokens: 1024,
            system,
            messages:   vec![Message { role: "user", content: user_msg }],
            tools:      vec![tool_def],
            // Force the model to call our tool — no free-form text output
            tool_choice: json!({ "type": "tool", "name": "submit_decision" }),
        };

        let resp = self.client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key",         &self.api_key)
            .header("anthropic-version",  ANTHROPIC_VERSION)
            .header("content-type",       "application/json")
            .json(&request)
            .send()
            .await?;

        let status = resp.status();
        let body   = resp.text().await?;

        if !status.is_success() {
            bail!("Anthropic API error [{}]: {}", status, body);
        }

        debug!("Anthropic response: {}", &body[..body.len().min(500)]);

        let parsed: MessagesResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Failed to parse Anthropic response: {} | body: {}", e, &body[..body.len().min(200)]))?;

        // Log token usage
        tracing::info!(
            input_tokens  = parsed.usage.input_tokens,
            output_tokens = parsed.usage.output_tokens,
            "Anthropic API usage"
        );

        if parsed.stop_reason != "tool_use" {
            warn!("Unexpected stop_reason: {}", parsed.stop_reason);
        }

        // Extract the tool_use block
        for block in &parsed.content {
            if block.block_type == "tool_use" {
                if let (Some(name), Some(input)) = (&block.name, &block.input) {
                    if name == "submit_decision" {
                        let decision: StrategyDecision = serde_json::from_value(input.clone())
                            .map_err(|e| anyhow::anyhow!("Failed to parse decision tool input: {} | input: {}", e, input))?;
                        return Ok(decision);
                    }
                }
            }
        }

        bail!("No submit_decision tool call found in Anthropic response")
    }
}
