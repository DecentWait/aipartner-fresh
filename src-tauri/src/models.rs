use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
  pub prompt: i64,
  pub completion: i64,
  pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
  pub id: String,
  pub title: String,
  pub created_at: String,
  pub updated_at: String,
  #[serde(default)]
  pub is_pinned: bool,
  pub message_count: i64,
  #[serde(default)]
  pub model_override: String,
  #[serde(default)]
  pub provider_override: String,
  #[serde(default)]
  pub base_url_override: String,
  #[serde(default)]
  pub temperature_override: Option<f32>,
  #[serde(default)]
  pub max_tokens_override: Option<i64>,
  #[serde(default)]
  pub max_context_tokens_override: Option<i64>,
  #[serde(default)]
  pub max_recent_messages_override: Option<i64>,
  #[serde(default)]
  pub max_memory_items_override: Option<i64>,
  #[serde(default)]
  pub system_prompt: String,
  #[serde(default)]
  pub thinking_override: String,
  #[serde(default)]
  pub reasoning_effort_override: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
  pub id: String,
  pub conversation_id: String,
  pub role: String,
  pub content: String,
  pub reasoning_content: String,
  pub token_usage: TokenUsage,
  pub created_at: String,
  pub updated_at: String,
  #[serde(default)]
  pub deleted_at: Option<String>,
  #[serde(default)]
  pub deleted_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
  pub id: String,
  pub content: String,
  pub importance: i64,
  pub created_at: String,
  pub updated_at: String,
  pub last_used_at: String,
  #[serde(default)]
  pub scope: String,
  #[serde(default)]
  pub conversation_id: Option<String>,
  #[serde(default)]
  pub source_message_id: Option<String>,
  #[serde(default)]
  pub source_conversation_id: Option<String>,
  #[serde(default)]
  pub source_conversation_title: Option<String>,
  #[serde(default)]
  pub source_message_preview: Option<String>,
  #[serde(default)]
  pub source_conversation_deleted: bool,
  #[serde(default)]
  pub source_message_deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationFile {
  pub id: String,
  pub conversation_id: String,
  pub file_name: String,
  pub file_path: String,
  pub file_size: i64,
  pub content_text: String,
  pub summary: String,
  pub created_at: String,
  #[serde(default)]
  pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserProfile {
  pub preferred_name: String,
  pub long_term_goals: String,
  pub interests: String,
  pub important_experiences: String,
  pub language_preference: String,
  pub notes: String,
  pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StyleProfile {
  pub detail_level: String,
  pub tone: String,
  pub technical_level: String,
  pub language_style: String,
  pub explicit_preferences: String,
  pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SummaryRecord {
  pub conversation_id: String,
  pub segment_index: i64,
  pub summary: String,
  pub covered_start_message_id: String,
  pub covered_end_message_id: String,
  pub covered_start_created_at: String,
  pub covered_end_created_at: String,
  pub source_message_count: i64,
  pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChatSettings {
  pub provider: String,
  pub base_url: String,
  pub model: String,
  pub temperature: f32,
  pub max_tokens: i64,
  pub max_context_tokens: i64,
  pub max_recent_messages: i64,
  pub max_memory_items: i64,
  pub txt_max_file_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettingsPayload {
  pub chat: ChatSettings,
  pub last_conversation_id: String,
  pub theme_mode: String,
  pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamTokenPayload {
  pub request_id: String,
  pub conversation_id: String,
  pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamDonePayload {
  pub request_id: String,
  pub conversation_id: String,
  pub user_message_id: String,
  pub message_id: String,
  pub content: String,
  pub reasoning_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamErrorPayload {
  pub request_id: String,
  pub conversation_id: String,
  pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageInput {
  pub conversation_id: String,
  pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditMessageInput {
  pub message_id: String,
  pub new_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegenerateInput {
  pub user_message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteMessageInput {
  pub message_id: String,
  pub soft: Option<bool>,
  pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListInput {
  pub query: Option<String>,
  pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListMemoriesInput {
  pub conversation_id: Option<String>,
  pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMemoryScopeInput {
  pub memory_id: String,
  pub conversation_id: Option<String>,
  pub make_global: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetConversationModelInput {
  pub conversation_id: String,
  pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetConversationChatSettingsInput {
  pub conversation_id: String,
  pub provider: String,
  pub model: String,
  pub base_url: String,
  pub temperature_override: Option<f32>,
  pub max_tokens_override: Option<i64>,
  pub max_context_tokens_override: Option<i64>,
  pub max_recent_messages_override: Option<i64>,
  pub max_memory_items_override: Option<i64>,
  pub system_prompt: Option<String>,
  pub thinking_override: Option<String>,
  pub reasoning_effort_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationIdInput {
  pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameConversationInput {
  pub conversation_id: String,
  pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetConversationPinnedInput {
  pub conversation_id: String,
  pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveConversationInput {
  pub conversation_id: String,
  pub direction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateConversationInput {
  pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListMessagesInput {
  pub conversation_id: String,
  pub limit: Option<i64>,
  pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveApiKeyInput {
  pub provider: String,
  pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportJsonInput {
  pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
  pub imported_conversations: usize,
  pub imported_messages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupResult {
  pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadTxtFileInput {
  pub conversation_id: String,
  pub file_name: String,
  pub content_text: String,
  pub file_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListConversationFilesInput {
  pub conversation_id: String,
  pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteConversationFileInput {
  pub file_id: String,
}
