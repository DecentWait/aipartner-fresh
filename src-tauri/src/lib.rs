mod api;
mod db;
mod importer;
mod memory;
mod models;

use std::{
  collections::HashMap,
  fs,
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
  thread,
  time::{Duration, SystemTime},
};

use api::{stream_chat_completion, ApiMessage, ApiRuntimeConfig, StreamCallbacks};
use db::Database;
use models::{
  AppSettingsPayload, BackupResult, ChatSettings, Conversation, ConversationIdInput, CreateConversationInput,
  ConversationFile, DeleteConversationFileInput, DeleteMessageInput, EditMessageInput, ImportJsonInput,
  ImportResult, ListConversationFilesInput, ListInput, ListMemoriesInput, ListMessagesInput, Memory, Message,
  MoveConversationInput, RegenerateInput, RenameConversationInput, SaveApiKeyInput, SendMessageInput,
  SetConversationChatSettingsInput, SetConversationModelInput, SetConversationPinnedInput, StreamDonePayload,
  StreamErrorPayload, StreamTokenPayload, TokenUsage, UpdateMemoryScopeInput, UploadTxtFileInput,
};
use parking_lot::Mutex;
use tauri::{Emitter, Manager, State};
use uuid::Uuid;

type CmdResult<T> = Result<T, String>;
const AUTO_BACKUP_PREFIX: &str = "auto_";
const AUTO_BACKUP_INTERVAL_HOURS: u64 = 24;
const AUTO_BACKUP_RETENTION: usize = 14;
const AUTO_BACKUP_RECENT_THRESHOLD_SECS: u64 = 20 * 60 * 60;

pub struct AppState {
  app_root: PathBuf,
  files_dir: PathBuf,
  db: Arc<Database>,
  backup_dir: PathBuf,
  config_file: PathBuf,
  generation_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl AppState {
  fn register_generation(&self, conversation_id: &str) -> Arc<AtomicBool> {
    let mut guard = self.generation_flags.lock();
    let flag = Arc::new(AtomicBool::new(false));
    guard.insert(conversation_id.to_string(), flag.clone());
    flag
  }

  fn stop_generation(&self, conversation_id: &str) -> bool {
    if let Some(flag) = self.generation_flags.lock().get(conversation_id) {
      flag.store(true, Ordering::Relaxed);
      return true;
    }
    false
  }

  fn clear_generation(&self, conversation_id: &str) {
    let _ = self.generation_flags.lock().remove(conversation_id);
  }

  fn is_generating(&self, conversation_id: &str) -> bool {
    self.generation_flags.lock().contains_key(conversation_id)
  }
}

fn err<E: std::fmt::Display>(e: E) -> String {
  e.to_string()
}

fn normalize_title(input: &str) -> String {
  let cleaned = input.trim().replace('\n', " ");
  if cleaned.is_empty() {
    return "New Chat".to_string();
  }
  let mut out = String::new();
  for ch in cleaned.chars().take(36) {
    out.push(ch);
  }
  out
}

fn cfg_key(provider: &str) -> String {
  format!("api_key_{}", provider.trim().to_lowercase())
}

fn normalize_provider(input: &str) -> String {
  let p = input.trim().to_lowercase();
  if p.is_empty() {
    "deepseek".to_string()
  } else {
    p
  }
}

fn provider_default_base_url(provider: &str) -> Option<&'static str> {
  match provider {
    "deepseek" => Some("https://api.deepseek.com"),
    "openai" => Some("https://api.openai.com/v1"),
    "openrouter" => Some("https://openrouter.ai/api/v1"),
    "ollama" => Some("http://127.0.0.1:11434/v1"),
    "custom" => None,
    _ => None,
  }
}

fn resolve_base_url(provider: &str, configured: &str) -> Option<String> {
  let trimmed = configured.trim();
  if !trimmed.is_empty() {
    return Some(trimmed.to_string());
  }
  provider_default_base_url(provider).map(str::to_string)
}

fn provider_requires_api_key(provider: &str) -> bool {
  matches!(provider, "deepseek" | "openai" | "openrouter")
}

fn normalize_thinking_override(input: &str) -> String {
  match input.trim().to_lowercase().as_str() {
    "enabled" => "enabled".to_string(),
    "disabled" => "disabled".to_string(),
    _ => String::new(),
  }
}

fn normalize_reasoning_effort(input: &str) -> String {
  match input.trim().to_lowercase().as_str() {
    "max" => "max".to_string(),
    "high" => "high".to_string(),
    _ => String::new(),
  }
}

fn should_replay_reasoning_content(provider: &str, model: &str, thinking_type: &str) -> bool {
  let p = normalize_provider(provider);
  if p != "deepseek" {
    return false;
  }
  if model.trim().eq_ignore_ascii_case("deepseek-reasoner") {
    return false;
  }
  if thinking_type.eq_ignore_ascii_case("enabled") {
    // Current app does not support tool-call rounds yet; avoid replaying reasoning_content
    // to keep compatibility with DeepSeek non-tool multi-turn behavior.
    return false;
  }
  false
}

fn supports_deepseek_thinking_params(model: &str) -> bool {
  matches!(
    model.trim().to_lowercase().as_str(),
    "deepseek-v4-pro" | "deepseek-v4-flash"
  )
}

fn normalize_theme_mode(input: &str) -> &'static str {
  match input.trim().to_lowercase().as_str() {
    "light" => "light",
    "dark" => "dark",
    _ => "system",
  }
}

