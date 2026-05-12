use std::sync::{
  atomic::{AtomicBool, Ordering},
  Arc,
};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::models::TokenUsage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
  pub role: String,
  pub content: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApiRuntimeConfig {
  pub api_key: String,
  pub base_url: String,
  pub model: String,
  pub temperature: f32,
  pub max_tokens: i64,
  pub include_temperature: bool,
  pub deepseek_thinking_type: Option<String>,
  pub deepseek_reasoning_effort: Option<String>,
}

pub struct StreamCallbacks<FC, FR>
where
  FC: FnMut(&str) + Send + 'static,
  FR: FnMut(&str) + Send + 'static,
{
  pub on_content: FC,
  pub on_reasoning: FR,
}

pub fn normalize_base_url(base_url: &str) -> String {
  let raw = base_url.trim().trim_end_matches('/');
  if raw.is_empty() {
    return "https://api.deepseek.com/v1/chat/completions".to_string();
  }
  if raw.ends_with("/chat/completions") {
    raw.to_string()
  } else if raw.ends_with("/v1") {
    format!("{raw}/chat/completions")
  } else {
    format!("{raw}/v1/chat/completions")
  }
}

fn parse_api_error_text(text: &str) -> String {
  let trimmed = text.trim();
  if trimmed.is_empty() {
    return "empty response body".to_string();
  }
  if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
    if let Some(msg) = v
      .get("error")
      .and_then(|x| x.get("message").or_else(|| x.get("msg")))
      .and_then(Value::as_str)
    {
      return msg.to_string();
    }
    if let Some(msg) = v.get("message").and_then(Value::as_str) {
      return msg.to_string();
    }
  }
  trimmed.to_string()
}

fn parse_token_usage(v: &Value) -> TokenUsage {
  TokenUsage {
    prompt: v.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
    completion: v
      .get("completion_tokens")
      .and_then(Value::as_i64)
      .unwrap_or(0),
    total: v.get("total_tokens").and_then(Value::as_i64).unwrap_or(0),
  }
}

fn extract_reasoning(delta: &Value) -> Option<String> {
  if let Some(s) = delta
    .get("reasoning_content")
    .or_else(|| delta.get("reasoning"))
    .and_then(Value::as_str)
  {
    if !s.is_empty() {
      return Some(s.to_string());
    }
  }
  None
}

fn read_content_text(node: &Value) -> String {
  if let Some(s) = node.as_str() {
    return s.to_string();
  }
  if let Some(items) = node.as_array() {
    let mut out = String::new();
    for item in items {
      if let Some(text) = item.get("text").and_then(Value::as_str) {
        out.push_str(text);
      } else if let Some(text) = item
        .get("content")
        .and_then(|x| x.get("text"))
        .and_then(Value::as_str)
      {
        out.push_str(text);
      }
    }
    return out;
  }
  String::new()
}

fn strip_tag_blocks(mut input: String, open: &str, close: &str, sink: &mut Vec<String>) -> String {
  loop {
    let Some(start) = input.find(open) else {
      break;
    };
    let content_start = start + open.len();
    let Some(rel_end) = input[content_start..].find(close) else {
      break;
    };
    let end = content_start + rel_end;
    let inner = input[content_start..end].trim();
    if !inner.is_empty() {
      sink.push(inner.to_string());
    }
    input.replace_range(start..(end + close.len()), "");
  }
  input
}

fn split_inline_reasoning(content: &str) -> (String, String) {
  let mut reasoning_parts: Vec<String> = Vec::new();
  let mut cleaned = content.to_string();
  cleaned = strip_tag_blocks(cleaned, "<thinking>", "</thinking>", &mut reasoning_parts);
  cleaned = strip_tag_blocks(cleaned, "<think>", "</think>", &mut reasoning_parts);
  (cleaned.trim().to_string(), reasoning_parts.join("\n\n"))
}

fn extract_delta_content(delta: &Value) -> Option<String> {
  if let Some(content) = delta.get("content").and_then(Value::as_str) {
    if !content.is_empty() {
      return Some(content.to_string());
    }
  }

  let content_node = delta.get("content").unwrap_or(&Value::Null);
  let from_node = read_content_text(content_node);
  if !from_node.is_empty() {
    return Some(from_node);
  }

  if let Some(text) = delta.get("text").and_then(Value::as_str) {
    if !text.is_empty() {
      return Some(text.to_string());
    }
  }
  None
}

