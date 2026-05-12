import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import "katex/dist/katex.min.css";
import "highlight.js/styles/github-dark.css";
import "./App.css";
import { commands, events } from "./api";
import type {
  AppSettingsPayload,
  Conversation,
  ConversationFile,
  Memory,
  Message,
  StreamDonePayload,
  StreamErrorPayload,
  StreamTokenPayload,
} from "./types";

type Tab = "chat" | "memories" | "settings";
type PendingMode = "send" | "rewrite" | null;

type StreamView = {
  request_id: string;
  conversation_id: string;
  content: string;
  reasoning_content: string;
};

const REMARK_PLUGINS = [remarkMath, remarkGfm];
const REHYPE_PLUGINS = [rehypeKatex, rehypeHighlight];
const MESSAGE_PAGE_SIZE = 1000;
const STREAM_FLUSH_MS = 16;
const THEME_MEDIA_QUERY = "(prefers-color-scheme: dark)";
const PROVIDER_OPTIONS = ["deepseek", "openai", "openrouter", "ollama", "custom"] as const;
const DEFAULT_TXT_FILE_BYTES = 1_048_576;
const MIN_TXT_FILE_BYTES = 1_024;
const MAX_TXT_FILE_BYTES_HARD_CAP = 33_554_432;
type ConversationPreset = {
  provider: string;
  model: string;
  baseUrl: string;
  temperature: number | null;
  maxTokens: number | null;
  maxContextTokens: number | null;
  maxRecentMessages: number | null;
  maxMemoryItems: number | null;
  thinkingOverride: "enabled" | "disabled" | "";
  reasoningEffortOverride: "high" | "max" | "";
};
const CONVERSATION_PRESETS: Record<string, ConversationPreset> = {
  friend: {
    provider: "deepseek",
    model: "deepseek-v4-flash",
    baseUrl: "",
    temperature: 0.7,
    maxTokens: 2048,
    maxContextTokens: 12000,
    maxRecentMessages: 20,
    maxMemoryItems: 8,
    thinkingOverride: "disabled",
    reasoningEffortOverride: "",
  },
  pgee_sentence: {
    provider: "deepseek",
    model: "deepseek-v4-pro",
    baseUrl: "",
    temperature: 0.3,
    maxTokens: 2048,
    maxContextTokens: 12000,
    maxRecentMessages: 20,
    maxMemoryItems: 5,
    thinkingOverride: "enabled",
    reasoningEffortOverride: "high",
  },
  pgee_fullpaper: {
    provider: "deepseek",
    model: "deepseek-v4-pro",
    baseUrl: "",
    temperature: 0.2,
    maxTokens: 4096,
    maxContextTokens: 24000,
    maxRecentMessages: 20,
    maxMemoryItems: 3,
    thinkingOverride: "enabled",
    reasoningEffortOverride: "max",
  },
  coding: {
    provider: "deepseek",
    model: "deepseek-v4-pro",
    baseUrl: "",
    temperature: 0.2,
    maxTokens: 4096,
    maxContextTokens: 24000,
    maxRecentMessages: 25,
    maxMemoryItems: 6,
    thinkingOverride: "enabled",
    reasoningEffortOverride: "high",
  },
  quickqa: {
    provider: "deepseek",
    model: "deepseek-v4-flash",
    baseUrl: "",
    temperature: 0.5,
    maxTokens: 1024,
    maxContextTokens: 8192,
    maxRecentMessages: 10,
    maxMemoryItems: 4,
    thinkingOverride: "disabled",
    reasoningEffortOverride: "",
  },
};
const EMPTY_STREAM: StreamView = {
  request_id: "",
  conversation_id: "",
  content: "",
  reasoning_content: "",
};

function normalizeProvider(provider?: string): string {
  const p = (provider || "").trim().toLowerCase();
  if (!p) return "deepseek";
  return p;
}

function defaultBaseUrlForProvider(provider?: string): string {
  switch (normalizeProvider(provider)) {
    case "openai":
      return "https://api.openai.com/v1";
    case "openrouter":
      return "https://openrouter.ai/api/v1";
    case "ollama":
      return "http://127.0.0.1:11434/v1";
    case "custom":
      return "";
    default:
      return "https://api.deepseek.com";
  }
}

function providerNeedsApiKey(provider?: string): boolean {
  const p = normalizeProvider(provider);
  return p === "deepseek" || p === "openai" || p === "openrouter";
}

function normalizeThemeMode(mode?: string): "system" | "light" | "dark" {
  if (mode === "light" || mode === "dark") return mode;
  return "system";
}

function applyThemeMode(mode: string) {
  const next = normalizeThemeMode(mode);
  const root = document.documentElement;
  if (next === "system") {
    const prefersDark = window.matchMedia(THEME_MEDIA_QUERY).matches;
    root.dataset.theme = prefersDark ? "dark" : "light";
    return;
  }
  root.dataset.theme = next;
}

function fmtTime(ts: string) {
  if (!ts) return "";
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleString();
}