fn sync_settings_file(state: &AppState) -> CmdResult<()> {
  let chat = state.db.get_chat_settings();
  let provider = normalize_provider(&chat.provider);
  let mut api_keys = serde_json::Map::new();
  for p in ["deepseek", "openai", "openrouter", "ollama", "custom"] {
    api_keys.insert(
      p.to_string(),
      serde_json::Value::String(state.db.get_setting(&cfg_key(p), "")),
    );
  }
  if !api_keys.contains_key(&provider) {
    api_keys.insert(
      provider.clone(),
      serde_json::Value::String(state.db.get_setting(&cfg_key(&provider), "")),
    );
  }
  let payload = serde_json::json!({
    "chat": chat,
    "models": state.db.get_models(),
    "last_conversation_id": state.db.get_setting("last_conversation_id", ""),
    "theme_mode": state.db.get_setting("theme_mode", "system"),
    "api_keys": api_keys
  });
  if let Some(parent) = state.config_file.parent() {
    fs::create_dir_all(parent).map_err(err)?;
  }
  fs::write(
    &state.config_file,
    serde_json::to_string_pretty(&payload).map_err(err)?,
  )
  .map_err(err)
}

fn maybe_load_settings_file(state: &AppState) -> CmdResult<()> {
  if !state.config_file.exists() {
    return sync_settings_file(state);
  }
  let raw = fs::read_to_string(&state.config_file).map_err(err)?;
  let v = serde_json::from_str::<serde_json::Value>(&raw).map_err(err)?;
  if let Some(chat) = v.get("chat") {
    if let Ok(chat_settings) = serde_json::from_value::<ChatSettings>(chat.clone()) {
      state.db.set_chat_settings(&chat_settings).map_err(err)?;
    }
  }
  if let Some(last_id) = v
    .get("last_conversation_id")
    .and_then(serde_json::Value::as_str)
  {
    state
      .db
      .set_setting("last_conversation_id", last_id)
      .map_err(err)?;
  }
  if let Some(theme_mode) = v.get("theme_mode").and_then(serde_json::Value::as_str) {
    state
      .db
      .set_setting("theme_mode", normalize_theme_mode(theme_mode))
      .map_err(err)?;
  }
  if let Some(models) = v.get("models") {
    if let Ok(items) = serde_json::from_value::<Vec<String>>(models.clone()) {
      if !items.is_empty() {
        state.db.set_models(&items).map_err(err)?;
      }
    }
  }
  if let Some(api_keys) = v.get("api_keys").and_then(serde_json::Value::as_object) {
    for (provider, key_val) in api_keys {
      if let Some(key) = key_val.as_str() {
        if !key.trim().is_empty() {
          state
            .db
            .set_setting(&cfg_key(provider), key.trim())
            .map_err(err)?;
        }
      }
    }
  }
  sync_settings_file(state)
}

fn load_api_key(db: &Database, provider: &str) -> Option<String> {
  let key = db.get_setting(&cfg_key(&normalize_provider(provider)), "");
  let trimmed = key.trim();
  if trimmed.is_empty() {
    None
  } else {
    Some(trimmed.to_string())
  }
}

fn estimate_tokens(input: &str) -> i64 {
  if input.is_empty() {
    return 1;
  }
  ((input.chars().count() as i64) + 3) / 4 + 1
}

fn estimate_messages_tokens(messages: &[ApiMessage]) -> i64 {
  messages
    .iter()
    .map(|m| estimate_tokens(&m.role) + estimate_tokens(&m.content) + 4)
    .sum()
}

fn clip(input: &str, max_chars: usize) -> String {
  if input.chars().count() <= max_chars {
    return input.to_string();
  }
  input.chars().take(max_chars).collect()
}

const MAX_TXT_FILE_BYTES: i64 = 1_048_576;

fn is_txt_file_name(name: &str) -> bool {
  Path::new(name)
    .extension()
    .and_then(|x| x.to_str())
    .map(|x| x.eq_ignore_ascii_case("txt"))
    .unwrap_or(false)
}

fn sanitize_txt_file_name(name: &str) -> String {
  let source = Path::new(name)
    .file_name()
    .and_then(|x| x.to_str())
    .unwrap_or("upload.txt")
    .trim();

  let mut out = String::new();
  for ch in source.chars() {
    if ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ') {
      out.push(ch);
    } else {
      out.push('_');
    }
  }
  let mut out = out.trim().replace(' ', "_");
  if out.is_empty() {
    out = "upload.txt".to_string();
  }
  if !out.to_lowercase().ends_with(".txt") {
    out.push_str(".txt");
  }
  while out.starts_with('.') {
    out.remove(0);
  }
  if out.is_empty() {
    out = "upload.txt".to_string();
  }
  if out.chars().count() > 96 {
    let mut truncated: String = out.chars().take(92).collect();
    if !truncated.to_lowercase().ends_with(".txt") {
      truncated.push_str(".txt");
    }
    out = truncated;
  }
  out
}

fn summarize_txt_content(content: &str) -> String {
  let mut lines: Vec<String> = Vec::new();
  for raw in content.lines() {
    let line = raw.trim();
    if line.is_empty() {
      continue;
    }
    let clipped = clip(line, 120);
    if !lines.iter().any(|x| x == &clipped) {
      lines.push(clipped);
    }
    if lines.len() >= 6 {
      break;
    }
  }
  if lines.is_empty() {
    let compact = content
      .replace('\r', " ")
      .replace('\n', " ")
      .split_whitespace()
      .collect::<Vec<_>>()
      .join(" ");
    if compact.is_empty() {
      return String::new();
    }
    return clip(&compact, 320);
  }
  clip(&lines.join(" | "), 600)
}

fn looks_like_raw_txt_request(user_text: &str) -> bool {
  let lower = user_text.to_lowercase();
  let has_file_ref = lower.contains("txt")
    || lower.contains("file")
    || user_text.contains("文件")
    || user_text.contains("附件");
  let has_raw_intent = lower.contains("full text")
    || lower.contains("original text")
    || lower.contains("verbatim")
    || lower.contains("quote")
    || user_text.contains("原文")
    || user_text.contains("全文")
    || user_text.contains("逐字")
    || user_text.contains("引用")
    || user_text.contains("完整内容")
    || user_text.contains("文件内容");
  has_file_ref && has_raw_intent
}

