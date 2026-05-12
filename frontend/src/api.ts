import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AppSettingsPayload,
  BackupResult,
  Conversation,
  ConversationFile,
  ImportResult,
  Memory,
  Message,
  StreamDonePayload,
  StreamErrorPayload,
  StreamTokenPayload,
} from "./types";

type Unlisten = () => void;

export const commands = {
  getSettings: () => invoke<AppSettingsPayload>("get_settings"),
  setSettings: (payload: AppSettingsPayload) => invoke("set_settings", { payload }),
  saveApiKey: (provider: string, api_key: string) =>
    invoke("save_api_key", { payload: { provider, api_key } }),
  hasApiKey: (provider: string) => invoke<boolean>("has_api_key", { provider }),
  listConversations: (query = "", limit = 300) =>
    invoke<Conversation[]>("list_conversations", { input: { query, limit } }),
  createConversation: (title?: string) =>
    invoke<Conversation>("create_conversation", { input: { title } }),
  renameConversation: (conversation_id: string, title: string) =>
    invoke("rename_conversation", { input: { conversation_id, title } }),
  deleteConversation: (conversation_id: string) =>
    invoke("delete_conversation", { input: { conversation_id } }),
  setConversationPinned: (conversation_id: string, pinned: boolean) =>
    invoke("set_conversation_pinned", { input: { conversation_id, pinned } }),
  moveConversation: (conversation_id: string, direction: "up" | "down") =>
    invoke("move_conversation", { input: { conversation_id, direction } }),
  setConversationModel: (conversation_id: string, model: string) =>
    invoke("set_conversation_model", { input: { conversation_id, model } }),
  setConversationChatSettings: (
    conversation_id: string,
    provider: string,
    model: string,
    base_url: string,
    temperature_override: number | null,
    max_tokens_override: number | null,
    max_context_tokens_override: number | null,
    max_recent_messages_override: number | null,
    max_memory_items_override: number | null,
    system_prompt: string | null,
    thinking_override: string | null,
    reasoning_effort_override: string | null,
  ) =>
    invoke("set_conversation_chat_settings", {
      input: {
        conversation_id,
        provider,
        model,
        base_url,
        temperature_override,
        max_tokens_override,
        max_context_tokens_override,
        max_recent_messages_override,
        max_memory_items_override,
        system_prompt,
        thinking_override,
        reasoning_effort_override,
      },
    }),
  listMessages: (conversation_id: string, limit = 200, offset = 0) =>
    invoke<Message[]>("list_messages", { input: { conversation_id, limit, offset } }),
  uploadTxtFile: (conversation_id: string, file_name: string, content_text: string, file_size: number) =>
    invoke<ConversationFile>("upload_txt_file", {
      input: { conversation_id, file_name, content_text, file_size },
    }),
  listConversationFiles: (conversation_id: string, limit = 200) =>
    invoke<ConversationFile[]>("list_conversation_files", { input: { conversation_id, limit } }),
  deleteConversationFile: (file_id: string) =>
    invoke("delete_conversation_file", { input: { file_id } }),
  deleteMessage: (message_id: string, soft = true, reason = "user_request") =>
    invoke("delete_message", { input: { message_id, soft, reason } }),
  sendMessage: (conversation_id: string, content: string) =>
    invoke<string>("send_message", { input: { conversation_id, content } }),
  stopGeneration: (conversation_id: string) =>
    invoke<boolean>("stop_generation", { conversation_id }),
  editMessage: (message_id: string, new_content: string) =>
    invoke<string>("edit_message", { input: { message_id, new_content } }),
  regenerateFromUserMessage: (user_message_id: string) =>
    invoke<string>("regenerate_from_user_message", { input: { user_message_id } }),
  listMemories: (conversation_id?: string, limit = 300) =>
    invoke<Memory[]>("list_memories", { input: { conversation_id, limit } }),
  updateMemoryScope: (memory_id: string, conversation_id: string, make_global: boolean) =>
    invoke("update_memory_scope", { input: { memory_id, conversation_id, make_global } }),
  importConversationsJson: (file_path: string) =>
    invoke<ImportResult>("import_conversations_json_cmd", { input: { file_path } }),
  createBackup: () => invoke<BackupResult>("create_backup"),
  restoreBackup: (file_path: string) => invoke("restore_backup", { input: { file_path } }),
};

export const events = {
  onToken: (cb: (payload: StreamTokenPayload) => void): Promise<Unlisten> =>
    listen<StreamTokenPayload>("chat:token", (event) => cb(event.payload)),
  onReasoning: (cb: (payload: StreamTokenPayload) => void): Promise<Unlisten> =>
    listen<StreamTokenPayload>("chat:reasoning", (event) => cb(event.payload)),
  onDone: (cb: (payload: StreamDonePayload) => void): Promise<Unlisten> =>
    listen<StreamDonePayload>("chat:done", (event) => cb(event.payload)),
  onError: (cb: (payload: StreamErrorPayload) => void): Promise<Unlisten> =>
    listen<StreamErrorPayload>("chat:error", (event) => cb(event.payload)),
};