function fmtFileSize(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(2)} MB`;
}

function normalizeTxtLimitBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return DEFAULT_TXT_FILE_BYTES;
  return Math.min(MAX_TXT_FILE_BYTES_HARD_CAP, Math.max(MIN_TXT_FILE_BYTES, Math.round(bytes)));
}

function clipText(input: string, max = 120) {
  if (!input) return "";
  const clean = input.replace(/\s+/g, " ").trim();
  if (clean.length <= max) return clean;
  return `${clean.slice(0, max)}...`;
}

function normalizeMath(raw: string) {
  if (!raw) return "";
  return raw
    .replace(/\\\[(.*?)\\\]/gs, (_m, expr: string) => `$$${expr}$$`)
    .replace(/\\\((.*?)\\\)/gs, (_m, expr: string) => `$${expr}$`);
}

const Md = memo(function Md({ content }: { content: string }) {
  const normalized = useMemo(() => normalizeMath(content), [content]);
  return (
    <ReactMarkdown remarkPlugins={REMARK_PLUGINS} rehypePlugins={REHYPE_PLUGINS}>
      {normalized}
    </ReactMarkdown>
  );
});

const MessageCard = memo(function MessageCard({
  message,
  onEdit,
  onRegenerate,
  onDelete,
}: {
  message: Message;
  onEdit: (msg: Message) => void;
  onRegenerate: (msg: Message) => void;
  onDelete: (msg: Message) => void;
}) {
  return (
    <div className={`message ${message.role === "user" ? "user" : "assistant"}`}>
      <div className="message-meta">
        {message.role} | {fmtTime(message.created_at)}
      </div>
      <div className="message-body">
        <Md content={message.content} />
      </div>
      {message.reasoning_content ? (
        <details className="reasoning" open>
          <summary>思考过程</summary>
          <Md content={message.reasoning_content} />
        </details>
      ) : null}
      <div className="message-actions">
        <button className="btn-mini" onClick={() => navigator.clipboard.writeText(message.content || "")}>
          Copy
        </button>
        {!message.id.startsWith("local-") ? (
          <button className="btn-mini danger" onClick={() => onDelete(message)}>
            Delete
          </button>
        ) : null}
        {message.role === "user" && !message.id.startsWith("local-") ? (
          <>
            <button className="btn-mini" onClick={() => onEdit(message)}>
              Edit
            </button>
            <button className="btn-mini" onClick={() => onRegenerate(message)}>
              Regenerate
            </button>
          </>
        ) : null}
      </div>
    </div>
  );
});

const Composer = memo(function Composer({
  sending,
  onSubmit,
}: {
  sending: boolean;
  onSubmit: (text: string) => void;
}) {
  const [draft, setDraft] = useState("");

  const submit = useCallback(() => {
    if (sending) return;
    const text = draft.trim();
    if (!text) return;
    onSubmit(text);
    setDraft("");
  }, [draft, sending, onSubmit]);

  return (
    <footer className="composer">
      <textarea
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        placeholder="Type a message. Enter to send, Shift+Enter for newline."
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            submit();
          }
        }}
      />
      <div className="composer-actions">
        <button className="btn btn-primary" onClick={submit} disabled={sending}>
          {sending ? "Sending..." : "Send"}
        </button>
      </div>
    </footer>
  );
});

export default function App() {
  const [tab, setTab] = useState<Tab>("chat");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [headerInfoCollapsed, setHeaderInfoCollapsed] = useState(false);
  const [settings, setSettings] = useState<AppSettingsPayload | null>(null);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [query, setQuery] = useState("");
  const [currentId, setCurrentId] = useState("");
  const [messagesByConv, setMessagesByConv] = useState<Record<string, Message[]>>({});
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");
  const [apiKeyInput, setApiKeyInput] = useState("");
  const [importPath, setImportPath] = useState("C:\\Users\\waitd\\Desktop\\conversations.json");
  const [restorePath, setRestorePath] = useState("");
  const [memories, setMemories] = useState<Memory[]>([]);
  const [conversationFiles, setConversationFiles] = useState<ConversationFile[]>([]);
  const [uploadingTxt, setUploadingTxt] = useState(false);
  const [hasKey, setHasKey] = useState<Record<string, boolean>>({});
  const [streamView, setStreamView] = useState<StreamView>(EMPTY_STREAM);

  const messageEndRef = useRef<HTMLDivElement | null>(null);
  const txtInputRef = useRef<HTMLInputElement | null>(null);
  const currentIdRef = useRef("");
  const queryRef = useRef("");
  const pendingModeRef = useRef<PendingMode>(null);
  const staleConversationsRef = useRef<Record<string, boolean>>({});
  const messagesByConvRef = useRef<Record<string, Message[]>>({});
  const streamBufferRef = useRef<StreamView>({ ...EMPTY_STREAM });
  const flushTimerRef = useRef<number | null>(null);

  const currentMessages = useMemo(() => messagesByConv[currentId] || [], [messagesByConv, currentId]);

  const currentConversation = useMemo(
    () => conversations.find((x) => x.id === currentId) || null,
    [conversations, currentId],
  );
  const activeModelLabel = useMemo(() => {
    const fixed = (currentConversation?.model_override || "").trim();
    if (fixed) return `${fixed} (fixed)`;
    const fallback = settings?.chat.model?.trim() || "-";
    return `${fallback} (global)`;
  }, [currentConversation?.model_override, settings?.chat.model]);
  const activeProviderLabel = useMemo(() => {
    const fixed = (currentConversation?.provider_override || "").trim();
    if (fixed) return `${normalizeProvider(fixed)} (fixed)`;
    const fallback = normalizeProvider(settings?.chat.provider || "");
    return `${fallback} (global)`;
  }, [currentConversation?.provider_override, settings?.chat.provider]);
  const hasConversationSettings = useMemo(() => {
    const c = currentConversation;
    if (!c) return false;
    return (
      (c.provider_override || "").trim() !== "" ||
      (c.model_override || "").trim() !== "" ||
      (c.base_url_override || "").trim() !== "" ||
      (c.system_prompt || "").trim() !== "" ||
      (c.thinking_override || "").trim() !== "" ||
      (c.reasoning_effort_override || "").trim() !== "" ||
      c.temperature_override != null ||
      c.max_tokens_override != null ||
      c.max_context_tokens_override != null ||
      c.max_recent_messages_override != null ||
      c.max_memory_items_override != null
    );
  }, [currentConversation]);
  const settingsModeLabel = hasConversationSettings ? "Using conversation settings" : "Using global settings";
  const activeContextTokensLabel = useMemo(() => {
    if (!settings) return "-";
    return String(currentConversation?.max_context_tokens_override ?? settings.chat.max_context_tokens);
  }, [currentConversation?.max_context_tokens_override, settings]);
  const activeRecentMessagesLabel = useMemo(() => {
    if (!settings) return "-";
    return String(currentConversation?.max_recent_messages_override ?? settings.chat.max_recent_messages);
  }, [currentConversation?.max_recent_messages_override, settings]);
  const activeMemoryItemsLabel = useMemo(() => {
    if (!settings) return "-";
    return String(currentConversation?.max_memory_items_override ?? settings.chat.max_memory_items);
  }, [currentConversation?.max_memory_items_override, settings]);
  const activeSystemPromptLabel = useMemo(() => {
    const txt = (currentConversation?.system_prompt || "").trim();
    if (!txt) return "none";
    return `set (${txt.length} chars)`;
  }, [currentConversation?.system_prompt]);
  const activeThinkingLabel = useMemo(() => {
    const provider = (currentConversation?.provider_override || settings?.chat.provider || "").trim().toLowerCase();
    if (provider !== "deepseek") return "n/a";
    const txt = (currentConversation?.thinking_override || "").trim().toLowerCase();
    return txt || "enabled (default)";
  }, [currentConversation?.provider_override, currentConversation?.thinking_override, settings?.chat.provider]);
  const activeReasoningEffortLabel = useMemo(() => {
    const provider = (currentConversation?.provider_override || settings?.chat.provider || "").trim().toLowerCase();
    if (provider !== "deepseek") return "n/a";
    const txt = (currentConversation?.reasoning_effort_override || "").trim().toLowerCase();
    return txt || "high (default)";
  }, [
    currentConversation?.provider_override,
    currentConversation?.reasoning_effort_override,
    settings?.chat.provider,
  ]);

  const stats = useMemo(() => {
    let tokenTotal = 0;
    let textChars = 0;
    for (const m of currentMessages) {
      tokenTotal += m.token_usage?.total || 0;
      textChars += (m.content || "").length + (m.reasoning_content || "").length;
    }
    return {
      loaded: currentMessages.length,
      total: currentConversation?.message_count ?? currentMessages.length,
      tokenTotal,
      textChars,
    };
  }, [currentMessages, currentConversation]);

  useEffect(() => {
    currentIdRef.current = currentId;
  }, [currentId]);

  useEffect(() => {
    queryRef.current = query;
  }, [query]);

  useEffect(() => {
    messagesByConvRef.current = messagesByConv;
  }, [messagesByConv]);

  const flushStreamNow = useCallback(() => {
    setStreamView({ ...streamBufferRef.current });
  }, []);

  const clearStream = useCallback(() => {
    if (flushTimerRef.current !== null) {
      window.clearTimeout(flushTimerRef.current);
      flushTimerRef.current = null;
    }
    streamBufferRef.current = { ...EMPTY_STREAM };
    setStreamView(EMPTY_STREAM);
  }, []);

  const scheduleStreamFlush = useCallback(() => {
    if (flushTimerRef.current !== null) return;
    flushTimerRef.current = window.setTimeout(() => {
      flushTimerRef.current = null;
      flushStreamNow();
    }, STREAM_FLUSH_MS);
  }, [flushStreamNow]);

  const refreshConversations = useCallback(async (q: string) => {
    const rows = await commands.listConversations(q, 500);
    setConversations(rows);
  }, []);

  const loadMessages = useCallback(async (conversationId: string, opts?: { force?: boolean }) => {
    if (!conversationId) return;
    const force = opts?.force ?? false;
    const hasCache = !!messagesByConvRef.current[conversationId];
    const stale = !!staleConversationsRef.current[conversationId];
    if (!force && hasCache && !stale) return;
    const chunks: Message[][] = [];
    let offset = 0;
    for (;;) {
      const batch = await commands.listMessages(conversationId, MESSAGE_PAGE_SIZE, offset);
      if (!batch.length) break;
      chunks.push(batch);
      if (batch.length < MESSAGE_PAGE_SIZE) break;
      offset += MESSAGE_PAGE_SIZE;
    }
    const rows: Message[] = [];
    for (let i = chunks.length - 1; i >= 0; i -= 1) {
      rows.push(...chunks[i]);
    }
    staleConversationsRef.current[conversationId] = false;
    setMessagesByConv((prev) => ({ ...prev, [conversationId]: rows }));
  }, []);

  const refreshMemories = useCallback(async (conversationId?: string) => {
    const cid = (conversationId ?? currentIdRef.current ?? "").trim();
    if (!cid) {
      setMemories([]);
      return;
    }
    const rows = await commands.listMemories(cid, 400);
    setMemories(rows);
  }, []);

  const refreshConversationFiles = useCallback(async (conversationId?: string) => {
    const cid = (conversationId ?? currentIdRef.current ?? "").trim();
    if (!cid) {
      setConversationFiles([]);
      return;
    }
    const rows = await commands.listConversationFiles(cid, 200);
    setConversationFiles(rows);
  }, []);

  const refreshHasKey = useCallback(async (provider = "deepseek") => {
    const p = normalizeProvider(provider);
    const pairs = await Promise.all(
      PROVIDER_OPTIONS.map(async (name) => [name, await commands.hasApiKey(name)] as const),
    );
    const map: Record<string, boolean> = Object.fromEntries(pairs);
    if (!(p in map)) {
      map[p] = await commands.hasApiKey(p);
    }
    setHasKey(map);
  }, []);

  const bootstrap = useCallback(async () => {
    const sRaw = await commands.getSettings();
    const s: AppSettingsPayload = {
      ...sRaw,
      theme_mode: normalizeThemeMode(sRaw.theme_mode),
    };
    setSettings(s);
    await refreshHasKey(s.chat.provider);
    await refreshConversations("");

    const fallback = await commands.listConversations("", 1);
    let selected = s.last_conversation_id || fallback[0]?.id || "";
    if (!selected) {
      const conv = await commands.createConversation("New Chat");
      selected = conv.id;
      await refreshConversations("");
    }
    setCurrentId(selected);
    await loadMessages(selected, { force: true });
    await refreshMemories(selected);
    await refreshConversationFiles(selected);
  }, [loadMessages, refreshConversations, refreshHasKey, refreshMemories, refreshConversationFiles]);

  useEffect(() => {
    void bootstrap().catch((e) => setError(String(e)));
  }, [bootstrap]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      void refreshConversations(query).catch((e) => setError(String(e)));
    }, 180);
    return () => window.clearTimeout(timer);
  }, [query, refreshConversations]);

  useEffect(() => {
    if (tab !== "memories") return;
    void refreshMemories(currentId).catch((e) => setError(String(e)));
  }, [currentId, refreshMemories, tab]);

  useEffect(() => {
    void refreshConversationFiles(currentId).catch((e) => setError(String(e)));
  }, [currentId, refreshConversationFiles]);

  useEffect(() => {
    const media = window.matchMedia(THEME_MEDIA_QUERY);
    applyThemeMode(settings?.theme_mode || "system");

    const onThemeChange = () => {
      if (normalizeThemeMode(settings?.theme_mode) === "system") {
        applyThemeMode("system");
      }
    };

    media.addEventListener("change", onThemeChange);
    return () => media.removeEventListener("change", onThemeChange);
  }, [settings?.theme_mode]);

  useEffect(() => {
    messageEndRef.current?.scrollIntoView({ behavior: sending ? "auto" : "smooth", block: "end" });
  }, [currentMessages, streamView.content, streamView.reasoning_content, currentId, sending]);

  useEffect(() => {
    let unlisten: Array<() => void> = [];
    (async () => {
      const u1 = await events.onToken((payload: StreamTokenPayload) => {
        if (payload.conversation_id !== currentIdRef.current) return;
        const buffer = streamBufferRef.current;
        buffer.request_id = payload.request_id;
        buffer.conversation_id = payload.conversation_id;
        buffer.content += payload.token;
        scheduleStreamFlush();
      });

      const u2 = await events.onReasoning((payload: StreamTokenPayload) => {
        if (payload.conversation_id !== currentIdRef.current) return;
        const buffer = streamBufferRef.current;
        buffer.request_id = payload.request_id;
        buffer.conversation_id = payload.conversation_id;
        buffer.reasoning_content += payload.token;
        scheduleStreamFlush();
      });

      const u3 = await events.onDone(async (payload: StreamDonePayload) => {
        if (flushTimerRef.current !== null) {
          window.clearTimeout(flushTimerRef.current);
          flushTimerRef.current = null;
        }
        flushStreamNow();

        const mode = pendingModeRef.current;
        pendingModeRef.current = null;
        setSending(false);

        if (payload.conversation_id === currentIdRef.current) {
          if (mode === "send") {
            setMessagesByConv((prev) => {
              const list = [...(prev[payload.conversation_id] || [])];
              if (payload.user_message_id) {
                for (let i = list.length - 1; i >= 0; i -= 1) {
                  if (list[i].role === "user" && list[i].id.startsWith("local-")) {
                    list[i] = { ...list[i], id: payload.user_message_id };
                    break;
                  }
                }
              }
              list.push({
                id: payload.message_id,
                conversation_id: payload.conversation_id,
                role: "assistant",
                content: payload.content,
                reasoning_content: payload.reasoning_content || "",
                token_usage: { prompt: 0, completion: 0, total: 0 },
                created_at: new Date().toISOString(),
                updated_at: new Date().toISOString(),
              });
              return { ...prev, [payload.conversation_id]: list };
            });
          } else {
            staleConversationsRef.current[payload.conversation_id] = true;
            await loadMessages(payload.conversation_id, { force: true });
          }
        } else {
          staleConversationsRef.current[payload.conversation_id] = true;
        }

        clearStream();
        await refreshConversations(queryRef.current);
        await refreshMemories();
      });

      const u4 = await events.onError((payload: StreamErrorPayload) => {
        pendingModeRef.current = null;
        setSending(false);
        setError(payload.error);
        clearStream();
      });

      unlisten = [u1, u2, u3, u4];
    })();

    return () => {
      if (flushTimerRef.current !== null) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = null;
      }
      for (const fn of unlisten) fn();
    };
  }, [clearStream, flushStreamNow, loadMessages, refreshConversations, refreshMemories, scheduleStreamFlush]);

  const onCreateConversation = useCallback(async () => {
    try {
      const conv = await commands.createConversation("New Chat");
      staleConversationsRef.current[conv.id] = true;
      await refreshConversations("");
      setCurrentId(conv.id);
      await loadMessages(conv.id, { force: true });
    } catch (e) {
      setError(String(e));
    }
  }, [loadMessages, refreshConversations]);

  const onSend = useCallback(async (text: string) => {
    if (!currentId || sending) return;
    pendingModeRef.current = "send";
    clearStream();
    setSending(true);
    setError("");
    const localId = `local-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

    setMessagesByConv((prev) => ({
      ...prev,
      [currentId]: [
        ...(prev[currentId] || []),
        {
          id: localId,
          conversation_id: currentId,
          role: "user",
          content: text,
          reasoning_content: "",
          token_usage: { prompt: 0, completion: 0, total: 0 },
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        },
      ],
    }));

    try {
      await commands.sendMessage(currentId, text);
    } catch (e) {
      pendingModeRef.current = null;
      setSending(false);
      setMessagesByConv((prev) => ({
        ...prev,
        [currentId]: (prev[currentId] || []).filter((m) => m.id !== localId),
      }));
      setError(String(e));
    }
  }, [clearStream, currentId, sending]);

  const onStop = useCallback(async () => {
    if (!currentId) return;
    await commands.stopGeneration(currentId);
  }, [currentId]);

  const onEdit = useCallback(async (msg: Message) => {
    const next = window.prompt("Edit message", msg.content || "");
    if (next === null) return;
    try {
      pendingModeRef.current = "rewrite";
      clearStream();
      setSending(true);
      await commands.editMessage(msg.id, next.trim());
    } catch (e) {
      pendingModeRef.current = null;
      setSending(false);
      setError(String(e));
    }
  }, [clearStream]);

  const onRegenerate = useCallback(async (msg: Message) => {
    if (!window.confirm("Regenerate from this user message?")) return;
    try {
      pendingModeRef.current = "rewrite";
      clearStream();
      setSending(true);
      await commands.regenerateFromUserMessage(msg.id);
    } catch (e) {
      pendingModeRef.current = null;
      setSending(false);
      setError(String(e));
    }
  }, [clearStream]);

  const onRenameConversation = useCallback(async (row: Conversation) => {
    const next = window.prompt("Rename conversation", row.title);
    if (next === null) return;
    await commands.renameConversation(row.id, next.trim());
    staleConversationsRef.current[row.id] = true;
    await refreshConversations(query);
  }, [query, refreshConversations]);

  const onDeleteConversation = useCallback(async (row: Conversation) => {
    if (!window.confirm("Delete this conversation?")) return;
    await commands.deleteConversation(row.id);
    delete staleConversationsRef.current[row.id];
    setMessagesByConv((prev) => {
      const next = { ...prev };
      delete next[row.id];
      return next;
    });

    await refreshConversations(query);
    if (currentId === row.id) {
      const next = (await commands.listConversations("", 1))[0];
      if (next) {
        setCurrentId(next.id);
        await loadMessages(next.id);
      } else {
        const conv = await commands.createConversation("New Chat");
        setCurrentId(conv.id);
        await refreshConversations("");
        await loadMessages(conv.id, { force: true });
      }
    }
  }, [currentId, loadMessages, query, refreshConversations]);

  const onToggleConversationPinned = useCallback(async (row: Conversation) => {
    try {
      await commands.setConversationPinned(row.id, !row.is_pinned);
      staleConversationsRef.current[row.id] = true;
      await refreshConversations(queryRef.current);
    } catch (e) {
      setError(String(e));
    }
  }, [refreshConversations]);

  const onMoveConversation = useCallback(async (row: Conversation, direction: "up" | "down") => {
    try {
      await commands.moveConversation(row.id, direction);
      staleConversationsRef.current[row.id] = true;
      await refreshConversations(queryRef.current);
    } catch (e) {
      setError(String(e));
    }
  }, [refreshConversations]);

  const onDeleteMessage = useCallback(async (msg: Message) => {
    if (!window.confirm("Delete this message?")) return;
    try {
      await commands.deleteMessage(msg.id, true, "user_request");
      staleConversationsRef.current[msg.conversation_id] = true;
      await loadMessages(msg.conversation_id, { force: true });
      await refreshConversations(queryRef.current);
    } catch (e) {
      setError(String(e));
    }
  }, [loadMessages, refreshConversations]);

  const onUploadTxt = useCallback(async (file: File | null) => {
    if (!file || !currentId) return;
    const txtLimitBytes = normalizeTxtLimitBytes(settings?.chat.txt_max_file_bytes ?? DEFAULT_TXT_FILE_BYTES);
    const lower = file.name.toLowerCase();
    if (!lower.endsWith(".txt")) {
      setError("Only .txt file upload is supported.");
      return;
    }
    if (file.size <= 0) {
      setError("Empty file is not allowed.");
      return;
    }
    if (file.size > txtLimitBytes) {
      setError(`TXT file exceeds limit (${fmtFileSize(txtLimitBytes)}).`);
      return;
    }

    setUploadingTxt(true);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      let text = "";
      let decodeWarn = "";
      try {
        text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
      } catch {
        text = new TextDecoder("utf-8").decode(bytes);
        decodeWarn = "non-UTF-8 text decoded in compatibility mode (may contain garbled chars).";
      }
      if (!text.trim()) {
        setError("Empty file is not allowed.");
        return;
      }
      await commands.uploadTxtFile(currentId, file.name, text, file.size);
      await refreshConversationFiles(currentId);
      setError(
        decodeWarn
          ? `Uploaded ${file.name}; ${decodeWarn}`
          : `Uploaded ${file.name}.`,
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setUploadingTxt(false);
      if (txtInputRef.current) {
        txtInputRef.current.value = "";
      }
    }
  }, [currentId, refreshConversationFiles, settings]);

  const onDeleteConversationFile = useCallback(async (file: ConversationFile) => {
    if (!window.confirm(`Delete attachment ${file.file_name}?`)) return;
    try {
      await commands.deleteConversationFile(file.id);
      await refreshConversationFiles(file.conversation_id);
      setError(`Deleted ${file.file_name}.`);
    } catch (e) {
      setError(String(e));
    }
  }, [refreshConversationFiles]);

  const onToggleMemoryScope = useCallback(async (memory: Memory) => {
    const cid = currentIdRef.current.trim();
    if (!cid) return;
    const isGlobal = (memory.scope || "").toLowerCase() === "global";
    try {
      await commands.updateMemoryScope(memory.id, cid, !isGlobal);
      await refreshMemories(cid);
      setError(isGlobal ? "Memory moved to current conversation." : "Memory marked as global.");
    } catch (e) {
      setError(String(e));
    }
  }, [refreshMemories]);

  const onSetConversationModel = useCallback(async () => {
    if (!currentId) return;
    const currentFixed = (currentConversation?.model_override || "").trim();
    const next = window.prompt(
      "Set fixed model for this conversation. Leave empty to use global model.",
      currentFixed,
    );
    if (next === null) return;
    try {
      await commands.setConversationModel(currentId, next.trim());
      staleConversationsRef.current[currentId] = true;
      await refreshConversations(queryRef.current);
      setError(next.trim() ? `Conversation model fixed: ${next.trim()}` : "Conversation now follows global model.");
    } catch (e) {
      setError(String(e));
    }
  }, [currentConversation?.model_override, currentId, refreshConversations]);

  const onApplyConversationPreset = useCallback(async () => {
    if (!currentId || !settings || !currentConversation) return;
    const keys = Object.keys(CONVERSATION_PRESETS);
    const key = window.prompt(
      `Preset key (${keys.join(", ")}). Empty to cancel.`,
      "friend",
    );
    if (key === null) return;
    const normalized = key.trim().toLowerCase();
    if (!normalized) return;
    const preset = CONVERSATION_PRESETS[normalized];
    if (!preset) {
      setError(`Unknown preset: ${normalized}`);
      return;
    }
    try {
      await commands.setConversationChatSettings(
        currentId,
        preset.provider,
        preset.model,
        preset.baseUrl,
        preset.temperature,
        preset.maxTokens,
        preset.maxContextTokens,
        preset.maxRecentMessages,
        preset.maxMemoryItems,
        (currentConversation.system_prompt || "").trim() || null,
        preset.thinkingOverride || null,
        preset.reasoningEffortOverride || null,
      );
      staleConversationsRef.current[currentId] = true;
      await refreshConversations(queryRef.current);
      await refreshHasKey(preset.provider || settings.chat.provider);
      setError(`Preset applied: ${normalized}`);
    } catch (e) {
      setError(String(e));
    }
  }, [currentConversation, currentId, refreshConversations, refreshHasKey, settings]);

  const onSetConversationChatSettings = useCallback(async () => {
    if (!currentId || !currentConversation || !settings) return;
    const p = window.prompt(
      "Provider override for this conversation (empty = follow global)",
      (currentConversation.provider_override || "").trim(),
    );
    if (p === null) return;
    const m = window.prompt(
      "Model override for this conversation (empty = follow global)",
      (currentConversation.model_override || "").trim(),
    );
    if (m === null) return;
    const b = window.prompt(
      "Base URL override for this conversation (empty = follow global)",
      (currentConversation.base_url_override || "").trim(),
    );
    if (b === null) return;
    const t = window.prompt(
      "Temperature override (empty = follow global)",
      currentConversation.temperature_override == null ? "" : String(currentConversation.temperature_override),
    );
    if (t === null) return;
    const k = window.prompt(
      "Max tokens override (empty = follow global)",
      currentConversation.max_tokens_override == null ? "" : String(currentConversation.max_tokens_override),
    );
    if (k === null) return;
    const c = window.prompt(
      "Max context tokens override (empty = follow global)",
      currentConversation.max_context_tokens_override == null
        ? ""
        : String(currentConversation.max_context_tokens_override),
    );
    if (c === null) return;
    const r = window.prompt(
      "Max recent messages override (empty = follow global)",
      currentConversation.max_recent_messages_override == null
        ? ""
        : String(currentConversation.max_recent_messages_override),
    );
    if (r === null) return;
    const i = window.prompt(
      "Max memory items override (empty = follow global)",
      currentConversation.max_memory_items_override == null
        ? ""
        : String(currentConversation.max_memory_items_override),
    );
    if (i === null) return;
    const s = window.prompt(
      "Conversation system prompt / fixed instruction (empty = disable for this conversation)",
      (currentConversation.system_prompt || "").trim(),
    );
    if (s === null) return;
    const th = window.prompt(
      "DeepSeek thinking override: enabled / disabled (empty = follow default)",
      (currentConversation.thinking_override || "").trim(),
    );
    if (th === null) return;
    const eff = window.prompt(
      "DeepSeek reasoning effort override: high / max (empty = follow default)",
      (currentConversation.reasoning_effort_override || "").trim(),
    );
    if (eff === null) return;

    const tempText = t.trim();
    const maxText = k.trim();
    const ctxText = c.trim();
    const recentText = r.trim();
    const memoryText = i.trim();
    const systemPromptText = s.trim();
    const thinkingText = th.trim().toLowerCase();
    const effortText = eff.trim().toLowerCase();
    let tempOverride: number | null = null;
    if (tempText) {
      const parsed = Number(tempText);
      if (!Number.isFinite(parsed)) {
        setError("Invalid temperature override.");
        return;
      }
      tempOverride = parsed;
    }
    let maxTokensOverride: number | null = null;
    if (maxText) {
      const parsed = Number(maxText);
      if (!Number.isFinite(parsed) || parsed <= 0) {
        setError("Invalid max_tokens override.");
        return;
      }
      maxTokensOverride = Math.floor(parsed);
    }
    let maxContextTokensOverride: number | null = null;
    if (ctxText) {
      const parsed = Number(ctxText);
      if (!Number.isFinite(parsed) || parsed <= 0) {
        setError("Invalid max_context_tokens override.");
        return;
      }
      maxContextTokensOverride = Math.floor(parsed);
    }
    let maxRecentMessagesOverride: number | null = null;
    if (recentText) {
      const parsed = Number(recentText);
      if (!Number.isFinite(parsed) || parsed <= 0) {
        setError("Invalid max_recent_messages override.");
        return;
      }
      maxRecentMessagesOverride = Math.floor(parsed);
    }
    let maxMemoryItemsOverride: number | null = null;
    if (memoryText) {
      const parsed = Number(memoryText);
      if (!Number.isFinite(parsed) || parsed < 0) {
        setError("Invalid max_memory_items override.");
        return;
      }
      maxMemoryItemsOverride = Math.floor(parsed);
    }
    if (thinkingText && thinkingText !== "enabled" && thinkingText !== "disabled") {
      setError("Invalid thinking override. Use enabled/disabled or empty.");
      return;
    }
    if (effortText && effortText !== "high" && effortText !== "max") {
      setError("Invalid reasoning effort override. Use high/max or empty.");
      return;
    }

    try {
      await commands.setConversationChatSettings(
        currentId,
        p.trim(),
        m.trim(),
        b.trim(),
        tempOverride,
        maxTokensOverride,
        maxContextTokensOverride,
        maxRecentMessagesOverride,
        maxMemoryItemsOverride,
        systemPromptText || null,
        thinkingText || null,
        effortText || null,
      );
      staleConversationsRef.current[currentId] = true;
      await refreshConversations(queryRef.current);
      await refreshHasKey(p.trim() || settings.chat.provider);
      setError("Conversation settings updated.");
    } catch (e) {
      setError(String(e));
    }
  }, [currentConversation, currentId, refreshConversations, refreshHasKey, settings]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Delete") return;
      const target = e.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName.toLowerCase();
        if (tag === "input" || tag === "textarea" || target.isContentEditable) return;
      }
      const active = conversations.find((x) => x.id === currentIdRef.current);
      if (!active) return;
      e.preventDefault();
      void onDeleteConversation(active);
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [conversations, onDeleteConversation]);

  const onSaveSettings = useCallback(async () => {
    if (!settings) return;
    const payload: AppSettingsPayload = {
      ...settings,
      theme_mode: normalizeThemeMode(settings.theme_mode),
    };
    await commands.setSettings(payload);
    setSettings(payload);
    await refreshConversations(query);
    await refreshHasKey(settings.chat.provider);
    setError("Settings saved.");
  }, [query, refreshConversations, refreshHasKey, settings]);

  const onSaveApiKey = useCallback(async () => {
    if (!settings) return;
    const key = apiKeyInput.trim();
    if (!key) return;
    await commands.saveApiKey(settings.chat.provider, key);
    setApiKeyInput("");
    await refreshHasKey(settings.chat.provider);
    setError("API key saved.");
  }, [apiKeyInput, refreshHasKey, settings]);

  const onImportJson = useCallback(async () => {
    const path = importPath.trim();
    if (!path) return;
    const result = await commands.importConversationsJson(path);
    setMessagesByConv({});
    staleConversationsRef.current = {};
    await refreshConversations(query);
    setError(`Imported conversations: ${result.imported_conversations}, messages: ${result.imported_messages}`);
  }, [importPath, query, refreshConversations]);

  const onBackup = useCallback(async () => {
    const result = await commands.createBackup();
    setError(`Backup created: ${result.path}`);
  }, []);

  const onRestore = useCallback(async () => {
    const path = restorePath.trim();
    if (!path) return;
    await commands.restoreBackup(path);
    setMessagesByConv({});
    staleConversationsRef.current = {};
    await bootstrap();
    setError("Backup restored.");
  }, [bootstrap, restorePath]);

  return (
    <div className={`app ${sidebarCollapsed ? "sidebar-collapsed" : ""}`}>
      <aside className={`sidebar ${sidebarCollapsed ? "collapsed" : ""}`}>
        <div className="brand">
          <h1>AIPartner</h1>
          <p>Local long-term AI companion</p>
        </div>

        <button className="btn btn-secondary" onClick={() => setSidebarCollapsed(true)}>
          Collapse Sidebar
        </button>

        <button className="btn btn-primary" onClick={() => void onCreateConversation()}>
          + New Chat
        </button>

        <input
          className="search"
          value={query}
          placeholder="Search conversation..."
          onChange={(e) => setQuery(e.target.value)}
        />

        <div className="conversation-list">
          {conversations.map((c) => (
            <div
              key={c.id}
              className={`conversation-item ${c.id === currentId ? "active" : ""}`}
              onClick={() => {
                setCurrentId(c.id);
                void loadMessages(c.id);
                if (tab === "memories") {
                  void refreshMemories(c.id);
                }
              }}
            >
              <div className="conversation-title">{c.title}</div>
              <div className="conversation-meta">{fmtTime(c.updated_at)}</div>
              <div className="conversation-meta">messages {c.message_count}</div>
              <div className="conversation-meta">{c.is_pinned ? "pinned" : "normal"}</div>
              <div className="conversation-meta">
                model {(c.model_override || "").trim() ? `fixed: ${c.model_override}` : "global"}
              </div>
              <div className="conversation-actions">
                <button
                  className="btn-mini"
                  onClick={(e) => {
                    e.stopPropagation();
                    void onRenameConversation(c);
                  }}
                >
                  Rename
                </button>
                <button
                  className="btn-mini"
                  onClick={(e) => {
                    e.stopPropagation();
                    void onToggleConversationPinned(c);
                  }}
                >
                  {c.is_pinned ? "Unpin" : "Pin"}
                </button>
                <button
                  className="btn-mini"
                  onClick={(e) => {
                    e.stopPropagation();
                    void onMoveConversation(c, "up");
                  }}
                >
                  Up
                </button>
                <button
                  className="btn-mini"
                  onClick={(e) => {
                    e.stopPropagation();
                    void onMoveConversation(c, "down");
                  }}
                >
                  Down
                </button>
                <button
                  className="btn-mini danger"
                  onClick={(e) => {
                    e.stopPropagation();
                    void onDeleteConversation(c);
                  }}
                >
                  Delete
                </button>
              </div>
            </div>
          ))}
        </div>

        <div className="sidebar-tabs">
          <button className={`btn-mini ${tab === "chat" ? "active" : ""}`} onClick={() => setTab("chat")}>
            Chat
          </button>
          <button
            className={`btn-mini ${tab === "memories" ? "active" : ""}`}
            onClick={() => {
              setTab("memories");
              void refreshMemories();
            }}
          >
            Memories
          </button>
          <button className={`btn-mini ${tab === "settings" ? "active" : ""}`} onClick={() => setTab("settings")}>
            Settings
          </button>
        </div>
      </aside>

      <main className="main">
        {tab !== "chat" ? (
          <div className="main-toolbar">
            <button className="btn btn-secondary" onClick={() => setSidebarCollapsed((v) => !v)}>
              {sidebarCollapsed ? "Show Sidebar" : "Hide Sidebar"}
            </button>
            <div className="main-toolbar-tabs">
              <button className="btn-mini" onClick={() => setTab("chat")}>
                Chat
              </button>
              <button
                className={`btn-mini ${tab === "memories" ? "active" : ""}`}
                onClick={() => {
                  setTab("memories");
                  void refreshMemories();
                }}
              >
                Memories
              </button>
              <button className={`btn-mini ${tab === "settings" ? "active" : ""}`} onClick={() => setTab("settings")}>
                Settings
              </button>
            </div>
          </div>
        ) : null}

        {tab === "chat" ? (
          <div className="chat-layout">
            <header className="header">
              <div>
                <h2>{currentConversation?.title || "Chat"}</h2>
                {!headerInfoCollapsed ? (
                  <>
                    <p>Loaded {stats.loaded}/{stats.total} messages | tokens {stats.tokenTotal} | chars {stats.textChars}</p>
                    <p>Provider: {activeProviderLabel}</p>
                    <p>Model: {activeModelLabel}</p>
                    <p>{settingsModeLabel}</p>
                    <p>
                      Context budget: max_context_tokens {activeContextTokensLabel} | max_recent_messages{" "}
                      {activeRecentMessagesLabel} | max_memory_items {activeMemoryItemsLabel}
                    </p>
                    <p>Conversation system prompt: {activeSystemPromptLabel}</p>
                    <p>DeepSeek thinking: {activeThinkingLabel} | reasoning_effort: {activeReasoningEffortLabel}</p>
                  </>
                ) : (
                  <p className="header-collapsed-tip">Info collapsed</p>
                )}
              </div>
              <div className="header-actions">
                <div className="header-toolbar-inline">
                  <button className="btn btn-secondary" onClick={() => setSidebarCollapsed((v) => !v)}>
                    {sidebarCollapsed ? "Show Sidebar" : "Hide Sidebar"}
                  </button>
                  <button className="btn-mini active" onClick={() => setTab("chat")}>
                    Chat
                  </button>
                  <button
                    className="btn-mini"
                    onClick={() => {
                      setTab("memories");
                      void refreshMemories();
                    }}
                  >
                    Memories
                  </button>
                  <button className="btn-mini" onClick={() => setTab("settings")}>
                    Settings
                  </button>
                  <button
                    className="btn btn-secondary"
                    onClick={() => setHeaderInfoCollapsed((v) => !v)}
                  >
                    {headerInfoCollapsed ? "Show Info" : "Hide Info"}
                  </button>
                </div>
                <button
                  className="btn btn-secondary"
                  onClick={() => void onApplyConversationPreset()}
                  disabled={!currentId}
                >
                  Apply Preset
                </button>
                <button
                  className="btn btn-secondary"
                  onClick={() => void onSetConversationChatSettings()}
                  disabled={!currentId}
                >
                  Set Chat Settings
                </button>
                <button className="btn btn-secondary" onClick={() => void onSetConversationModel()} disabled={!currentId}>
                  Set Model
                </button>
                <button className="btn btn-secondary" onClick={() => void onStop()} disabled={!sending}>
                  Stop
                </button>
              </div>
            </header>

            <section className="file-attachments">
              <div className="file-attachments-head">
                <div className="file-attachments-title">TXT Attachments (current conversation)</div>
                <div className="inline">
                  <input
                    ref={txtInputRef}
                    className="hidden-file-input"
                    type="file"
                    accept=".txt,text/plain"
                    onChange={(e) => void onUploadTxt(e.target.files?.[0] || null)}
                    disabled={!currentId || sending || uploadingTxt}
                  />
                  <button
                    className="btn btn-secondary"
                    onClick={() => txtInputRef.current?.click()}
                    disabled={!currentId || sending || uploadingTxt}
                  >
                    {uploadingTxt ? "Uploading..." : "Upload .txt"}
                  </button>
                </div>
              </div>
              <div className="file-attachments-list">
                {conversationFiles.length ? (
                  conversationFiles.map((f) => (
                    <div key={f.id} className="file-item">
                      <div className="file-item-main">
                        <div className="file-name">{f.file_name}</div>
                        <div className="file-meta">
                          {fmtFileSize(f.file_size)} | {fmtTime(f.created_at)}
                        </div>
                        {f.summary?.trim() ? <div className="file-summary">{f.summary}</div> : null}
                      </div>
                      <button
                        className="btn-mini danger"
                        onClick={() => void onDeleteConversationFile(f)}
                        disabled={sending || uploadingTxt}
                      >
                        Delete
                      </button>
                    </div>
                  ))
                ) : (
                  <div className="file-empty">No .txt attachment.</div>
                )}
              </div>
            </section>

            <section className="messages">
              {currentMessages.map((m) => (
                <MessageCard
                  key={m.id}
                  message={m}
                  onEdit={onEdit}
                  onRegenerate={onRegenerate}
                  onDelete={onDeleteMessage}
                />
              ))}

              {streamView.conversation_id === currentId && (streamView.content || streamView.reasoning_content) ? (
                <div className="message assistant">
                  <div className="message-meta">assistant | streaming | {streamView.request_id}</div>
                  <div className="message-body">
                    <Md content={streamView.content || "..."} />
                  </div>
                  {streamView.reasoning_content ? (
                    <details className="reasoning" open>
                      <summary>思考过程（实时）</summary>
                      <Md content={streamView.reasoning_content} />
                    </details>
                  ) : null}
                </div>
              ) : null}

              <div ref={messageEndRef} />
            </section>

            <Composer sending={sending} onSubmit={onSend} />
          </div>
        ) : null}

        {tab === "memories" ? (
          <section className="panel">
            <h2>Long-term Memories · {currentConversation?.title || "Current Conversation"}</h2>
            <p>Showing global memories + current conversation memories.</p>
            <button className="btn btn-secondary" onClick={() => void refreshMemories()}>
              Refresh
            </button>
            <div className="memory-list">
              {memories.map((m) => (
                <div key={m.id} className="memory-item">
                  <div className="memory-content">{m.content}</div>
                  <div className="memory-meta">
                    scope {(m.scope || "global").toLowerCase()} | importance {m.importance} | used{" "}
                    {fmtTime(m.last_used_at)}
                  </div>
                  <div className="memory-meta">created {fmtTime(m.created_at)}</div>
                  <div className="memory-meta">
                    source conversation{" "}
                    {(() => {
                      const sourceConvId = (m.source_conversation_id || "").trim();
                      if (!sourceConvId) return "旧版本记忆";
                      if (m.source_conversation_deleted) return "来源会话已删除";
                      const title = (m.source_conversation_title || "").trim();
                      if (title) return title;
                      return sourceConvId;
                    })()}
                  </div>
                  <div className="memory-meta">
                    source message{" "}
                    {(() => {
                      const sourceMsgId = (m.source_message_id || "").trim();
                      if (!sourceMsgId) return "Unknown";
                      if (m.source_message_deleted) return "来源消息已删除";
                      const preview = (m.source_message_preview || "").trim();
                      if (preview) return clipText(preview, 140);
                      return sourceMsgId;
                    })()}
                  </div>
                  <div className="memory-meta">
                    <button className="btn-mini" onClick={() => void onToggleMemoryScope(m)}>
                      {(m.scope || "").toLowerCase() === "global" ? "取消全局（转当前会话）" : "标记为全局"}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </section>
        ) : null}

        {tab === "settings" ? (
          <section className="panel">
            <h2>Settings</h2>
            {settings ? (
              <>
                <div className="form-grid">
                  <label>
                    Provider
                    <select
                      value={normalizeProvider(settings.chat.provider)}
                      onChange={(e) => {
                        const oldProvider = normalizeProvider(settings.chat.provider);
                        const nextProvider = normalizeProvider(e.target.value);
                        const currentBase = settings.chat.base_url.trim();
                        const oldDefault = defaultBaseUrlForProvider(oldProvider);
                        const nextDefault = defaultBaseUrlForProvider(nextProvider);
                        const nextBase =
                          currentBase === "" || currentBase === oldDefault ? nextDefault : settings.chat.base_url;
                        setSettings({
                          ...settings,
                          chat: { ...settings.chat, provider: nextProvider, base_url: nextBase },
                        });
                      }}
                    >
                      <option value="deepseek">DeepSeek</option>
                      <option value="openai">OpenAI</option>
                      <option value="openrouter">OpenRouter</option>
                      <option value="ollama">Ollama</option>
                      <option value="custom">Custom</option>
                    </select>
                  </label>
                  <label>
                    Theme
                    <select
                      value={normalizeThemeMode(settings.theme_mode)}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          theme_mode: normalizeThemeMode(e.target.value),
                        })
                      }
                    >
                      <option value="system">System</option>
                      <option value="light">Light</option>
                      <option value="dark">Dark</option>
                    </select>
                  </label>
                  <label>
                    Model
                    <input
                      value={settings.chat.model}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          chat: { ...settings.chat, model: e.target.value },
                        })
                      }
                    />
                  </label>
                  <label>
                    Base URL
                    <input
                      placeholder={defaultBaseUrlForProvider(settings.chat.provider) || "https://your-endpoint/v1"}
                      value={settings.chat.base_url}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          chat: { ...settings.chat, base_url: e.target.value },
                        })
                      }
                    />
                  </label>
                  <label>
                    Temperature
                    <input
                      type="number"
                      step="0.1"
                      min={0}
                      max={2}
                      value={settings.chat.temperature}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          chat: {
                            ...settings.chat,
                            temperature: Number(e.target.value || 0.7),
                          },
                        })
                      }
                    />
                  </label>
                  <label>
                    Max Tokens
                    <input
                      type="number"
                      min={256}
                      max={65536}
                      value={settings.chat.max_tokens}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          chat: { ...settings.chat, max_tokens: Number(e.target.value || 4096) },
                        })
                      }
                    />
                  </label>
                  <label>
                    Max Context Tokens
                    <input
                      type="number"
                      min={2048}
                      max={128000}
                      value={settings.chat.max_context_tokens}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          chat: {
                            ...settings.chat,
                            max_context_tokens: Number(e.target.value || 12000),
                          },
                        })
                      }
                    />
                  </label>
                  <label>
                    Max Recent Messages
                    <input
                      type="number"
                      min={4}
                      max={64}
                      value={settings.chat.max_recent_messages}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          chat: {
                            ...settings.chat,
                            max_recent_messages: Number(e.target.value || 20),
                          },
                        })
                      }
                    />
                  </label>
                  <label>
                    Max Memory Items
                    <input
                      type="number"
                      min={0}
                      max={32}
                      value={settings.chat.max_memory_items}
                      onChange={(e) =>
                        setSettings({
                          ...settings,
                          chat: {
                            ...settings.chat,
                            max_memory_items: Number(e.target.value || 8),
                          },
                        })
                      }
                    />
                  </label>
                  <label>
                    TXT Max File Size (KB)
                    <input
                      type="number"
                      min={1}
                      max={32768}
                      value={Math.max(1, Math.round(normalizeTxtLimitBytes(settings.chat.txt_max_file_bytes) / 1024))}
                      onChange={(e) => {
                        const kbRaw = Number(e.target.value || 1024);
                        const kb = Number.isFinite(kbRaw) ? Math.round(kbRaw) : 1024;
                        const clampedKb = Math.max(1, Math.min(32768, kb));
                        setSettings({
                          ...settings,
                          chat: {
                            ...settings.chat,
                            txt_max_file_bytes: clampedKb * 1024,
                          },
                        });
                      }}
                    />
                  </label>
                </div>

                <div className="setting-block">
                  <h3>API Key ({normalizeProvider(settings.chat.provider)})</h3>
                  <p>
                    saved: {hasKey[normalizeProvider(settings.chat.provider)] ? "yes" : "no"} (stored in
                    data/AIPartner.db and config/settings.json)
                    {providerNeedsApiKey(settings.chat.provider) ? "" : " | optional for this provider"}
                  </p>
                  <div className="inline">
                    <input
                      type="password"
                      placeholder="Paste API key"
                      value={apiKeyInput}
                      onChange={(e) => setApiKeyInput(e.target.value)}
                    />
                    <button className="btn btn-secondary" onClick={() => void onSaveApiKey()}>
                      Save Key
                    </button>
                  </div>
                </div>

                <div className="setting-block">
                  <h3>Import conversations.json</h3>
                  <div className="inline">
                    <input
                      placeholder="C:\\Users\\...\\conversations.json"
                      value={importPath}
                      onChange={(e) => setImportPath(e.target.value)}
                    />
                    <button className="btn btn-secondary" onClick={() => void onImportJson()}>
                      Import
                    </button>
                  </div>
                </div>

                <div className="setting-block">
                  <h3>Backup / Restore</h3>
                  <div className="inline">
                    <button className="btn btn-secondary" onClick={() => void onBackup()}>
                      Create Backup
                    </button>
                  </div>
                  <div className="inline">
                    <input
                      placeholder="C:\\...\\backup.db"
                      value={restorePath}
                      onChange={(e) => setRestorePath(e.target.value)}
                    />
                    <button className="btn btn-danger" onClick={() => void onRestore()}>
                      Restore
                    </button>
                  </div>
                </div>

                <div className="inline">
                  <button className="btn btn-primary" onClick={() => void onSaveSettings()}>
                    Save Settings
                  </button>
                </div>
              </>
            ) : (
              <div>Loading settings...</div>
            )}
          </section>
        ) : null}

        {error ? <div className="toast">{error}</div> : null}
      </main>
    </div>
  );
}