fn resolve_stored_txt_path(state: &AppState, stored: &str) -> Option<PathBuf> {
  let rel = stored.trim();
  if rel.is_empty() {
    return None;
  }
  let rel_path = PathBuf::from(rel);
  if rel_path.is_absolute() {
    return None;
  }
  let full = state.app_root.join(&rel_path);
  let full_canonical = full.canonicalize().ok()?;
  let files_canonical = state.files_dir.canonicalize().ok()?;
  if full_canonical.starts_with(&files_canonical) {
    Some(full_canonical)
  } else {
    None
  }
}

fn read_txt_raw_for_prompt(state: &AppState, file: &ConversationFile) -> String {
  if let Some(path) = resolve_stored_txt_path(state, &file.file_path) {
    if let Ok(bytes) = fs::read(path) {
      if let Ok(text) = String::from_utf8(bytes.clone()) {
        return text;
      }
      return String::from_utf8_lossy(&bytes).to_string();
    }
  }
  file.content_text.clone()
}

fn conversation_file_lines_for_prompt(
  state: &AppState,
  conversation_id: &str,
  user_text: &str,
) -> Vec<String> {
  let files = state
    .db
    .list_conversation_files_for_context(conversation_id, 8)
    .unwrap_or_default();
  if files.is_empty() {
    return Vec::new();
  }

  let use_raw = looks_like_raw_txt_request(user_text);
  let mut lines: Vec<String> = Vec::new();

  if use_raw {
    let mut remain_chars = 4200usize;
    for file in files {
      if remain_chars <= 200 {
        break;
      }
      let raw = read_txt_raw_for_prompt(state, &file);
      if raw.trim().is_empty() {
        continue;
      }
      let clip_len = remain_chars.min(1200);
      let excerpt = clip(raw.trim(), clip_len);
      remain_chars = remain_chars.saturating_sub(excerpt.chars().count());
      lines.push(format!(
        "- {} ({} bytes)\n  summary: {}\n  excerpt:\n{}",
        file.file_name,
        file.file_size.max(0),
        clip(file.summary.trim(), 280),
        excerpt
      ));
    }
    return lines;
  }

  for file in files {
    let summary = if file.summary.trim().is_empty() {
      summarize_txt_content(&file.content_text)
    } else {
      clip(file.summary.trim(), 320)
    };
    if summary.trim().is_empty() {
      continue;
    }
    lines.push(format!(
      "- {} ({} bytes): {}",
      file.file_name,
      file.file_size.max(0),
      summary
    ));
    if lines.len() >= 8 {
      break;
    }
  }
  lines
}

fn profile_line(label: &str, value: &str, max_chars: usize) -> Option<String> {
  let clean = value.trim();
  if clean.is_empty() {
    None
  } else {
    Some(format!("- {label}: {}", clip(clean, max_chars)))
  }
}

fn memory_lines_for_prompt(
  state: &AppState,
  conversation_id: &str,
  user_text: &str,
  limit: i64,
) -> Vec<String> {
  if limit <= 0 {
    return Vec::new();
  }
  state
    .db
    .search_memories(user_text, Some(conversation_id), limit)
    .unwrap_or_default()
    .into_iter()
    .map(|m| format!("- ({}) {}", m.importance, clip(&m.content, 220)))
    .collect()
}

