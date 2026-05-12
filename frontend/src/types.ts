export type Role = "user" | "assistant" | string;

export interface TokenUsage {
  prompt: number;
  completion: number;
  total: number;
}

export interface Conversation {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  is_pinned?: boolean;
  model_override?: string;
  provider_override?: string;
  base_url_override?: string;
  temperature_override?: number | null;
  max_tokens_override?: number | null;
  max_context_tokens_override?: number | null;
  max_recent_messages_override?: number | null;
  max_memory_items_override?: number | null;
  system_prompt?: string;
}

export interface Message {
  id: string;
  conversation_id: string;
  role: Role;
  content: string;
  reasoning_content: string;
  token_usage: TokenUsage;
  created_at: string;
  updated_at: string;
  deleted_at?: string | null;
  deleted_reason?: string;
}

export interface Memory {
  id: string;
  content: string;
  importance: number;
  created_at: string;
  updated_at: string;
  last_used_at: string;
  scope: string;
  conversation_id?: string | null;
  source_message_id?: string | null;
  source_conversation_id?: string | null;
  source_conversation_title?: string | null;
  source_message_preview?: string | null;
  source_conversation_deleted?: boolean;
  source_message_deleted?: boolean;
}

export interface ConversationFile {
  id: string;
  conversation_id: string;
  file_name: string;
  file_path: string;
  file_size: number;
  content_text: string;
  summary: string;
  created_at: string;
  deleted_at?: string | null;
}

export interface ChatSettings {
  provider: string;
  base_url: string;
  model: string;
  temperature: number;
  max_tokens: number;
  max_context_tokens: number;
  max_recent_messages: number;
  max_memory_items: number;
}

export interface AppSettingsPayload {
  chat: ChatSettings;
  last_conversation_id: string;
  theme_mode: "system" | "light" | "dark" | string;
  models: string[];
}

export interface StreamTokenPayload {
  request_id: string;
  conversation_id: string;
  token: string;
}

export interface StreamDonePayload {
  request_id: string;
  conversation_id: string;
  user_message_id: string;
  message_id: string;
  content: string;
  reasoning_content: string;
}

export interface StreamErrorPayload {
  request_id: string;
  conversation_id: string;
  error: string;
}

export interface ImportResult {
  imported_conversations: number;
  imported_messages: number;
}

export interface BackupResult {
  path: string;
}