fn parse_non_stream_completion(body_text: &str) -> Result<(String, String, TokenUsage)> {
  let v: Value = serde_json::from_str(body_text)
    .with_context(|| format!("parse non-stream response failed: {body_text}"))?;

  if let Some(error_msg) = v
    .get("error")
    .and_then(|x| x.get("message").or_else(|| x.get("msg")))
    .and_then(Value::as_str)
  {
    anyhow::bail!("model api error: {error_msg}");
  }

  let usage = v.get("usage").map(parse_token_usage).unwrap_or_default();

  let choice = v
    .get("choices")
    .and_then(Value::as_array)
    .and_then(|choices| choices.first())
    .cloned()
    .unwrap_or(Value::Null);

  let msg = choice.get("message").cloned().unwrap_or(Value::Null);

  let mut reasoning_content = msg
    .get("reasoning_content")
    .or_else(|| msg.get("reasoning"))
    .or_else(|| choice.get("reasoning_content"))
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_string();

  let content_raw = if !msg.is_null() {
    read_content_text(msg.get("content").unwrap_or(&Value::Null))
  } else {
    choice
      .get("text")
      .and_then(Value::as_str)
      .unwrap_or("")
      .to_string()
  };

  let (content, inline_reasoning) = split_inline_reasoning(&content_raw);
  if !inline_reasoning.is_empty() {
    if reasoning_content.trim().is_empty() {
      reasoning_content = inline_reasoning;
    } else {
      reasoning_content = format!("{reasoning_content}\n\n{inline_reasoning}");
    }
  }

  Ok((content, reasoning_content, usage))
}

async fn create_stream_response(
  client: &reqwest::Client,
  config: &ApiRuntimeConfig,
  messages: &[ApiMessage],
  include_usage: bool,
) -> Result<reqwest::Response> {
  let endpoint = normalize_base_url(&config.base_url);
  let mut body = json!({
    "model": config.model,
    "messages": messages,
    "max_tokens": config.max_tokens,
    "stream": true
  });
  if config.include_temperature {
    body["temperature"] = json!(config.temperature);
  }
  if let Some(t) = config.deepseek_thinking_type.as_deref() {
    body["thinking"] = json!({ "type": t });
  }
  if let Some(eff) = config.deepseek_reasoning_effort.as_deref() {
    body["reasoning_effort"] = json!(eff);
  }
  if include_usage {
    body["stream_options"] = json!({ "include_usage": true });
  }
  let mut req = client
    .post(endpoint)
    .header("Content-Type", "application/json")
    .json(&body);
  if !config.api_key.trim().is_empty() {
    req = req.bearer_auth(&config.api_key);
  }
  let response = req.send().await.context("call model api failed")?;
  Ok(response)
}

async fn create_non_stream_response(
  client: &reqwest::Client,
  config: &ApiRuntimeConfig,
  messages: &[ApiMessage],
) -> Result<reqwest::Response> {
  let endpoint = normalize_base_url(&config.base_url);
  let body = json!({
    "model": config.model,
    "messages": messages,
    "max_tokens": config.max_tokens,
    "stream": false
  });
  let mut body = body;
  if config.include_temperature {
    body["temperature"] = json!(config.temperature);
  }
  if let Some(t) = config.deepseek_thinking_type.as_deref() {
    body["thinking"] = json!({ "type": t });
  }
  if let Some(eff) = config.deepseek_reasoning_effort.as_deref() {
    body["reasoning_effort"] = json!(eff);
  }
  let mut req = client
    .post(endpoint)
    .header("Content-Type", "application/json")
    .json(&body);
  if !config.api_key.trim().is_empty() {
    req = req.bearer_auth(&config.api_key);
  }
  let response = req
    .send()
    .await
    .context("call model api (non-stream) failed")?;
  Ok(response)
}