fn build_context_messages(
  state: &AppState,
  conversation_id: &str,
  user_text: &str,
) -> Vec<ApiMessage> {
  let settings = state.db.get_chat_settings();
  let (
    provider_override,
    model_override,
    _base_url_override,
    _temperature_override,
    _max_tokens_override,
    max_context_tokens_override,
    max_recent_messages_override,
    max_memory_items_override,
    system_prompt_override,
    thinking_override,
    _reasoning_effort_override,
  ) = state
    .db
    .get_conversation_chat_overrides(conversation_id)
    .unwrap_or_default();
  let effective_provider = if provider_override.trim().is_empty() {
    normalize_provider(&settings.provider)
  } else {
    normalize_provider(&provider_override)
  };
  let effective_model = if model_override.trim().is_empty() {
    settings.model.trim().to_string()
  } else {
    model_override.trim().to_string()
  };
  let effective_thinking = if effective_provider == "deepseek" {
    let thinking = normalize_thinking_override(&thinking_override);
    if thinking.is_empty() {
      "enabled".to_string()
    } else {
      thinking
    }
  } else {
    String::new()
  };
  let replay_reasoning = should_replay_reasoning_content(
    &effective_provider,
    &effective_model,
    &effective_thinking,
  );
  let max_context_tokens = max_context_tokens_override
    .unwrap_or(settings.max_context_tokens)
    .clamp(2048, 128000);
  let max_recent_messages = max_recent_messages_override
    .unwrap_or(settings.max_recent_messages)
    .clamp(4, 48);
  let max_memory_items = max_memory_items_override
    .unwrap_or(settings.max_memory_items)
    .clamp(0, 16);
  let current_input_chars = ((max_context_tokens * 2).clamp(1200, 24000)) as usize;

  let user_profile = state.db.get_user_profile();
  let style_profile = state.db.get_style_profile();
  let summary_segments = state
    .db
    .list_summary_segments(conversation_id, 8)
    .unwrap_or_default();
  let memories = memory_lines_for_prompt(state, conversation_id, user_text, max_memory_items);
  let file_lines = conversation_file_lines_for_prompt(state, conversation_id, user_text);

  let recent_all = state
    .db
    .get_recent_messages(conversation_id, max_recent_messages)
    .unwrap_or_default();
  let mut recent: Vec<ApiMessage> = recent_all
    .iter()
    .filter(|m| (m.role == "user" || m.role == "assistant") && !m.content.trim().is_empty())
    .map(|m| ApiMessage {
      role: m.role.clone(),
      content: clip(&m.content, 7000),
      reasoning_content: if replay_reasoning
        && m.role == "assistant"
        && !m.reasoning_content.trim().is_empty()
      {
        Some(clip(&m.reasoning_content, 7000))
      } else {
        None
      },
    })
    .collect();

  let current_user = ApiMessage {
    role: "user".to_string(),
    content: clip(user_text, current_input_chars),
    reasoning_content: None,
  };
  let has_latest_user = recent_all
    .last()
    .map(|m| m.role == "user" && m.content.trim() == user_text.trim())
    .unwrap_or(false);
  let min_recent_keep = if has_latest_user { 1usize } else { 0usize };
  let build_required = |recent_items: &Vec<ApiMessage>| {
    let mut out = recent_items.clone();
    if !has_latest_user {
      out.push(current_user.clone());
    }
    out
  };
  let mut required = build_required(&recent);

  // Always prioritize current input + recent history. Drop oldest recent items when over budget.
  while estimate_messages_tokens(&required) > max_context_tokens.saturating_sub(256)
    && recent.len() > min_recent_keep
  {
    recent.remove(0);
    required = build_required(&recent);
  }
  if required.is_empty() {
    required.push(current_user.clone());
  }

  let core_persona = state
    .db
    .get_setting("core_persona_prompt", "")
    .trim()
    .to_string();
  let system_prompt_section = {
    let clean = system_prompt_override.trim();
    if clean.is_empty() {
      String::new()
    } else {
      let max_prompt_tokens = (max_context_tokens / 3).clamp(256, 8000);
      let prompt_chars = (max_prompt_tokens * 4) as usize;
      format!("[Conversation System Prompt]\n{}", clip(clean, prompt_chars))
    }
  };
  let user_lines: Vec<String> = [
    profile_line("preferred_name", &user_profile.preferred_name, 48),
    profile_line("long_term_goals", &user_profile.long_term_goals, 420),
    profile_line("interests", &user_profile.interests, 300),
    profile_line("important_experiences", &user_profile.important_experiences, 420),
    profile_line("language_preference", &user_profile.language_preference, 24),
    profile_line("notes", &user_profile.notes, 420),
    profile_line("style_detail_level", &style_profile.detail_level, 24),
    profile_line("style_tone", &style_profile.tone, 24),
    profile_line("style_technical_level", &style_profile.technical_level, 24),
    profile_line("style_language_style", &style_profile.language_style, 24),
    profile_line("style_explicit_preferences", &style_profile.explicit_preferences, 360),
  ]
  .into_iter()
  .flatten()
  .collect();
  let user_profile_section = if user_lines.is_empty() {
    String::new()
  } else {
    format!("[User Profile]\n{}", user_lines.join("\n"))
  };

  let memory_section = if memories.is_empty() {
    String::new()
  } else {
    format!("[Relevant Long-Term Memories]\n{}", memories.join("\n"))
  };

  let summary_section = if summary_segments.is_empty() {
    String::new()
  } else {
    let mut lines: Vec<String> = Vec::new();
    for seg in summary_segments.iter().rev() {
      lines.push(format!(
        "- seg#{} {}",
        seg.segment_index,
        clip(seg.summary.trim(), 600)
      ));
    }
    format!("[Older Summaries]\n{}", lines.join("\n"))
  };

  let file_section = if file_lines.is_empty() {
    String::new()
  } else if looks_like_raw_txt_request(user_text) {
    format!("[Current Conversation TXT Attachments - Raw Excerpts On Demand]\n{}", file_lines.join("\n"))
  } else {
    format!("[Current Conversation TXT Attachments - Summaries]\n{}", file_lines.join("\n"))
  };

  let mut pinned_sections: Vec<String> = Vec::new();
  if !system_prompt_section.trim().is_empty() {
    pinned_sections.push(system_prompt_section);
  }
  if !core_persona.trim().is_empty() {
    pinned_sections.push(core_persona);
  }
  let pinned_text = pinned_sections.join("\n\n");
  let pinned_tokens = estimate_tokens(&pinned_text) + 8;

  // Keep conversation-level system prompt and current input/recent first, then trim oldest recent messages.
  while estimate_messages_tokens(&required) + pinned_tokens > max_context_tokens.saturating_sub(96)
    && recent.len() > min_recent_keep
  {
    recent.remove(0);
    required = build_required(&recent);
  }
  if required.is_empty() {
    required.push(current_user.clone());
  }

  let mut sections = pinned_sections;
  let mut used_tokens = pinned_tokens + estimate_messages_tokens(&required);

  // Priority after pinned sections: profile, txt summaries/raw, memories, summaries.
  for section in [user_profile_section, file_section, memory_section, summary_section] {
    if section.trim().is_empty() {
      continue;
    }
    let sec_tokens = estimate_tokens(&section) + 8;
    if used_tokens + sec_tokens <= max_context_tokens {
      sections.push(section);
      used_tokens += sec_tokens;
    }
  }

  let mut out = vec![ApiMessage {
    role: "system".to_string(),
    content: clip(&sections.join("\n\n"), 32000),
    reasoning_content: None,
  }];
  out.extend(required);
  out
}

async fn run_generation(
  app: tauri::AppHandle,
  state: &AppState,
  conversation_id: String,
  user_text: String,
  save_user_message: bool,
  source_user_message_id: Option<String>,
) -> CmdResult<String> {
  if state.is_generating(&conversation_id) {
    return Err("current conversation already has a running generation task".to_string());
  }

  let settings = state.db.get_chat_settings();
  let (
    provider_override,
    model_override,
    base_url_override,
    temperature_override,
    max_tokens_override,
    _max_context_tokens_override,
    _max_recent_messages_override,
    _max_memory_items_override,
    _system_prompt_override,
    thinking_override,
    reasoning_effort_override,
  ) = state
    .db
    .get_conversation_chat_overrides(&conversation_id)
    .unwrap_or_default();
  let provider = if provider_override.trim().is_empty() {
    normalize_provider(&settings.provider)
  } else {
    normalize_provider(&provider_override)
  };
  let model = if model_override.trim().is_empty() {
    settings.model.trim().to_string()
  } else {
    model_override.trim().to_string()
  };
  let base_url_input = if base_url_override.trim().is_empty() {
    settings.base_url.as_str()
  } else {
    base_url_override.as_str()
  };
  let base_url = resolve_base_url(&provider, base_url_input).ok_or_else(|| {
    format!("provider '{}' requires base_url; please set it in Settings", provider)
  })?;
  let temperature = temperature_override
    .unwrap_or(settings.temperature)
    .clamp(0.0, 2.0);
  let max_tokens = max_tokens_override
    .unwrap_or(settings.max_tokens)
    .clamp(256, 262_144);
  let (deepseek_thinking_type, deepseek_reasoning_effort, include_temperature) =
    if provider == "deepseek" && supports_deepseek_thinking_params(&model) {
    let normalized_thinking = {
      let v = normalize_thinking_override(&thinking_override);
      if v.is_empty() {
        "enabled".to_string()
      } else {
        v
      }
    };
    let normalized_effort = {
      let v = normalize_reasoning_effort(&reasoning_effort_override);
      if v.is_empty() {
        "high".to_string()
      } else {
        v
      }
    };
    let include_temp = normalized_thinking != "enabled";
      let effort_to_send = if normalized_thinking == "enabled" {
        Some(normalized_effort)
      } else {
        None
      };
      (Some(normalized_thinking), effort_to_send, include_temp)
    } else {
      (None, None, true)
    };
  let api_key = load_api_key(&state.db, &provider).unwrap_or_default();
  if provider_requires_api_key(&provider) && api_key.trim().is_empty() {
    return Err(format!(
      "missing {} API key, please save it in Settings first",
      provider.to_uppercase()
    ));
  }

  let request_id = Uuid::new_v4().to_string();
  let cancel_flag = state.register_generation(&conversation_id);

  let user_message_id = if save_user_message {
    match state
      .db
      .add_message(
        &conversation_id,
        "user",
        &user_text,
        "",
        &TokenUsage::default(),
      ) {
      Ok(id) => id,
      Err(e) => {
        state.clear_generation(&conversation_id);
        return Err(err(e));
      }
    }
  } else {
    source_user_message_id.unwrap_or_default()
  };

  let _ = memory::update_profiles_from_user_text(&state.db, &user_text);
  let _ = memory::maybe_extract_memories(
    &state.db,
    Some(&conversation_id),
    Some(&user_message_id),
    &user_text,
  );

  let context_messages = build_context_messages(state, &conversation_id, &user_text);
  let config = ApiRuntimeConfig {
    api_key,
    base_url,
    model,
    temperature,
    max_tokens,
    include_temperature,
    deepseek_thinking_type,
    deepseek_reasoning_effort,
  };

  let app_for_content = app.clone();
  let rid_for_content = request_id.clone();
  let cid_for_content = conversation_id.clone();
  let app_for_reasoning = app.clone();
  let rid_for_reasoning = request_id.clone();
  let cid_for_reasoning = conversation_id.clone();

  let stream_result = stream_chat_completion(
    config,
    context_messages,
    StreamCallbacks {
      on_content: move |token| {
        let _ = app_for_content.emit(
          "chat:token",
          StreamTokenPayload {
            request_id: rid_for_content.clone(),
            conversation_id: cid_for_content.clone(),
            token: token.to_string(),
          },
        );
      },
      on_reasoning: move |token| {
        let _ = app_for_reasoning.emit(
          "chat:reasoning",
          StreamTokenPayload {
            request_id: rid_for_reasoning.clone(),
            conversation_id: cid_for_reasoning.clone(),
            token: token.to_string(),
          },
        );
      },
    },
    cancel_flag.clone(),
  )
  .await;

  let final_result = match stream_result {
    Ok((mut content, reasoning_content, usage)) => {
      if cancel_flag.load(Ordering::Relaxed) {
        if content.trim().is_empty() {
          content = "Generation stopped.".to_string();
        } else {
          content.push_str("\n\n[Generation stopped]");
        }
      }

      let message_id = state
        .db
        .add_message(
          &conversation_id,
          "assistant",
          &content,
          &reasoning_content,
          &usage,
        )
        .map_err(err)?;

      let _ = state
        .db
        .maybe_auto_title(&conversation_id, &normalize_title(&user_text));
      let _ = memory::maybe_update_summary(&state.db, &conversation_id);
      let _ = state
        .db
        .set_setting("last_conversation_id", &conversation_id)
        .map_err(err);
      let _ = sync_settings_file(state);

      let _ = app.emit(
        "chat:done",
        StreamDonePayload {
          request_id: request_id.clone(),
          conversation_id: conversation_id.clone(),
          user_message_id: user_message_id.clone(),
          message_id,
          content,
          reasoning_content,
        },
      );
      Ok(request_id)
    }
    Err(e) => {
      let _ = app.emit(
        "chat:error",
        StreamErrorPayload {
          request_id: request_id.clone(),
          conversation_id: conversation_id.clone(),
          error: e.to_string(),
        },
      );
      Err(e.to_string())
    }
  };

  state.clear_generation(&conversation_id);
  final_result
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> CmdResult<AppSettingsPayload> {
  Ok(AppSettingsPayload {
    chat: state.db.get_chat_settings(),
    last_conversation_id: state.db.get_setting("last_conversation_id", ""),
    theme_mode: normalize_theme_mode(&state.db.get_setting("theme_mode", "system")).to_string(),
    models: state.db.get_models(),
  })
}

#[tauri::command]
fn set_settings(state: State<'_, AppState>, payload: AppSettingsPayload) -> CmdResult<()> {
  state.db.set_chat_settings(&payload.chat).map_err(err)?;
  state.db.set_models(&payload.models).map_err(err)?;
  state
    .db
    .set_setting("last_conversation_id", &payload.last_conversation_id)
    .map_err(err)?;
  state
    .db
    .set_setting("theme_mode", normalize_theme_mode(&payload.theme_mode))
    .map_err(err)?;
  sync_settings_file(&state)
}

#[tauri::command]
fn save_api_key(state: State<'_, AppState>, payload: SaveApiKeyInput) -> CmdResult<()> {
  let provider = normalize_provider(&payload.provider);
  let api_key = payload.api_key.trim();
  if api_key.is_empty() {
    return Err("api_key cannot be empty".to_string());
  }
  state
    .db
    .set_setting(&cfg_key(&provider), api_key)
    .map_err(err)?;
  state.db.set_setting("provider", &provider).map_err(err)?;
  sync_settings_file(&state)
}

#[tauri::command]
fn has_api_key(state: State<'_, AppState>, provider: String) -> CmdResult<bool> {
  let provider = normalize_provider(&provider);
  Ok(!state
    .db
    .get_setting(&cfg_key(&provider), "")
    .trim()
    .is_empty())
}

#[tauri::command]
fn list_conversations(state: State<'_, AppState>, input: Option<ListInput>) -> CmdResult<Vec<Conversation>> {
  let query = input.as_ref().and_then(|x| x.query.clone()).unwrap_or_default();
  let limit = input.as_ref().and_then(|x| x.limit).unwrap_or(300).clamp(20, 1000);
  state.db.list_conversations(&query, limit).map_err(err)
}

#[tauri::command]
fn create_conversation(
  state: State<'_, AppState>,
  input: Option<CreateConversationInput>,
) -> CmdResult<Conversation> {
  let conv = state
    .db
    .create_conversation(input.as_ref().and_then(|x| x.title.as_deref()))
    .map_err(err)?;
  state
    .db
    .set_setting("last_conversation_id", &conv.id)
    .map_err(err)?;
  let _ = sync_settings_file(&state);
  Ok(conv)
}

#[tauri::command]
fn rename_conversation(state: State<'_, AppState>, input: RenameConversationInput) -> CmdResult<()> {
  state
    .db
    .rename_conversation(&input.conversation_id, &input.title)
    .map_err(err)
}

#[tauri::command]
fn delete_conversation(state: State<'_, AppState>, input: ConversationIdInput) -> CmdResult<()> {
  let conversation_id = input.conversation_id.trim().to_string();
  if conversation_id.is_empty() {
    return Ok(());
  }
  let files = state
    .db
    .list_conversation_files_for_context(&conversation_id, 4000)
    .unwrap_or_default();
  state.db.delete_conversation(&conversation_id).map_err(err)?;
  for file in files {
    if let Some(path) = resolve_stored_txt_path(&state, &file.file_path) {
      let _ = fs::remove_file(path);
    }
  }
  Ok(())
}

#[tauri::command]
fn set_conversation_pinned(state: State<'_, AppState>, input: SetConversationPinnedInput) -> CmdResult<()> {
  state
    .db
    .set_conversation_pinned(&input.conversation_id, input.pinned)
    .map_err(err)
}

#[tauri::command]
fn move_conversation(state: State<'_, AppState>, input: MoveConversationInput) -> CmdResult<()> {
  let direction = input.direction.trim().to_lowercase();
  let dir = if direction == "down" { "down" } else { "up" };
  state
    .db
    .move_conversation(&input.conversation_id, dir)
    .map_err(err)
}

#[tauri::command]
fn set_conversation_model(state: State<'_, AppState>, input: SetConversationModelInput) -> CmdResult<()> {
  state
    .db
    .set_conversation_model(&input.conversation_id, &input.model)
    .map_err(err)
}

#[tauri::command]
fn set_conversation_chat_settings(
  state: State<'_, AppState>,
  input: SetConversationChatSettingsInput,
) -> CmdResult<()> {
  let provider = if input.provider.trim().is_empty() {
    String::new()
  } else {
    normalize_provider(&input.provider)
  };
  let temperature = input.temperature_override.map(|v| v.clamp(0.0, 2.0));
  let max_tokens = input.max_tokens_override.map(|v| v.clamp(256, 262_144));
  let max_context_tokens = input
    .max_context_tokens_override
    .map(|v| v.clamp(2048, 128_000));
  let max_recent_messages = input
    .max_recent_messages_override
    .map(|v| v.clamp(4, 48));
  let max_memory_items = input
    .max_memory_items_override
    .map(|v| v.clamp(0, 16));
  let system_prompt = input.system_prompt.as_deref().map(str::trim);
  let thinking_override = normalize_thinking_override(input.thinking_override.as_deref().unwrap_or(""));
  let effort_override = normalize_reasoning_effort(input.reasoning_effort_override.as_deref().unwrap_or(""));
  if input
    .thinking_override
    .as_deref()
    .map(str::trim)
    .map(|v| !v.is_empty() && thinking_override.is_empty())
    .unwrap_or(false)
  {
    return Err("Invalid thinking_override, use enabled/disabled or empty.".to_string());
  }
  if input
    .reasoning_effort_override
    .as_deref()
    .map(str::trim)
    .map(|v| !v.is_empty() && effort_override.is_empty())
    .unwrap_or(false)
  {
    return Err("Invalid reasoning_effort_override, use high/max or empty.".to_string());
  }
  state
    .db
    .set_conversation_chat_settings(
      &input.conversation_id,
      &provider,
      &input.model,
      &input.base_url,
      temperature,
      max_tokens,
      max_context_tokens,
      max_recent_messages,
      max_memory_items,
      system_prompt,
      Some(&thinking_override),
      Some(&effort_override),
    )
    .map_err(err)
}

#[tauri::command]
fn list_messages(state: State<'_, AppState>, input: ListMessagesInput) -> CmdResult<Vec<Message>> {
  state
    .db
    .list_messages(
      &input.conversation_id,
      input.limit.unwrap_or(200).clamp(20, 2000),
      input.offset.unwrap_or(0).max(0),
    )
    .map_err(err)
}

#[tauri::command]
fn upload_txt_file(state: State<'_, AppState>, input: UploadTxtFileInput) -> CmdResult<ConversationFile> {
  let conversation_id = input.conversation_id.trim().to_string();
  if conversation_id.is_empty() {
    return Err("conversation_id cannot be empty".to_string());
  }
  let file_name = input.file_name.trim();
  if !is_txt_file_name(file_name) {
    return Err("only .txt files are supported".to_string());
  }
  if input.file_size <= 0 {
    return Err("empty file is not allowed".to_string());
  }
  if input.file_size > MAX_TXT_FILE_BYTES {
    return Err("txt file exceeds 1MB limit".to_string());
  }

  let content_text = input.content_text.replace('\u{0000}', "");
  if content_text.trim().is_empty() {
    return Err("empty file is not allowed".to_string());
  }

  // Extra guard for malicious payload expansion.
  let content_bytes = content_text.as_bytes().len() as i64;
  if content_bytes > MAX_TXT_FILE_BYTES {
    return Err("txt content exceeds 1MB limit after decoding".to_string());
  }

  let safe_name = sanitize_txt_file_name(file_name);
  let disk_name = format!("{}_{}", Uuid::new_v4().as_simple(), safe_name);
  let abs_path = state.files_dir.join(&disk_name);
  fs::write(&abs_path, content_text.as_bytes()).map_err(err)?;

  let rel_path = format!("data/files/{disk_name}");
  let summary = summarize_txt_content(&content_text);
  let mut row = state
    .db
    .add_conversation_file(
      &conversation_id,
      file_name,
      &rel_path,
      input.file_size,
      &content_text,
      &summary,
    )
    .map_err(err)?;
  row.content_text.clear();
  Ok(row)
}

#[tauri::command]
fn list_conversation_files(
  state: State<'_, AppState>,
  input: ListConversationFilesInput,
) -> CmdResult<Vec<ConversationFile>> {
  let conversation_id = input.conversation_id.trim();
  if conversation_id.is_empty() {
    return Ok(Vec::new());
  }
  state
    .db
    .list_conversation_files(conversation_id, input.limit.unwrap_or(200).clamp(1, 2000))
    .map_err(err)
}

#[tauri::command]
fn delete_conversation_file(state: State<'_, AppState>, input: DeleteConversationFileInput) -> CmdResult<()> {
  let row = state
    .db
    .soft_delete_conversation_file(input.file_id.trim())
    .map_err(err)?;
  if let Some(file) = row {
    if let Some(path) = resolve_stored_txt_path(&state, &file.file_path) {
      let _ = fs::remove_file(path);
    }
  }
  Ok(())
}

#[tauri::command]
fn delete_message(state: State<'_, AppState>, input: DeleteMessageInput) -> CmdResult<()> {
  let soft = input.soft.unwrap_or(true);
  let reason = input.reason.unwrap_or_else(|| "user_request".to_string());
  if let Some(conversation_id) = state
    .db
    .delete_message(&input.message_id, soft, &reason)
    .map_err(err)?
  {
    let _ = state.db.set_setting("last_conversation_id", &conversation_id);
    let _ = sync_settings_file(&state);
  }
  Ok(())
}

#[tauri::command]
async fn send_message(
  app: tauri::AppHandle,
  state: State<'_, AppState>,
  input: SendMessageInput,
) -> CmdResult<String> {
  let conversation_id = input.conversation_id.trim().to_string();
  let content = input.content.trim().to_string();
  if conversation_id.is_empty() {
    return Err("conversation_id cannot be empty".to_string());
  }
  if content.is_empty() {
    return Err("content cannot be empty".to_string());
  }
  run_generation(app, &state, conversation_id, content, true, None).await
}

#[tauri::command]
fn stop_generation(state: State<'_, AppState>, conversation_id: String) -> CmdResult<bool> {
  Ok(state.stop_generation(conversation_id.trim()))
}

#[tauri::command]
async fn edit_message(
  app: tauri::AppHandle,
  state: State<'_, AppState>,
  input: EditMessageInput,
) -> CmdResult<String> {
  let row = state
    .db
    .get_message(&input.message_id)
    .map_err(err)?
    .ok_or_else(|| "message not found".to_string())?;
  if row.role != "user" {
    return Err("only user message can be edited".to_string());
  }
  state
    .db
    .update_message_content(&input.message_id, input.new_content.trim())
    .map_err(err)?;
  let conv_id = state
    .db
    .truncate_messages_after(&input.message_id)
    .map_err(err)?
    .ok_or_else(|| "conversation not found".to_string())?;
  run_generation(
    app,
    &state,
    conv_id,
    input.new_content.trim().to_string(),
    false,
    Some(input.message_id),
  )
  .await
}

#[tauri::command]
async fn regenerate_from_user_message(
  app: tauri::AppHandle,
  state: State<'_, AppState>,
  input: RegenerateInput,
) -> CmdResult<String> {
  let row = state
    .db
    .get_message(&input.user_message_id)
    .map_err(err)?
    .ok_or_else(|| "user message not found".to_string())?;
  if row.role != "user" {
    return Err("target is not user message".to_string());
  }
  let conv_id = state
    .db
    .truncate_messages_after(&input.user_message_id)
    .map_err(err)?
    .ok_or_else(|| "conversation not found".to_string())?;
  run_generation(
    app,
    &state,
    conv_id,
    row.content,
    false,
    Some(input.user_message_id),
  )
  .await
}

#[tauri::command]
fn list_memories(state: State<'_, AppState>, input: Option<ListMemoriesInput>) -> CmdResult<Vec<Memory>> {
  let limit = input.as_ref().and_then(|x| x.limit).unwrap_or(300).clamp(20, 2000);
  let conversation_id = input
    .as_ref()
    .and_then(|x| x.conversation_id.clone())
    .unwrap_or_default();
  if conversation_id.trim().is_empty() {
    state.db.list_memories(limit).map_err(err)
  } else {
    state
      .db
      .list_memories_for_conversation(&conversation_id, limit)
      .map_err(err)
  }
}

#[tauri::command]
fn update_memory_scope(state: State<'_, AppState>, input: UpdateMemoryScopeInput) -> CmdResult<()> {
  state
    .db
    .set_memory_scope(
      &input.memory_id,
      input.make_global,
      input.conversation_id.as_deref(),
    )
    .map_err(err)
}

#[tauri::command]
fn import_conversations_json_cmd(
  state: State<'_, AppState>,
  input: ImportJsonInput,
) -> CmdResult<ImportResult> {
  let result = importer::import_conversations_json(&state.db, &input.file_path).map_err(err)?;
  sync_settings_file(&state)?;
  Ok(result)
}

#[tauri::command]
fn create_backup(state: State<'_, AppState>) -> CmdResult<BackupResult> {
  let file_name = format!("manual_{}.db", chrono::Utc::now().format("%Y%m%d_%H%M%S"));
  let path = state.backup_dir.join(file_name);
  state.db.backup_to(&path).map_err(err)?;
  Ok(BackupResult {
    path: path.to_string_lossy().to_string(),
  })
}

#[tauri::command]
fn restore_backup(state: State<'_, AppState>, input: ImportJsonInput) -> CmdResult<()> {
  let path = PathBuf::from(input.file_path);
  let safety_name = format!("pre_restore_{}.db", chrono::Utc::now().format("%Y%m%d_%H%M%S"));
  let safety_path = state.backup_dir.join(safety_name);
  state.db.backup_to(&safety_path).map_err(err)?;
  state.db.restore_from(&path).map_err(err)?;
  sync_settings_file(&state)
}

fn list_auto_backups(backup_dir: &Path) -> Vec<(PathBuf, SystemTime)> {
  let mut items: Vec<(PathBuf, SystemTime)> = Vec::new();
  let read_dir = match fs::read_dir(backup_dir) {
    Ok(v) => v,
    Err(_) => return items,
  };
  for entry in read_dir.flatten() {
    let path = entry.path();
    if !path.is_file() {
      continue;
    }
    let name = match path.file_name().and_then(|x| x.to_str()) {
      Some(v) => v,
      None => continue,
    };
    if !name.starts_with(AUTO_BACKUP_PREFIX) || !name.ends_with(".db") {
      continue;
    }
    let modified = entry
      .metadata()
      .ok()
      .and_then(|m| m.modified().ok())
      .unwrap_or(SystemTime::UNIX_EPOCH);
    items.push((path, modified));
  }
  items.sort_by(|a, b| b.1.cmp(&a.1));
  items
}

fn prune_auto_backups(backup_dir: &Path) {
  let items = list_auto_backups(backup_dir);
  for (idx, (path, _)) in items.iter().enumerate() {
    if idx < AUTO_BACKUP_RETENTION {
      continue;
    }
    let _ = fs::remove_file(path);
  }
}

fn maybe_run_auto_backup(db: &Arc<Database>, backup_dir: &Path, force: bool) {
  let latest = list_auto_backups(backup_dir).into_iter().next();
  let should_backup = if force {
    true
  } else {
    match latest {
      None => true,
      Some((_, ts)) => SystemTime::now()
        .duration_since(ts)
        .map(|d| d.as_secs() >= AUTO_BACKUP_RECENT_THRESHOLD_SECS)
        .unwrap_or(true),
    }
  };
  if !should_backup {
    return;
  }

  let file_name = format!(
    "{}{}.db",
    AUTO_BACKUP_PREFIX,
    chrono::Utc::now().format("%Y%m%d_%H%M%S")
  );
  let path = backup_dir.join(file_name);
  if db.backup_to(&path).is_ok() {
    prune_auto_backups(backup_dir);
  }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .setup(|app| {
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }

      let exe = std::env::current_exe().map_err(|e| e.to_string())?;
      let app_root = exe
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "cannot resolve app root".to_string())?;
      let data_dir = app_root.join("data");
      let files_dir = data_dir.join("files");
      let backup_dir = data_dir.join("backups");
      let export_dir = data_dir.join("exports");
      let logs_dir = data_dir.join("logs");
      let cache_dir = app_root.join("cache");
      let config_dir = app_root.join("config");
      let config_file = config_dir.join("settings.json");
      fs::create_dir_all(&data_dir)?;
      fs::create_dir_all(&files_dir)?;
      fs::create_dir_all(&backup_dir)?;
      fs::create_dir_all(&export_dir)?;
      fs::create_dir_all(&logs_dir)?;
      fs::create_dir_all(&cache_dir)?;
      fs::create_dir_all(&config_dir)?;

      let db = Database::open(data_dir.join("AIPartner.db")).map_err(err)?;
      db.init_schema(&backup_dir).map_err(err)?;
      let db = Arc::new(db);
      maybe_run_auto_backup(&db, &backup_dir, false);
      {
        let db_for_task = db.clone();
        let backup_dir_for_task = backup_dir.clone();
        thread::spawn(move || loop {
          thread::sleep(Duration::from_secs(AUTO_BACKUP_INTERVAL_HOURS * 3600));
          maybe_run_auto_backup(&db_for_task, &backup_dir_for_task, false);
        });
      }
      let state = AppState {
        app_root,
        files_dir,
        db,
        backup_dir,
        config_file,
        generation_flags: Mutex::new(HashMap::new()),
      };
      maybe_load_settings_file(&state)?;
      app.manage(state);
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      get_settings,
      set_settings,
      save_api_key,
      has_api_key,
      list_conversations,
      create_conversation,
      rename_conversation,
      delete_conversation,
      set_conversation_pinned,
      move_conversation,
      set_conversation_model,
      set_conversation_chat_settings,
      list_messages,
      upload_txt_file,
      list_conversation_files,
      delete_conversation_file,
      delete_message,
      send_message,
      stop_generation,
      edit_message,
      regenerate_from_user_message,
      list_memories,
      update_memory_scope,
      import_conversations_json_cmd,
      create_backup,
      restore_backup
    ])
    .run(tauri::generate_context!())
    .unwrap_or_else(|e| {
      eprintln!("error while running tauri application: {e}");
    });
}