pub async fn stream_chat_completion<FC, FR>(
  config: ApiRuntimeConfig,
  messages: Vec<ApiMessage>,
  callbacks: StreamCallbacks<FC, FR>,
  cancel: Arc<AtomicBool>,
) -> Result<(String, String, TokenUsage)>
where
  FC: FnMut(&str) + Send + 'static,
  FR: FnMut(&str) + Send + 'static,
{
  let client = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(12))
    .timeout(Duration::from_secs(180))
    .build()
    .context("build http client failed")?;

  let mut response = create_stream_response(&client, &config, &messages, true).await?;
  if !response.status().is_success() {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if text.contains("stream_options") || text.to_lowercase().contains("unsupported") {
      response = create_stream_response(&client, &config, &messages, false).await?;
      if !response.status().is_success() {
        let non_stream = create_non_stream_response(&client, &config, &messages).await?;
        if !non_stream.status().is_success() {
          let status2 = non_stream.status();
          let text2 = non_stream.text().await.unwrap_or_default();
          anyhow::bail!("model api error [{}]: {}", status2, parse_api_error_text(&text2));
        }
        let body = non_stream.text().await.unwrap_or_default();
        let (content, reasoning, usage) = parse_non_stream_completion(&body)?;
        let mut on_content = callbacks.on_content;
        let mut on_reasoning = callbacks.on_reasoning;
        if !reasoning.is_empty() {
          on_reasoning(&reasoning);
        }
        if !content.is_empty() {
          on_content(&content);
        }
        return Ok((content, reasoning, usage));
      }
    } else if text.to_lowercase().contains("stream") {
      let non_stream = create_non_stream_response(&client, &config, &messages).await?;
      if !non_stream.status().is_success() {
        let status2 = non_stream.status();
        let text2 = non_stream.text().await.unwrap_or_default();
        anyhow::bail!("model api error [{}]: {}", status2, parse_api_error_text(&text2));
      }
      let body = non_stream.text().await.unwrap_or_default();
      let (content, reasoning, usage) = parse_non_stream_completion(&body)?;
      let mut on_content = callbacks.on_content;
      let mut on_reasoning = callbacks.on_reasoning;
      if !reasoning.is_empty() {
        on_reasoning(&reasoning);
      }
      if !content.is_empty() {
        on_content(&content);
      }
      return Ok((content, reasoning, usage));
    } else {
      anyhow::bail!("model api error [{}]: {}", status, parse_api_error_text(&text));
    }
  }

  let mut on_content = callbacks.on_content;
  let mut on_reasoning = callbacks.on_reasoning;

  // Some providers return normal JSON even when stream=true. Fallback safely.
  let content_type = response
    .headers()
    .get(reqwest::header::CONTENT_TYPE)
    .and_then(|x| x.to_str().ok())
    .unwrap_or("")
    .to_lowercase();
  if !content_type.contains("text/event-stream") {
    let body = response.text().await.unwrap_or_default();
    let (content, reasoning, usage) = parse_non_stream_completion(&body)?;
    if !reasoning.is_empty() {
      on_reasoning(&reasoning);
    }
    if !content.is_empty() {
      on_content(&content);
    }
    return Ok((content, reasoning, usage));
  }

  let mut content_full = String::new();
  let mut reasoning_full = String::new();
  let mut usage = TokenUsage::default();
  let mut buf = String::new();
  let mut stream = response.bytes_stream();
  let mut done_received = false;

  while let Some(item) = stream.next().await {
    if cancel.load(Ordering::Relaxed) {
      break;
    }
    let chunk = item?;
    let s = String::from_utf8_lossy(&chunk);
    buf.push_str(&s);
    while let Some(pos) = buf.find('\n') {
      let line = buf[..pos].trim().to_string();
      buf.drain(..=pos);
      if line.is_empty() || !line.starts_with("data:") {
        continue;
      }
      let payload = line.trim_start_matches("data:").trim();
      if payload == "[DONE]" {
        done_received = true;
        break;
      }
      let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => continue,
      };
      if let Some(error_msg) = v
        .get("error")
        .and_then(|x| x.get("message").or_else(|| x.get("msg")))
        .and_then(Value::as_str)
      {
        anyhow::bail!("model stream error: {error_msg}");
      }
      let delta = v
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|x| x.get("delta"));

      if let Some(reasoning) = delta.and_then(extract_reasoning) {
        reasoning_full.push_str(&reasoning);
        on_reasoning(&reasoning);
      }
      if let Some(content) = delta.and_then(extract_delta_content) {
        content_full.push_str(&content);
        on_content(&content);
      }
      if let Some(u) = v.get("usage") {
        usage = parse_token_usage(u);
      }
    }
    if done_received {
      break;
    }
  }

  let (content_clean, inline_reasoning) = split_inline_reasoning(&content_full);
  if !inline_reasoning.is_empty() {
    if reasoning_full.trim().is_empty() {
      reasoning_full = inline_reasoning;
    } else {
      reasoning_full = format!("{reasoning_full}\n\n{inline_reasoning}");
    }
  }

  Ok((content_clean, reasoning_full, usage))
}
