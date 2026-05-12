use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use uuid::Uuid;

use crate::models::{
  ChatSettings, Conversation, ConversationFile, Memory, Message, StyleProfile, SummaryRecord, TokenUsage,
  UserProfile,
};

const LATEST_SCHEMA_VERSION: i64 = 12;

fn now_iso() -> String {
  Utc::now().to_rfc3339()
}

fn apply_runtime_pragmas(conn: &Connection) -> Result<()> {
  conn.execute_batch(
    r#"
    PRAGMA journal_mode=WAL;
    PRAGMA synchronous=NORMAL;
    PRAGMA foreign_keys=ON;
    PRAGMA temp_store=MEMORY;
    "#,
  )?;
  Ok(())
}

fn escaped_sqlite_path(path: &Path) -> String {
  path.to_string_lossy().replace('\'', "''")
}

fn default_models() -> Vec<String> {
  vec![
    "deepseek-chat".to_string(),
    "deepseek-reasoner".to_string(),
    "deepseek-v4-flash".to_string(),
    "deepseek-v4-pro".to_string(),
    "gpt-4.1".to_string(),
    "gpt-4.1-mini".to_string(),
  ]
}

fn normalize_provider(input: &str) -> String {
  let p = input.trim().to_lowercase();
  if p.is_empty() {
    "deepseek".to_string()
  } else {
    p
  }
}

fn default_base_url_for_provider(provider: &str) -> &'static str {
  match provider {
    "openai" => "https://api.openai.com/v1",
    "openrouter" => "https://openrouter.ai/api/v1",
    "ollama" => "http://127.0.0.1:11434/v1",
    "custom" => "",
    _ => "https://api.deepseek.com",
  }
}

fn normalize_memory_scope(input: &str) -> &'static str {
  if input.trim().eq_ignore_ascii_case("conversation") {
    "conversation"
  } else {
    "global"
  }
}

fn default_user_profile() -> UserProfile {
  UserProfile {
    preferred_name: String::new(),
    long_term_goals: String::new(),
    interests: String::new(),
    important_experiences: String::new(),
    language_preference: "auto".to_string(),
    notes: String::new(),
    updated_at: now_iso(),
  }
}

fn default_style_profile() -> StyleProfile {
  StyleProfile {
    detail_level: "balanced".to_string(),
    tone: "warm_direct".to_string(),
    technical_level: "balanced".to_string(),
    language_style: "auto".to_string(),
    explicit_preferences: String::new(),
    updated_at: now_iso(),
  }
}

fn row_to_message(r: &Row<'_>) -> rusqlite::Result<Message> {
  let token_usage_text: String = r.get(5)?;
  let token_usage = serde_json::from_str::<TokenUsage>(&token_usage_text).unwrap_or_default();
  Ok(Message {
    id: r.get(0)?,
    conversation_id: r.get(1)?,
    role: r.get(2)?,
    content: r.get(3)?,
    reasoning_content: r.get(4)?,
    token_usage,
    created_at: r.get(6)?,
    updated_at: r.get(7)?,
    deleted_at: r.get(8).ok(),
    deleted_reason: r.get::<_, String>(9).unwrap_or_default(),
  })
}

fn row_to_conversation_file(r: &Row<'_>) -> rusqlite::Result<ConversationFile> {
  Ok(ConversationFile {
    id: r.get(0)?,
    conversation_id: r.get(1)?,
    file_name: r.get(2)?,
    file_path: r.get(3)?,
    file_size: r.get(4)?,
    content_text: r.get(5).unwrap_or_default(),
    summary: r.get(6).unwrap_or_default(),
    created_at: r.get(7)?,
    deleted_at: r.get(8).ok(),
  })
}

fn normalize_for_match(input: &str) -> String {
  input
    .to_lowercase()
    .replace(
      [
        ' ', '\n', '\t', '，', ',', '。', '.', '！', '!', '？', '?', ':', '：', ';', '；', '"',
        '\'', '`',
      ],
      "",
    )
}

fn clip_chars(input: &str, max_chars: usize) -> String {
  if input.chars().count() <= max_chars {
    return input.to_string();
  }
  input.chars().take(max_chars).collect()
}

fn looks_low_value_memory(text: &str) -> bool {
  let t = text.trim().to_lowercase();
  if t.chars().count() <= 8 {
    return true;
  }
  let low_hits = [
    "ok", "嗯", "好的", "谢谢", "收到", "知道了", "yes", "fine", "great", "lol",
  ];
  low_hits.iter().any(|x| t == *x)
}

fn merge_memory_content(old: &str, new_text: &str) -> String {
  let old_trim = old.trim();
  let new_trim = new_text.trim();
  if old_trim.is_empty() {
    return clip_chars(new_trim, 400);
  }
  if new_trim.is_empty() {
    return clip_chars(old_trim, 400);
  }
  if old_trim.contains(new_trim) {
    return clip_chars(old_trim, 400);
  }
  if new_trim.contains(old_trim) {
    return clip_chars(new_trim, 400);
  }
  clip_chars(&format!("{old_trim}；{new_trim}"), 400)
}

fn score_text_overlap(a: &str, b: &str) -> i64 {
  let na = normalize_for_match(a);
  let nb = normalize_for_match(b);
  if na.is_empty() || nb.is_empty() {
    return 0;
  }
  if nb.contains(&na) || na.contains(&nb) {
    return 12;
  }
  na.chars().filter(|c| nb.contains(*c)).count().min(10) as i64
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
  let exists: Option<i64> = conn
    .query_row(
      "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 LIMIT 1",
      params![table],
      |r| r.get(0),
    )
    .optional()?;
  Ok(exists.is_some())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
  let pragma = format!("PRAGMA table_info({table})");
  let mut stmt = conn.prepare(&pragma)?;
  let rows = stmt.query_map([], |r| {
    let name: String = r.get(1)?;
    Ok(name)
  })?;
  for name in rows.flatten() {
    if name.eq_ignore_ascii_case(column) {
      return Ok(true);
    }
  }
  Ok(false)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, def: &str) -> Result<()> {
  if column_exists(conn, table, column)? {
    return Ok(());
  }
  let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {def}");
  conn.execute_batch(&sql)?;
  Ok(())
}

fn create_segmented_summaries_table(conn: &Connection) -> Result<()> {
  conn.execute_batch(
    r#"
    CREATE TABLE IF NOT EXISTS summaries(
      id TEXT PRIMARY KEY,
      conversation_id TEXT NOT NULL,
      segment_index INTEGER NOT NULL,
      summary TEXT NOT NULL,
      covered_start_message_id TEXT NOT NULL DEFAULT '',
      covered_end_message_id TEXT NOT NULL DEFAULT '',
      covered_start_created_at TEXT NOT NULL DEFAULT '',
      covered_end_created_at TEXT NOT NULL DEFAULT '',
      source_message_count INTEGER NOT NULL DEFAULT 0,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_summary_conv_segment ON summaries(conversation_id, segment_index);
    CREATE INDEX IF NOT EXISTS idx_summary_conv_updated ON summaries(conversation_id, updated_at);
    "#,
  )?;
  Ok(())
}

fn ensure_segmented_summaries(conn: &Connection) -> Result<()> {
  if !table_exists(conn, "summaries")? {
    return create_segmented_summaries_table(conn);
  }

  if column_exists(conn, "summaries", "segment_index")? {
    return create_segmented_summaries_table(conn);
  }

  conn.execute_batch("ALTER TABLE summaries RENAME TO summaries_legacy")?;
  create_segmented_summaries_table(conn)?;

  if table_exists(conn, "summaries_legacy")? {
    let mut stmt = match conn.prepare(
      "SELECT id, conversation_id, summary, covered_until_message_id, covered_until_created_at, created_at, updated_at FROM summaries_legacy",
    ) {
      Ok(s) => s,
      Err(_) => {
        conn.execute_batch("DROP TABLE IF EXISTS summaries_legacy")?;
        return Ok(());
      }
    };

    let rows = stmt.query_map([], |r| {
      Ok((
        r.get::<_, String>(0).unwrap_or_else(|_| Uuid::new_v4().to_string()),
        r.get::<_, String>(1).unwrap_or_default(),
        r.get::<_, String>(2).unwrap_or_default(),
        r.get::<_, String>(3).unwrap_or_default(),
        r.get::<_, String>(4).unwrap_or_default(),
        r.get::<_, String>(5).unwrap_or_else(|_| now_iso()),
        r.get::<_, String>(6).unwrap_or_else(|_| now_iso()),
      ))
    })?;

    for (id, conversation_id, summary, end_msg_id, end_ts, created_at, updated_at) in
      rows.flatten()
    {
      if conversation_id.trim().is_empty() || summary.trim().is_empty() {
        continue;
      }
      conn.execute(
        r#"
        INSERT INTO summaries(
          id,conversation_id,segment_index,summary,
          covered_start_message_id,covered_end_message_id,
          covered_start_created_at,covered_end_created_at,
          source_message_count,created_at,updated_at
        ) VALUES(?1,?2,0,?3,'',?4,'',?5,0,?6,?7)
        ON CONFLICT(conversation_id,segment_index) DO UPDATE SET
          summary=excluded.summary,
          covered_end_message_id=excluded.covered_end_message_id,
          covered_end_created_at=excluded.covered_end_created_at,
          updated_at=excluded.updated_at
        "#,
        params![
          if id.trim().is_empty() {
            Uuid::new_v4().to_string()
          } else {
            id
          },
          conversation_id,
          summary,
          end_msg_id,
          end_ts,
          created_at,
          updated_at,
        ],
      )?;
    }

    conn.execute_batch("DROP TABLE IF EXISTS summaries_legacy")?;
  }

  Ok(())
}

fn apply_migration_v1(conn: &Connection) -> Result<()> {
  conn.execute_batch(
    r#"
    CREATE TABLE IF NOT EXISTS settings(
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS conversations(
      id TEXT PRIMARY KEY,
      title TEXT NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS messages(
      id TEXT PRIMARY KEY,
      conversation_id TEXT NOT NULL,
      role TEXT NOT NULL,
      content TEXT NOT NULL,
      reasoning_content TEXT DEFAULT '',
      token_usage TEXT DEFAULT '{"prompt":0,"completion":0,"total":0}',
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
    );
    CREATE TABLE IF NOT EXISTS memories(
      id TEXT PRIMARY KEY,
      content TEXT NOT NULL,
      importance INTEGER NOT NULL DEFAULT 5,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      last_used_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_conv_updated ON conversations(updated_at);
    CREATE INDEX IF NOT EXISTS idx_msg_conv_created ON messages(conversation_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_mem_last_used ON memories(last_used_at);
    "#,
  )?;
  Ok(())
}

fn apply_migration_v2(conn: &Connection) -> Result<()> {
  conn.execute_batch(
    r#"
    CREATE TABLE IF NOT EXISTS user_profile(
      id INTEGER PRIMARY KEY CHECK(id=1),
      preferred_name TEXT NOT NULL DEFAULT '',
      long_term_goals TEXT NOT NULL DEFAULT '',
      interests TEXT NOT NULL DEFAULT '',
      important_experiences TEXT NOT NULL DEFAULT '',
      language_preference TEXT NOT NULL DEFAULT 'auto',
      notes TEXT NOT NULL DEFAULT '',
      updated_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS style_profile(
      id INTEGER PRIMARY KEY CHECK(id=1),
      detail_level TEXT NOT NULL DEFAULT 'balanced',
      tone TEXT NOT NULL DEFAULT 'warm_direct',
      technical_level TEXT NOT NULL DEFAULT 'balanced',
      language_style TEXT NOT NULL DEFAULT 'auto',
      explicit_preferences TEXT NOT NULL DEFAULT '',
      updated_at TEXT NOT NULL
    );
    "#,
  )?;
  ensure_segmented_summaries(conn)
}

fn apply_migration_v3(conn: &Connection) -> Result<()> {
  ensure_column(conn, "messages", "deleted_at", "TEXT")?;
  ensure_column(conn, "messages", "deleted_reason", "TEXT NOT NULL DEFAULT ''")?;
  ensure_column(conn, "memories", "normalized_content", "TEXT NOT NULL DEFAULT ''")?;
  conn.execute_batch(
    r#"
    CREATE INDEX IF NOT EXISTS idx_msg_conv_deleted_created ON messages(conversation_id, deleted_at, created_at);
    CREATE INDEX IF NOT EXISTS idx_memory_norm ON memories(normalized_content);
    "#,
  )?;
  Ok(())
}

fn apply_migration_v4(conn: &Connection) -> Result<()> {
  ensure_segmented_summaries(conn)?;
  conn.execute_batch(
    r#"
    CREATE INDEX IF NOT EXISTS idx_summary_conv_updated ON summaries(conversation_id, updated_at);
    CREATE INDEX IF NOT EXISTS idx_msg_conv_deleted_created ON messages(conversation_id, deleted_at, created_at);
    "#,
  )?;
  Ok(())
}

fn apply_migration_v5(conn: &Connection) -> Result<()> {
  ensure_column(
    conn,
    "memories",
    "scope",
    "TEXT NOT NULL DEFAULT 'global'",
  )?;
  ensure_column(conn, "memories", "conversation_id", "TEXT")?;
  conn.execute_batch(
    r#"
    UPDATE memories SET scope='global' WHERE scope IS NULL OR trim(scope)='';
    CREATE INDEX IF NOT EXISTS idx_memory_scope_conv_used ON memories(scope, conversation_id, last_used_at);
    "#,
  )?;
  Ok(())
}

fn apply_migration_v6(conn: &Connection) -> Result<()> {
  ensure_column(
    conn,
    "conversations",
    "model_override",
    "TEXT NOT NULL DEFAULT ''",
  )?;
  Ok(())
}

fn normalize_conversation_sort_order_locked(conn: &Connection) -> Result<()> {
  if !table_exists(conn, "conversations")? {
    return Ok(());
  }
  if !column_exists(conn, "conversations", "is_pinned")?
    || !column_exists(conn, "conversations", "sort_index")?
  {
    return Ok(());
  }

  let mut stmt = conn.prepare(
    r#"
    SELECT id
    FROM conversations
    ORDER BY
      is_pinned DESC,
      CASE WHEN sort_index<=0 THEN 9223372036854775807 ELSE sort_index END ASC,
      updated_at DESC,
      created_at DESC,
      id ASC
    "#,
  )?;
  let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
  let ids: Vec<String> = rows.flatten().collect();
  for (idx, id) in ids.iter().enumerate() {
    let next = (idx as i64) + 1;
    conn.execute(
      "UPDATE conversations SET sort_index=?1 WHERE id=?2 AND sort_index<>?1",
      params![next, id],
    )?;
  }
  Ok(())
}

fn apply_migration_v7(conn: &Connection) -> Result<()> {
  ensure_column(conn, "conversations", "is_pinned", "INTEGER NOT NULL DEFAULT 0")?;
  ensure_column(conn, "conversations", "sort_index", "INTEGER NOT NULL DEFAULT 0")?;
  conn.execute_batch(
    r#"
    UPDATE conversations SET is_pinned=0 WHERE is_pinned NOT IN (0,1);
    CREATE INDEX IF NOT EXISTS idx_conv_pin_sort_updated ON conversations(is_pinned, sort_index, updated_at);
    "#,
  )?;
  normalize_conversation_sort_order_locked(conn)?;
  Ok(())
}

fn apply_migration_v8(conn: &Connection) -> Result<()> {
  ensure_column(
    conn,
    "conversations",
    "provider_override",
    "TEXT NOT NULL DEFAULT ''",
  )?;
  ensure_column(
    conn,
    "conversations",
    "base_url_override",
    "TEXT NOT NULL DEFAULT ''",
  )?;
  ensure_column(conn, "conversations", "temperature_override", "REAL")?;
  ensure_column(conn, "conversations", "max_tokens_override", "INTEGER")?;
  Ok(())
}

fn apply_migration_v9(conn: &Connection) -> Result<()> {
  conn.execute_batch(
    r#"
    CREATE TABLE IF NOT EXISTS conversation_files(
      id TEXT PRIMARY KEY,
      conversation_id TEXT NOT NULL,
      file_name TEXT NOT NULL,
      file_path TEXT NOT NULL,
      file_size INTEGER NOT NULL DEFAULT 0,
      content_text TEXT NOT NULL DEFAULT '',
      summary TEXT NOT NULL DEFAULT '',
      created_at TEXT NOT NULL,
      deleted_at TEXT,
      FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_conv_files_conv_created ON conversation_files(conversation_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_conv_files_conv_deleted_created ON conversation_files(conversation_id, deleted_at, created_at);
    "#,
  )?;
  ensure_column(conn, "conversation_files", "file_name", "TEXT NOT NULL DEFAULT ''")?;
  ensure_column(conn, "conversation_files", "file_path", "TEXT NOT NULL DEFAULT ''")?;
  ensure_column(conn, "conversation_files", "file_size", "INTEGER NOT NULL DEFAULT 0")?;
  ensure_column(conn, "conversation_files", "content_text", "TEXT NOT NULL DEFAULT ''")?;
  ensure_column(conn, "conversation_files", "summary", "TEXT NOT NULL DEFAULT ''")?;
  ensure_column(conn, "conversation_files", "created_at", "TEXT NOT NULL DEFAULT ''")?;
  ensure_column(conn, "conversation_files", "deleted_at", "TEXT")?;
  Ok(())
}

fn apply_migration_v10(conn: &Connection) -> Result<()> {
  ensure_column(
    conn,
    "conversations",
    "max_context_tokens_override",
    "INTEGER",
  )?;
  ensure_column(
    conn,
    "conversations",
    "max_recent_messages_override",
    "INTEGER",
  )?;
  ensure_column(
    conn,
    "conversations",
    "max_memory_items_override",
    "INTEGER",
  )?;
  ensure_column(
    conn,
    "conversations",
    "system_prompt",
    "TEXT NOT NULL DEFAULT ''",
  )?;
  Ok(())
}

fn apply_migration_v11(conn: &Connection) -> Result<()> {
  ensure_column(conn, "memories", "source_message_id", "TEXT")?;
  ensure_column(conn, "memories", "source_conversation_id", "TEXT")?;
  conn.execute_batch(
    r#"
    CREATE INDEX IF NOT EXISTS idx_memory_source_message ON memories(source_message_id);
    CREATE INDEX IF NOT EXISTS idx_memory_source_conversation ON memories(source_conversation_id);
    "#,
  )?;
  Ok(())
}

fn apply_migration_v12(conn: &Connection) -> Result<()> {
  ensure_column(
    conn,
    "conversations",
    "thinking_override",
    "TEXT NOT NULL DEFAULT ''",
  )?;
  ensure_column(
    conn,
    "conversations",
    "reasoning_effort_override",
    "TEXT NOT NULL DEFAULT ''",
  )?;
  Ok(())
}

pub struct Database {
  path: PathBuf,
  conn: Mutex<Connection>,
}

impl Database {
  pub fn open(path: PathBuf) -> Result<Self> {
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)
        .with_context(|| format!("create db dir failed: {}", parent.display()))?;
    }
    let conn = Connection::open(&path).context("open sqlite failed")?;
    apply_runtime_pragmas(&conn)?;
    Ok(Self {
      path,
      conn: Mutex::new(conn),
    })
  }

  pub fn init_schema(&self, backup_dir: &Path) -> Result<()> {
    {
      let conn = self.conn.lock();
      conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations(
          version INTEGER PRIMARY KEY,
          name TEXT NOT NULL,
          applied_at TEXT NOT NULL
        );
        "#,
      )?;
    }

    let current = {
      let conn = self.conn.lock();
      conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| {
          r.get(0)
        })
        .unwrap_or(0i64)
    };

    if current < LATEST_SCHEMA_VERSION {
      if !backup_dir.as_os_str().is_empty() {
        let backup_name = format!(
          "pre_migration_v{}_to_v{}_{}.db",
          current,
          LATEST_SCHEMA_VERSION,
          Utc::now().format("%Y%m%d_%H%M%S")
        );
        let backup_path = backup_dir.join(backup_name);
        self.backup_to(&backup_path)?;
      }

      let conn = self.conn.lock();
      for v in (current + 1)..=LATEST_SCHEMA_VERSION {
        match v {
          1 => apply_migration_v1(&conn)?,
          2 => apply_migration_v2(&conn)?,
          3 => apply_migration_v3(&conn)?,
          4 => apply_migration_v4(&conn)?,
          5 => apply_migration_v5(&conn)?,
          6 => apply_migration_v6(&conn)?,
          7 => apply_migration_v7(&conn)?,
          8 => apply_migration_v8(&conn)?,
          9 => apply_migration_v9(&conn)?,
          10 => apply_migration_v10(&conn)?,
          11 => apply_migration_v11(&conn)?,
          12 => apply_migration_v12(&conn)?,
          _ => {}
        }
        conn.execute(
          "INSERT OR REPLACE INTO schema_migrations(version,name,applied_at) VALUES(?1,?2,?3)",
          params![v, format!("migration_v{v}"), now_iso()],
        )?;
      }
    }

    self.seed_default_settings()?;
    self.seed_profile_defaults()?;
    self.rebuild_memory_normalized_cache()?;
    self.normalize_conversation_sort_order()?;
    Ok(())
  }

  fn normalize_conversation_sort_order(&self) -> Result<()> {
    let conn = self.conn.lock();
    normalize_conversation_sort_order_locked(&conn)
  }

  fn seed_default_settings(&self) -> Result<()> {
    let conn = self.conn.lock();
    let now = now_iso();
    let models_json = serde_json::to_string(&default_models()).unwrap_or_else(|_| "[]".to_string());
    let defaults: Vec<(&str, String)> = vec![
      ("provider", "deepseek".to_string()),
      ("base_url", "https://api.deepseek.com".to_string()),
      ("model", "deepseek-chat".to_string()),
      ("temperature", "0.7".to_string()),
      ("max_tokens", "4096".to_string()),
      ("max_context_tokens", "12000".to_string()),
      ("max_recent_messages", "20".to_string()),
      ("max_memory_items", "8".to_string()),
      ("last_conversation_id", "".to_string()),
      ("theme_mode", "system".to_string()),
      ("models", models_json),
      ("api_key_deepseek", "".to_string()),
      ("api_key_openai", "".to_string()),
      ("api_key_openrouter", "".to_string()),
      ("api_key_ollama", "".to_string()),
      ("api_key_custom", "".to_string()),
    ];
    for (k, v) in defaults {
      conn.execute(
        "INSERT OR IGNORE INTO settings(key,value,updated_at) VALUES(?1,?2,?3)",
        params![k, v, now],
      )?;
    }
    Ok(())
  }

  fn seed_profile_defaults(&self) -> Result<()> {
    let conn = self.conn.lock();
    let p = default_user_profile();
    conn.execute(
      "INSERT OR IGNORE INTO user_profile(id,preferred_name,long_term_goals,interests,important_experiences,language_preference,notes,updated_at) VALUES(1,?1,?2,?3,?4,?5,?6,?7)",
      params![
        p.preferred_name,
        p.long_term_goals,
        p.interests,
        p.important_experiences,
        p.language_preference,
        p.notes,
        p.updated_at,
      ],
    )?;

    let s = default_style_profile();
    conn.execute(
      "INSERT OR IGNORE INTO style_profile(id,detail_level,tone,technical_level,language_style,explicit_preferences,updated_at) VALUES(1,?1,?2,?3,?4,?5,?6)",
      params![
        s.detail_level,
        s.tone,
        s.technical_level,
        s.language_style,
        s.explicit_preferences,
        s.updated_at,
      ],
    )?;
    Ok(())
  }

  fn rebuild_memory_normalized_cache(&self) -> Result<()> {
    let conn = self.conn.lock();
    if !column_exists(&conn, "memories", "normalized_content")? {
      return Ok(());
    }

    let mut stmt = conn.prepare("SELECT id, content FROM memories")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;

    for (id, content) in rows.flatten() {
      let normalized = normalize_for_match(content.as_str());
      let _ = conn.execute(
        "UPDATE memories SET normalized_content=?1 WHERE id=?2",
        params![normalized, id],
      );
    }
    Ok(())
  }

  pub fn get_setting(&self, key: &str, default: &str) -> String {
    let conn = self.conn.lock();
    conn
      .query_row("SELECT value FROM settings WHERE key=?1", params![key], |r| {
        r.get::<_, String>(0)
      })
      .optional()
      .unwrap_or(None)
      .unwrap_or_else(|| default.to_string())
  }

  pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
    let conn = self.conn.lock();
    conn.execute(
      r#"
      INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,?3)
      ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at
      "#,
      params![key, value, now_iso()],
    )?;
    Ok(())
  }

  pub fn get_models(&self) -> Vec<String> {
    let raw = self.get_setting("models", "[]");
    let parsed = serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default();
    if parsed.is_empty() {
      default_models()
    } else {
      parsed
    }
  }

  pub fn set_models(&self, models: &[String]) -> Result<()> {
    self.set_setting("models", &serde_json::to_string(models)?)
  }

  pub fn get_chat_settings(&self) -> ChatSettings {
    let provider = normalize_provider(&self.get_setting("provider", "deepseek"));
    let provider_default_base = default_base_url_for_provider(&provider);
    let base_url_saved = self.get_setting("base_url", provider_default_base);
    let base_url = if base_url_saved.trim().is_empty() {
      provider_default_base.to_string()
    } else {
      base_url_saved
    };
    let model_saved = self.get_setting("model", "deepseek-chat");
    let model = if model_saved.trim().is_empty() {
      "deepseek-chat".to_string()
    } else {
      model_saved
    };

    let max_tokens = self
      .get_setting("max_tokens", "4096")
      .parse::<i64>()
      .unwrap_or(4096);
    let max_tokens = if max_tokens <= 0 { 4096 } else { max_tokens };

    let max_context_tokens = self
      .get_setting("max_context_tokens", "12000")
      .parse::<i64>()
      .unwrap_or(12000);
    let max_context_tokens = if max_context_tokens <= 0 {
      12000
    } else {
      max_context_tokens
    };

    let max_recent_messages = self
      .get_setting("max_recent_messages", "20")
      .parse::<i64>()
      .unwrap_or(20);
    let max_recent_messages = if max_recent_messages <= 0 {
      20
    } else {
      max_recent_messages
    };

    let max_memory_items = self
      .get_setting("max_memory_items", "8")
      .parse::<i64>()
      .unwrap_or(8);
    let max_memory_items = if max_memory_items < 0 {
      8
    } else {
      max_memory_items
    };

    ChatSettings {
      provider,
      base_url,
      model,
      temperature: self
        .get_setting("temperature", "0.7")
        .parse::<f32>()
        .unwrap_or(0.7),
      max_tokens,
      max_context_tokens,
      max_recent_messages,
      max_memory_items,
    }
  }

  pub fn set_chat_settings(&self, settings: &ChatSettings) -> Result<()> {
    let provider = normalize_provider(&settings.provider);
    let mut base_url = settings.base_url.trim().to_string();
    if base_url.is_empty() && provider != "custom" {
      base_url = default_base_url_for_provider(&provider).to_string();
    }
    let model = if settings.model.trim().is_empty() {
      "deepseek-chat".to_string()
    } else {
      settings.model.trim().to_string()
    };

    self.set_setting("provider", &provider)?;
    self.set_setting("base_url", &base_url)?;
    self.set_setting("model", &model)?;
    let max_tokens = if settings.max_tokens <= 0 {
      4096
    } else {
      settings.max_tokens
    };
    let max_context_tokens = if settings.max_context_tokens <= 0 {
      12000
    } else {
      settings.max_context_tokens
    };
    let max_recent_messages = if settings.max_recent_messages <= 0 {
      20
    } else {
      settings.max_recent_messages
    };
    let max_memory_items = if settings.max_memory_items < 0 {
      8
    } else {
      settings.max_memory_items
    };

    self.set_setting("temperature", &settings.temperature.to_string())?;
    self.set_setting("max_tokens", &max_tokens.to_string())?;
    self.set_setting("max_context_tokens", &max_context_tokens.to_string())?;
    self.set_setting("max_recent_messages", &max_recent_messages.to_string())?;
    self.set_setting("max_memory_items", &max_memory_items.to_string())?;
    Ok(())
  }

  pub fn get_user_profile(&self) -> UserProfile {
    let conn = self.conn.lock();
    conn
      .query_row(
        "SELECT preferred_name,long_term_goals,interests,important_experiences,language_preference,notes,updated_at FROM user_profile WHERE id=1",
        [],
        |r| {
          Ok(UserProfile {
            preferred_name: r.get(0)?,
            long_term_goals: r.get(1)?,
            interests: r.get(2)?,
            important_experiences: r.get(3)?,
            language_preference: r.get(4)?,
            notes: r.get(5)?,
            updated_at: r.get(6)?,
          })
        },
      )
      .optional()
      .unwrap_or(None)
      .unwrap_or_else(default_user_profile)
  }

  pub fn save_user_profile(&self, profile: &UserProfile) -> Result<()> {
    let conn = self.conn.lock();
    let now = now_iso();
    conn.execute(
      r#"
      INSERT INTO user_profile(id,preferred_name,long_term_goals,interests,important_experiences,language_preference,notes,updated_at)
      VALUES(1,?1,?2,?3,?4,?5,?6,?7)
      ON CONFLICT(id) DO UPDATE SET
        preferred_name=excluded.preferred_name,
        long_term_goals=excluded.long_term_goals,
        interests=excluded.interests,
        important_experiences=excluded.important_experiences,
        language_preference=excluded.language_preference,
        notes=excluded.notes,
        updated_at=excluded.updated_at
      "#,
      params![
        profile.preferred_name,
        profile.long_term_goals,
        profile.interests,
        profile.important_experiences,
        profile.language_preference,
        profile.notes,
        now,
      ],
    )?;
    Ok(())
  }

  pub fn get_style_profile(&self) -> StyleProfile {
    let conn = self.conn.lock();
    conn
      .query_row(
        "SELECT detail_level,tone,technical_level,language_style,explicit_preferences,updated_at FROM style_profile WHERE id=1",
        [],
        |r| {
          Ok(StyleProfile {
            detail_level: r.get(0)?,
            tone: r.get(1)?,
            technical_level: r.get(2)?,
            language_style: r.get(3)?,
            explicit_preferences: r.get(4)?,
            updated_at: r.get(5)?,
          })
        },
      )
      .optional()
      .unwrap_or(None)
      .unwrap_or_else(default_style_profile)
  }

  pub fn save_style_profile(&self, profile: &StyleProfile) -> Result<()> {
    let conn = self.conn.lock();
    let now = now_iso();
    conn.execute(
      r#"
      INSERT INTO style_profile(id,detail_level,tone,technical_level,language_style,explicit_preferences,updated_at)
      VALUES(1,?1,?2,?3,?4,?5,?6)
      ON CONFLICT(id) DO UPDATE SET
        detail_level=excluded.detail_level,
        tone=excluded.tone,
        technical_level=excluded.technical_level,
        language_style=excluded.language_style,
        explicit_preferences=excluded.explicit_preferences,
        updated_at=excluded.updated_at
      "#,
      params![
        profile.detail_level,
        profile.tone,
        profile.technical_level,
        profile.language_style,
        profile.explicit_preferences,
        now,
      ],
    )?;
    Ok(())
  }

  pub fn list_summary_segments(&self, conversation_id: &str, limit: i64) -> Result<Vec<SummaryRecord>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT
        conversation_id,
        segment_index,
        summary,
        covered_start_message_id,
        covered_end_message_id,
        covered_start_created_at,
        covered_end_created_at,
        source_message_count,
        updated_at
      FROM summaries
      WHERE conversation_id=?1
      ORDER BY segment_index DESC
      LIMIT ?2
      "#,
    )?;
    let rows = stmt.query_map(params![conversation_id, limit.max(1)], |r| {
      Ok(SummaryRecord {
        conversation_id: r.get(0)?,
        segment_index: r.get(1)?,
        summary: r.get(2)?,
        covered_start_message_id: r.get(3)?,
        covered_end_message_id: r.get(4)?,
        covered_start_created_at: r.get(5)?,
        covered_end_created_at: r.get(6)?,
        source_message_count: r.get(7)?,
        updated_at: r.get(8)?,
      })
    })?;
    Ok(rows.flatten().collect())
  }

  pub fn latest_summary_end_created_at(&self, conversation_id: &str) -> Result<Option<String>> {
    let conn = self.conn.lock();
    conn
      .query_row(
        "SELECT covered_end_created_at FROM summaries WHERE conversation_id=?1 ORDER BY segment_index DESC LIMIT 1",
        params![conversation_id],
        |r| r.get::<_, String>(0),
      )
      .optional()
      .map_err(Into::into)
  }

  pub fn append_summary_segment(
    &self,
    conversation_id: &str,
    summary: &str,
    covered_start_message_id: &str,
    covered_end_message_id: &str,
    covered_start_created_at: &str,
    covered_end_created_at: &str,
    source_message_count: i64,
  ) -> Result<()> {
    let conn = self.conn.lock();
    let next_index: i64 = conn
      .query_row(
        "SELECT COALESCE(MAX(segment_index), -1) + 1 FROM summaries WHERE conversation_id=?1",
        params![conversation_id],
        |r| r.get(0),
      )
      .unwrap_or(0);

    let now = now_iso();
    conn.execute(
      r#"
      INSERT INTO summaries(
        id,conversation_id,segment_index,summary,
        covered_start_message_id,covered_end_message_id,
        covered_start_created_at,covered_end_created_at,
        source_message_count,created_at,updated_at
      ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)
      "#,
      params![
        Uuid::new_v4().to_string(),
        conversation_id,
        next_index,
        summary,
        covered_start_message_id,
        covered_end_message_id,
        covered_start_created_at,
        covered_end_created_at,
        source_message_count.max(0),
        now,
      ],
    )?;
    Ok(())
  }

  pub fn list_conversations(&self, query: &str, limit: i64) -> Result<Vec<Conversation>> {
    let conn = self.conn.lock();
    let q = format!("%{}%", query.trim());
    let mut stmt = conn.prepare(
      r#"
      SELECT
        c.id,c.title,c.created_at,c.updated_at,
        c.model_override,c.is_pinned,
        c.provider_override,c.base_url_override,c.temperature_override,c.max_tokens_override,
        c.max_context_tokens_override,c.max_recent_messages_override,c.max_memory_items_override,c.system_prompt,
        c.thinking_override,c.reasoning_effort_override,
        (SELECT COUNT(1) FROM messages m WHERE m.conversation_id=c.id AND m.deleted_at IS NULL) AS message_count
      FROM conversations c
      WHERE (?1='' OR c.title LIKE ?2)
      ORDER BY c.is_pinned DESC, c.sort_index ASC, c.updated_at DESC
      LIMIT ?3
      "#,
    )?;
    let rows = stmt.query_map(params![query.trim(), q, limit], |r| {
      Ok(Conversation {
        id: r.get(0)?,
        title: r.get(1)?,
        created_at: r.get(2)?,
        updated_at: r.get(3)?,
        model_override: r.get::<_, String>(4).unwrap_or_default(),
        is_pinned: r.get::<_, i64>(5).unwrap_or(0) == 1,
        provider_override: r.get::<_, String>(6).unwrap_or_default(),
        base_url_override: r.get::<_, String>(7).unwrap_or_default(),
        temperature_override: r.get::<_, Option<f32>>(8).ok().flatten(),
        max_tokens_override: r.get::<_, Option<i64>>(9).ok().flatten(),
        max_context_tokens_override: r.get::<_, Option<i64>>(10).ok().flatten(),
        max_recent_messages_override: r.get::<_, Option<i64>>(11).ok().flatten(),
        max_memory_items_override: r.get::<_, Option<i64>>(12).ok().flatten(),
        system_prompt: r.get::<_, String>(13).unwrap_or_default(),
        thinking_override: r.get::<_, String>(14).unwrap_or_default(),
        reasoning_effort_override: r.get::<_, String>(15).unwrap_or_default(),
        message_count: r.get(16)?,
      })
    })?;
    Ok(rows.flatten().collect())
  }

  pub fn create_conversation(&self, title: Option<&str>) -> Result<Conversation> {
    let id = Uuid::new_v4().to_string();
    let now = now_iso();
    let t = title.unwrap_or("New Chat").trim();
    let title = if t.is_empty() { "New Chat" } else { t };
    let conn = self.conn.lock();
    let top_unpinned: i64 = conn
      .query_row(
        "SELECT COALESCE(MIN(sort_index), 1) FROM conversations WHERE is_pinned=0",
        [],
        |r| r.get(0),
      )
      .unwrap_or(1);
    let new_sort = top_unpinned - 1;
    conn.execute(
      "INSERT INTO conversations(id,title,created_at,updated_at,model_override,provider_override,base_url_override,temperature_override,max_tokens_override,max_context_tokens_override,max_recent_messages_override,max_memory_items_override,system_prompt,thinking_override,reasoning_effort_override,is_pinned,sort_index) VALUES(?1,?2,?3,?3,'','','',NULL,NULL,NULL,NULL,NULL,'','','',0,?4)",
      params![id, title, now, new_sort],
    )?;
    normalize_conversation_sort_order_locked(&conn)?;
    Ok(Conversation {
      id,
      title: title.to_string(),
      created_at: now.clone(),
      updated_at: now,
      message_count: 0,
      model_override: String::new(),
      is_pinned: false,
      provider_override: String::new(),
      base_url_override: String::new(),
      temperature_override: None,
      max_tokens_override: None,
      max_context_tokens_override: None,
      max_recent_messages_override: None,
      max_memory_items_override: None,
      system_prompt: String::new(),
      thinking_override: String::new(),
      reasoning_effort_override: String::new(),
    })
  }

  pub fn rename_conversation(&self, conversation_id: &str, title: &str) -> Result<()> {
    let conn = self.conn.lock();
    conn.execute(
      "UPDATE conversations SET title=?1, updated_at=?2 WHERE id=?3",
      params![title.trim(), now_iso(), conversation_id],
    )?;
    Ok(())
  }

  pub fn set_conversation_pinned(&self, conversation_id: &str, pinned: bool) -> Result<()> {
    let conn = self.conn.lock();
    let is_pinned = if pinned { 1 } else { 0 };
    let sort_index: i64 = if pinned {
      let min_pinned: i64 = conn
        .query_row(
          "SELECT COALESCE(MIN(sort_index), 1) FROM conversations WHERE is_pinned=1",
          [],
          |r| r.get(0),
        )
        .unwrap_or(1);
      min_pinned - 1
    } else {
      let max_unpinned: i64 = conn
        .query_row(
          "SELECT COALESCE(MAX(sort_index), 0) FROM conversations WHERE is_pinned=0",
          [],
          |r| r.get(0),
        )
        .unwrap_or(0);
      max_unpinned + 1
    };
    conn.execute(
      "UPDATE conversations SET is_pinned=?1, sort_index=?2, updated_at=?3 WHERE id=?4",
      params![is_pinned, sort_index, now_iso(), conversation_id],
    )?;
    normalize_conversation_sort_order_locked(&conn)
  }

  pub fn move_conversation(&self, conversation_id: &str, direction: &str) -> Result<()> {
    let conn = self.conn.lock();
    normalize_conversation_sort_order_locked(&conn)?;

    let current: Option<(i64, i64)> = conn
      .query_row(
        "SELECT is_pinned, sort_index FROM conversations WHERE id=?1 LIMIT 1",
        params![conversation_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
      )
      .optional()?;
    let Some((is_pinned, current_sort)) = current else {
      return Ok(());
    };

    let direction = direction.trim().to_lowercase();
    let target: Option<(String, i64)> = if direction == "down" {
      conn
        .query_row(
          "SELECT id, sort_index FROM conversations WHERE is_pinned=?1 AND sort_index>?2 ORDER BY sort_index ASC LIMIT 1",
          params![is_pinned, current_sort],
          |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    } else {
      conn
        .query_row(
          "SELECT id, sort_index FROM conversations WHERE is_pinned=?1 AND sort_index<?2 ORDER BY sort_index DESC LIMIT 1",
          params![is_pinned, current_sort],
          |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    };

    let Some((target_id, target_sort)) = target else {
      return Ok(());
    };

    conn.execute(
      "UPDATE conversations SET sort_index=?1 WHERE id=?2",
      params![target_sort, conversation_id],
    )?;
    conn.execute(
      "UPDATE conversations SET sort_index=?1 WHERE id=?2",
      params![current_sort, target_id],
    )?;

    normalize_conversation_sort_order_locked(&conn)
  }

  pub fn set_conversation_model(&self, conversation_id: &str, model: &str) -> Result<()> {
    let conn = self.conn.lock();
    conn.execute(
      "UPDATE conversations SET model_override=?1, updated_at=?2 WHERE id=?3",
      params![model.trim(), now_iso(), conversation_id],
    )?;
    Ok(())
  }

  pub fn set_conversation_chat_settings(
    &self,
    conversation_id: &str,
    provider_override: &str,
    model_override: &str,
    base_url_override: &str,
    temperature_override: Option<f32>,
    max_tokens_override: Option<i64>,
    max_context_tokens_override: Option<i64>,
    max_recent_messages_override: Option<i64>,
    max_memory_items_override: Option<i64>,
    system_prompt: Option<&str>,
    thinking_override: Option<&str>,
    reasoning_effort_override: Option<&str>,
  ) -> Result<()> {
    let conn = self.conn.lock();
    let prompt = system_prompt.unwrap_or("").trim().to_string();
    let thinking = thinking_override.unwrap_or("").trim().to_string();
    let effort = reasoning_effort_override.unwrap_or("").trim().to_string();
    conn.execute(
      r#"
      UPDATE conversations
      SET
        provider_override=?1,
        model_override=?2,
        base_url_override=?3,
        temperature_override=?4,
        max_tokens_override=?5,
        max_context_tokens_override=?6,
        max_recent_messages_override=?7,
        max_memory_items_override=?8,
        system_prompt=?9,
        thinking_override=?10,
        reasoning_effort_override=?11,
        updated_at=?12
      WHERE id=?13
      "#,
      params![
        provider_override.trim(),
        model_override.trim(),
        base_url_override.trim(),
        temperature_override,
        max_tokens_override,
        max_context_tokens_override,
        max_recent_messages_override,
        max_memory_items_override,
        prompt,
        thinking,
        effort,
        now_iso(),
        conversation_id
      ],
    )?;
    Ok(())
  }

  pub fn get_conversation_chat_overrides(
    &self,
    conversation_id: &str,
  ) -> Result<(
    String,
    String,
    String,
    Option<f32>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    String,
    String,
    String,
  )> {
    let conn = self.conn.lock();
    let row: Option<(
      String,
      String,
      String,
      Option<f32>,
      Option<i64>,
      Option<i64>,
      Option<i64>,
      Option<i64>,
      String,
      String,
      String,
    )> = conn
      .query_row(
        "SELECT provider_override, model_override, base_url_override, temperature_override, max_tokens_override, max_context_tokens_override, max_recent_messages_override, max_memory_items_override, system_prompt, thinking_override, reasoning_effort_override FROM conversations WHERE id=?1 LIMIT 1",
        params![conversation_id],
        |r| {
          Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3).ok().flatten(),
            r.get(4).ok().flatten(),
            r.get(5).ok().flatten(),
            r.get(6).ok().flatten(),
            r.get(7).ok().flatten(),
            r.get::<_, String>(8).unwrap_or_default(),
            r.get::<_, String>(9).unwrap_or_default(),
            r.get::<_, String>(10).unwrap_or_default(),
          ))
        },
      )
      .optional()?;
    Ok(row.unwrap_or_default())
  }

  pub fn delete_conversation(&self, conversation_id: &str) -> Result<()> {
    let conn = self.conn.lock();
    conn.execute("DELETE FROM conversations WHERE id=?1", params![conversation_id])?;
    Ok(())
  }

  pub fn add_conversation_file(
    &self,
    conversation_id: &str,
    file_name: &str,
    file_path: &str,
    file_size: i64,
    content_text: &str,
    summary: &str,
  ) -> Result<ConversationFile> {
    let now = now_iso();
    let id = Uuid::new_v4().to_string();
    let conn = self.conn.lock();
    conn.execute(
      r#"
      INSERT INTO conversation_files(
        id,conversation_id,file_name,file_path,file_size,content_text,summary,created_at,deleted_at
      ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,NULL)
      "#,
      params![
        id,
        conversation_id.trim(),
        file_name.trim(),
        file_path.trim(),
        file_size.max(0),
        content_text,
        summary,
        now
      ],
    )?;
    conn.execute(
      "UPDATE conversations SET updated_at=?1 WHERE id=?2",
      params![now_iso(), conversation_id],
    )?;
    Ok(ConversationFile {
      id,
      conversation_id: conversation_id.trim().to_string(),
      file_name: file_name.trim().to_string(),
      file_path: file_path.trim().to_string(),
      file_size: file_size.max(0),
      content_text: content_text.to_string(),
      summary: summary.to_string(),
      created_at: now,
      deleted_at: None,
    })
  }

  pub fn list_conversation_files(&self, conversation_id: &str, limit: i64) -> Result<Vec<ConversationFile>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT
        id,conversation_id,file_name,file_path,file_size,'' AS content_text,summary,created_at,deleted_at
      FROM conversation_files
      WHERE conversation_id=?1 AND deleted_at IS NULL
      ORDER BY created_at DESC
      LIMIT ?2
      "#,
    )?;
    let rows = stmt.query_map(params![conversation_id.trim(), limit.max(1)], row_to_conversation_file)?;
    Ok(rows.flatten().collect())
  }

  pub fn list_conversation_files_for_context(
    &self,
    conversation_id: &str,
    limit: i64,
  ) -> Result<Vec<ConversationFile>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT
        id,conversation_id,file_name,file_path,file_size,content_text,summary,created_at,deleted_at
      FROM conversation_files
      WHERE conversation_id=?1 AND deleted_at IS NULL
      ORDER BY created_at DESC
      LIMIT ?2
      "#,
    )?;
    let rows = stmt.query_map(params![conversation_id.trim(), limit.max(1)], row_to_conversation_file)?;
    Ok(rows.flatten().collect())
  }

  pub fn soft_delete_conversation_file(&self, file_id: &str) -> Result<Option<ConversationFile>> {
    let conn = self.conn.lock();
    let row = conn
      .query_row(
        r#"
        SELECT id,conversation_id,file_name,file_path,file_size,content_text,summary,created_at,deleted_at
        FROM conversation_files
        WHERE id=?1 AND deleted_at IS NULL
        LIMIT 1
        "#,
        params![file_id],
        row_to_conversation_file,
      )
      .optional()?;
    let Some(mut file) = row else {
      return Ok(None);
    };
    let deleted_at = now_iso();
    conn.execute(
      "UPDATE conversation_files SET deleted_at=?1 WHERE id=?2 AND deleted_at IS NULL",
      params![deleted_at, file_id],
    )?;
    conn.execute(
      "UPDATE conversations SET updated_at=?1 WHERE id=?2",
      params![now_iso(), file.conversation_id],
    )?;
    file.deleted_at = Some(deleted_at);
    Ok(Some(file))
  }

  pub fn list_messages(&self, conversation_id: &str, limit: i64, offset: i64) -> Result<Vec<Message>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT id,conversation_id,role,content,reasoning_content,token_usage,created_at,updated_at,deleted_at,deleted_reason
      FROM messages
      WHERE conversation_id=?1 AND deleted_at IS NULL
      ORDER BY created_at DESC
      LIMIT ?2 OFFSET ?3
      "#,
    )?;
    let rows = stmt.query_map(params![conversation_id, limit, offset], row_to_message)?;
    let mut items: Vec<Message> = rows.flatten().collect();
    items.reverse();
    Ok(items)
  }

  pub fn list_messages_after(
    &self,
    conversation_id: &str,
    after_created_at: Option<&str>,
    limit: i64,
  ) -> Result<Vec<Message>> {
    let conn = self.conn.lock();
    let marker = after_created_at.unwrap_or("").trim().to_string();
    let mut stmt = conn.prepare(
      r#"
      SELECT id,conversation_id,role,content,reasoning_content,token_usage,created_at,updated_at,deleted_at,deleted_reason
      FROM messages
      WHERE conversation_id=?1 AND deleted_at IS NULL AND (?2='' OR created_at>?2)
      ORDER BY created_at ASC
      LIMIT ?3
      "#,
    )?;
    let rows = stmt.query_map(params![conversation_id, marker, limit.max(1)], row_to_message)?;
    Ok(rows.flatten().collect())
  }

  pub fn get_message(&self, message_id: &str) -> Result<Option<Message>> {
    let conn = self.conn.lock();
    conn
      .query_row(
        r#"
        SELECT id,conversation_id,role,content,reasoning_content,token_usage,created_at,updated_at,deleted_at,deleted_reason
        FROM messages WHERE id=?1 AND deleted_at IS NULL LIMIT 1
        "#,
        params![message_id],
        row_to_message,
      )
      .optional()
      .map_err(Into::into)
  }

  pub fn add_message(
    &self,
    conversation_id: &str,
    role: &str,
    content: &str,
    reasoning_content: &str,
    token_usage: &TokenUsage,
  ) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let now = now_iso();
    let usage_json =
      serde_json::to_string(token_usage).unwrap_or_else(|_| "{\"prompt\":0,\"completion\":0,\"total\":0}".to_string());
    let conn = self.conn.lock();
    conn.execute(
      r#"
      INSERT INTO messages(
        id,conversation_id,role,content,reasoning_content,token_usage,created_at,updated_at,deleted_at,deleted_reason
      ) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,NULL,'')
      "#,
      params![
        id,
        conversation_id,
        role,
        content,
        reasoning_content,
        usage_json,
        now
      ],
    )?;
    conn.execute(
      "UPDATE conversations SET updated_at=?1 WHERE id=?2",
      params![now_iso(), conversation_id],
    )?;
    Ok(id)
  }

  pub fn insert_imported_message(
    &self,
    conversation_id: &str,
    role: &str,
    content: &str,
    reasoning_content: &str,
    created_at: &str,
  ) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let usage_json = "{\"prompt\":0,\"completion\":0,\"total\":0}".to_string();
    let conn = self.conn.lock();
    conn.execute(
      r#"
      INSERT INTO messages(
        id,conversation_id,role,content,reasoning_content,token_usage,created_at,updated_at,deleted_at,deleted_reason
      ) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,NULL,'')
      "#,
      params![id, conversation_id, role, content, reasoning_content, usage_json, created_at],
    )?;
    conn.execute(
      "UPDATE conversations SET updated_at=?1 WHERE id=?2",
      params![now_iso(), conversation_id],
    )?;
    Ok(id)
  }

  pub fn update_message_content(&self, message_id: &str, content: &str) -> Result<()> {
    let conn = self.conn.lock();
    conn.execute(
      "UPDATE messages SET content=?1, updated_at=?2 WHERE id=?3 AND deleted_at IS NULL",
      params![content, now_iso(), message_id],
    )?;
    Ok(())
  }

  pub fn delete_message(&self, message_id: &str, soft: bool, reason: &str) -> Result<Option<String>> {
    let conn = self.conn.lock();
    let row: Option<String> = conn
      .query_row(
        "SELECT conversation_id FROM messages WHERE id=?1 LIMIT 1",
        params![message_id],
        |r| r.get(0),
      )
      .optional()?;

    let Some(conversation_id) = row else {
      return Ok(None);
    };

    if soft {
      conn.execute(
        "UPDATE messages SET deleted_at=?1, deleted_reason=?2, updated_at=?1 WHERE id=?3 AND deleted_at IS NULL",
        params![now_iso(), reason.trim(), message_id],
      )?;
    } else {
      conn.execute("DELETE FROM messages WHERE id=?1", params![message_id])?;
    }

    conn.execute(
      "DELETE FROM summaries WHERE conversation_id=?1",
      params![conversation_id],
    )?;
    conn.execute(
      "UPDATE conversations SET updated_at=?1 WHERE id=?2",
      params![now_iso(), conversation_id],
    )?;

    Ok(Some(conversation_id))
  }

  pub fn truncate_messages_after(&self, message_id: &str) -> Result<Option<String>> {
    let conn = self.conn.lock();
    let row: Option<(String, String)> = conn
      .query_row(
        "SELECT conversation_id, created_at FROM messages WHERE id=?1 AND deleted_at IS NULL LIMIT 1",
        params![message_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
      )
      .optional()?;
    let Some((conversation_id, ts)) = row else {
      return Ok(None);
    };
    conn.execute(
      "DELETE FROM messages WHERE conversation_id=?1 AND created_at>?2",
      params![conversation_id, ts],
    )?;
    conn.execute(
      "DELETE FROM summaries WHERE conversation_id=?1",
      params![conversation_id],
    )?;
    conn.execute(
      "UPDATE conversations SET updated_at=?1 WHERE id=?2",
      params![now_iso(), conversation_id],
    )?;
    Ok(Some(conversation_id))
  }

  pub fn get_recent_messages(&self, conversation_id: &str, limit: i64) -> Result<Vec<Message>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT id,conversation_id,role,content,reasoning_content,token_usage,created_at,updated_at,deleted_at,deleted_reason
      FROM messages
      WHERE conversation_id=?1 AND deleted_at IS NULL
      ORDER BY created_at DESC
      LIMIT ?2
      "#,
    )?;
    let rows = stmt.query_map(params![conversation_id, limit], row_to_message)?;
    let mut items: Vec<Message> = rows.flatten().collect();
    items.reverse();
    Ok(items)
  }

  pub fn maybe_auto_title(&self, conversation_id: &str, title: &str) -> Result<()> {
    let conn = self.conn.lock();
    let current: Option<String> = conn
      .query_row(
        "SELECT title FROM conversations WHERE id=?1 LIMIT 1",
        params![conversation_id],
        |r| r.get(0),
      )
      .optional()?;
    if matches!(current.as_deref(), Some("New Chat") | Some("新对话") | Some("")) {
      conn.execute(
        "UPDATE conversations SET title=?1, updated_at=?2 WHERE id=?3",
        params![title, now_iso(), conversation_id],
      )?;
    }
    Ok(())
  }

  pub fn upsert_memory(
    &self,
    content: &str,
    importance: i64,
    scope: &str,
    conversation_id: Option<&str>,
    source_message_id: Option<&str>,
    source_conversation_id: Option<&str>,
  ) -> Result<()> {
    let text = content.trim();
    if text.chars().count() < 6 {
      return Ok(());
    }

    let normalized = normalize_for_match(text);
    if normalized.is_empty() {
      return Ok(());
    }

    let scope = normalize_memory_scope(scope);
    let conv_id = if scope == "conversation" {
      conversation_id.map(str::trim).filter(|v| !v.is_empty())
    } else {
      None
    };
    let source_msg_id = source_message_id.map(str::trim).filter(|v| !v.is_empty());
    let source_conv_id = source_conversation_id
      .map(str::trim)
      .filter(|v| !v.is_empty());

    let conn = self.conn.lock();
    let now = now_iso();

    let exact: Option<(String, String, i64, Option<String>, Option<String>)> = conn
      .query_row(
        "SELECT id, content, importance, source_message_id, source_conversation_id FROM memories WHERE normalized_content=?1 AND scope=?2 AND ((conversation_id IS NULL AND ?3 IS NULL) OR conversation_id=?3) LIMIT 1",
        params![normalized, scope, conv_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3).ok(), r.get(4).ok())),
      )
      .optional()?;

    let incoming = importance.clamp(1, 10);
    let incoming_adjusted = if looks_low_value_memory(text) {
      (incoming - 2).max(1)
    } else {
      incoming
    };

    if let Some((id, old_content, old_importance, old_source_msg, old_source_conv)) = exact {
      let merged_content = merge_memory_content(&old_content, text);
      let mut merged_importance = ((old_importance * 3 + incoming_adjusted * 2) / 5).clamp(1, 10);
      if !looks_low_value_memory(&merged_content) {
        merged_importance = (merged_importance + 1).min(10);
      }
      let next_source_msg = old_source_msg
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| source_msg_id.map(str::to_string));
      let next_source_conv = old_source_conv
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| source_conv_id.map(str::to_string));
      conn.execute(
        "UPDATE memories SET content=?1, normalized_content=?2, importance=?3, source_message_id=?4, source_conversation_id=?5, updated_at=?6, last_used_at=?6 WHERE id=?7",
        params![
          merged_content,
          normalized,
          merged_importance,
          next_source_msg,
          next_source_conv,
          now,
          id
        ],
      )?;
      return Ok(());
    }

    let mut best_id = String::new();
    let mut best_content = String::new();
    let mut best_importance = 0i64;
    let mut best_score = 0i64;
    let mut best_source_msg: Option<String> = None;
    let mut best_source_conv: Option<String> = None;

    let mut stmt = conn.prepare(
      "SELECT id, content, importance, normalized_content, source_message_id, source_conversation_id FROM memories WHERE scope=?1 AND ((conversation_id IS NULL AND ?2 IS NULL) OR conversation_id=?2) ORDER BY importance DESC, last_used_at DESC LIMIT 300",
    )?;
    let rows = stmt.query_map(params![scope, conv_id], |r| {
      Ok((
        r.get::<_, String>(0)?,
        r.get::<_, String>(1)?,
        r.get::<_, i64>(2)?,
        r.get::<_, String>(3).unwrap_or_default(),
        r.get::<_, Option<String>>(4).ok().flatten(),
        r.get::<_, Option<String>>(5).ok().flatten(),
      ))
    })?;

    for (id, old_content, old_importance, old_norm, old_source_msg, old_source_conv) in rows.flatten() {
      let score = score_text_overlap(&normalized, &old_norm) + score_text_overlap(text, &old_content);
      if score > best_score {
        best_score = score;
        best_id = id;
        best_content = old_content;
        best_importance = old_importance;
        best_source_msg = old_source_msg;
        best_source_conv = old_source_conv;
      }
    }

    if best_score >= 14 && !best_id.is_empty() {
      let merged_content = merge_memory_content(&best_content, text);
      let mut merged_importance = ((best_importance * 3 + incoming_adjusted * 2) / 5).clamp(1, 10);
      if !looks_low_value_memory(&merged_content) {
        merged_importance = (merged_importance + 1).min(10);
      }
      let next_source_msg = best_source_msg
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| source_msg_id.map(str::to_string));
      let next_source_conv = best_source_conv
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| source_conv_id.map(str::to_string));
      conn.execute(
        "UPDATE memories SET content=?1, normalized_content=?2, importance=?3, source_message_id=?4, source_conversation_id=?5, updated_at=?6, last_used_at=?6 WHERE id=?7",
        params![
          merged_content,
          normalize_for_match(&merged_content),
          merged_importance,
          next_source_msg,
          next_source_conv,
          now,
          best_id
        ],
      )?;
      return Ok(());
    }

    conn.execute(
      "INSERT INTO memories(id,content,normalized_content,importance,created_at,updated_at,last_used_at,scope,conversation_id,source_message_id,source_conversation_id) VALUES(?1,?2,?3,?4,?5,?5,?5,?6,?7,?8,?9)",
      params![
        Uuid::new_v4().to_string(),
        text,
        normalized,
        incoming_adjusted,
        now,
        scope,
        conv_id,
        source_msg_id,
        source_conv_id
      ],
    )?;
    Ok(())
  }

  pub fn decay_memories(&self) -> Result<()> {
    let conn = self.conn.lock();
    let now = now_iso();

    conn.execute(
      "UPDATE memories SET importance=MAX(1, importance-1), updated_at=?1 WHERE importance>1 AND (length(trim(content)) < 8 OR normalized_content IN ('ok','yes','fine','嗯','好的','谢谢','收到','知道了'))",
      params![now],
    )?;

    conn.execute(
      "UPDATE memories SET importance=MAX(1, importance-1), updated_at=?1 WHERE importance>2 AND (julianday('now') - julianday(last_used_at)) > 45",
      params![now],
    )?;

    conn.execute(
      "DELETE FROM memories WHERE importance<=1 AND (julianday('now') - julianday(last_used_at)) > 180",
      [],
    )?;

    Ok(())
  }

  pub fn search_memories(
    &self,
    query: &str,
    conversation_id: Option<&str>,
    limit: i64,
  ) -> Result<Vec<Memory>> {
    let q = query.trim();
    if q.is_empty() {
      return Ok(Vec::new());
    }

    let conv_id = conversation_id.map(str::trim).filter(|v| !v.is_empty());
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT
        m.id,m.content,m.importance,m.created_at,m.updated_at,m.last_used_at,m.normalized_content,m.scope,m.conversation_id,
        m.source_message_id,m.source_conversation_id,
        (SELECT c.title FROM conversations c WHERE c.id=m.source_conversation_id LIMIT 1) AS source_conversation_title,
        (SELECT substr(msg.content,1,200) FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_preview,
        (SELECT msg.deleted_at FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_deleted_at,
        (SELECT 1 FROM conversations c WHERE c.id=m.source_conversation_id LIMIT 1) AS source_conversation_exists,
        (SELECT 1 FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_exists
      FROM memories m
      WHERE
        m.scope='global'
        OR (
          m.scope='conversation'
          AND m.conversation_id=?1
          AND EXISTS(SELECT 1 FROM conversations c2 WHERE c2.id=m.conversation_id)
        )
      ORDER BY m.importance DESC, m.last_used_at DESC
      LIMIT 800
      "#,
    )?;
    let rows = stmt.query_map(params![conv_id], |r| {
      Ok((
        Memory {
          id: r.get(0)?,
          content: r.get(1)?,
          importance: r.get(2)?,
          created_at: r.get(3)?,
          updated_at: r.get(4)?,
          last_used_at: r.get(5)?,
          scope: r.get::<_, String>(7).unwrap_or_else(|_| "global".to_string()),
          conversation_id: r.get(8).ok(),
          source_message_id: r.get(9).ok(),
          source_conversation_id: r.get(10).ok(),
          source_conversation_title: r.get(11).ok(),
          source_message_preview: r.get(12).ok(),
          source_conversation_deleted: {
            let src: Option<String> = r.get(10).ok();
            let exists: Option<i64> = r.get(14).ok();
            src.map(|v| !v.trim().is_empty()).unwrap_or(false) && exists.unwrap_or(0) != 1
          },
          source_message_deleted: {
            let src: Option<String> = r.get(9).ok();
            let deleted_at: Option<String> = r.get(13).ok();
            let exists: Option<i64> = r.get(15).ok();
            src.map(|v| !v.trim().is_empty()).unwrap_or(false)
              && (exists.unwrap_or(0) != 1
                || deleted_at
                  .as_deref()
                  .map(str::trim)
                  .filter(|v| !v.is_empty())
                  .is_some())
          },
        },
        r.get::<_, String>(6).unwrap_or_default(),
      ))
    })?;

    let mut scored: Vec<(i64, Memory)> = Vec::new();
    let q_norm = normalize_for_match(q);
    for (m, norm) in rows.flatten() {
      let mut score = score_text_overlap(&q_norm, &norm) + score_text_overlap(q, &m.content) + m.importance;
      if m.scope == "conversation" {
        score += 2;
      }
      if m.content.chars().count() > 160 {
        score += 1;
      }
      if score > 0 {
        scored.push((score, m));
      }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<Memory> = scored
      .into_iter()
      .take(limit.max(0) as usize)
      .map(|(_, m)| m)
      .collect();

    let now = now_iso();
    for item in &out {
      let _ = conn.execute(
        "UPDATE memories SET last_used_at=?1, updated_at=?1, importance=MIN(10, importance+1) WHERE id=?2",
        params![now, item.id],
      );
    }

    for item in &mut out {
      item.last_used_at = now.clone();
      item.updated_at = now.clone();
      item.importance = (item.importance + 1).min(10);
    }

    Ok(out)
  }

  pub fn list_memories(&self, limit: i64) -> Result<Vec<Memory>> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT
        m.id,m.content,m.importance,m.created_at,m.updated_at,m.last_used_at,m.scope,m.conversation_id,
        m.source_message_id,m.source_conversation_id,
        (SELECT c.title FROM conversations c WHERE c.id=m.source_conversation_id LIMIT 1) AS source_conversation_title,
        (SELECT substr(msg.content,1,200) FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_preview,
        (SELECT msg.deleted_at FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_deleted_at,
        (SELECT 1 FROM conversations c WHERE c.id=m.source_conversation_id LIMIT 1) AS source_conversation_exists,
        (SELECT 1 FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_exists
      FROM memories m
      WHERE
        m.scope='global'
        OR (
          m.scope='conversation'
          AND EXISTS(SELECT 1 FROM conversations c2 WHERE c2.id=m.conversation_id)
        )
      ORDER BY m.importance DESC, m.last_used_at DESC
      LIMIT ?1
      "#,
    )?;
    let rows = stmt.query_map(params![limit], |r| {
      Ok(Memory {
        id: r.get(0)?,
        content: r.get(1)?,
        importance: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        last_used_at: r.get(5)?,
        scope: r.get::<_, String>(6).unwrap_or_else(|_| "global".to_string()),
        conversation_id: r.get(7).ok(),
        source_message_id: r.get(8).ok(),
        source_conversation_id: r.get(9).ok(),
        source_conversation_title: r.get(10).ok(),
        source_message_preview: r.get(11).ok(),
        source_conversation_deleted: {
          let src: Option<String> = r.get(9).ok();
          let exists: Option<i64> = r.get(13).ok();
          src.map(|v| !v.trim().is_empty()).unwrap_or(false) && exists.unwrap_or(0) != 1
        },
        source_message_deleted: {
          let src: Option<String> = r.get(8).ok();
          let deleted_at: Option<String> = r.get(12).ok();
          let exists: Option<i64> = r.get(14).ok();
          src.map(|v| !v.trim().is_empty()).unwrap_or(false)
            && (exists.unwrap_or(0) != 1
              || deleted_at
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .is_some())
        },
      })
    })?;
    Ok(rows.flatten().collect())
  }

  pub fn list_memories_for_conversation(&self, conversation_id: &str, limit: i64) -> Result<Vec<Memory>> {
    let conv_id = conversation_id.trim();
    if conv_id.is_empty() {
      return self.list_memories(limit);
    }
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
      r#"
      SELECT
        m.id,m.content,m.importance,m.created_at,m.updated_at,m.last_used_at,m.scope,m.conversation_id,
        m.source_message_id,m.source_conversation_id,
        (SELECT c.title FROM conversations c WHERE c.id=m.source_conversation_id LIMIT 1) AS source_conversation_title,
        (SELECT substr(msg.content,1,200) FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_preview,
        (SELECT msg.deleted_at FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_deleted_at,
        (SELECT 1 FROM conversations c WHERE c.id=m.source_conversation_id LIMIT 1) AS source_conversation_exists,
        (SELECT 1 FROM messages msg WHERE msg.id=m.source_message_id LIMIT 1) AS source_message_exists
      FROM memories m
      WHERE
        m.scope='global'
        OR (
          m.scope='conversation'
          AND m.conversation_id=?1
          AND EXISTS(SELECT 1 FROM conversations c2 WHERE c2.id=m.conversation_id)
        )
      ORDER BY m.importance DESC, m.last_used_at DESC
      LIMIT ?2
      "#,
    )?;
    let rows = stmt.query_map(params![conv_id, limit], |r| {
      Ok(Memory {
        id: r.get(0)?,
        content: r.get(1)?,
        importance: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        last_used_at: r.get(5)?,
        scope: r.get::<_, String>(6).unwrap_or_else(|_| "global".to_string()),
        conversation_id: r.get(7).ok(),
        source_message_id: r.get(8).ok(),
        source_conversation_id: r.get(9).ok(),
        source_conversation_title: r.get(10).ok(),
        source_message_preview: r.get(11).ok(),
        source_conversation_deleted: {
          let src: Option<String> = r.get(9).ok();
          let exists: Option<i64> = r.get(13).ok();
          src.map(|v| !v.trim().is_empty()).unwrap_or(false) && exists.unwrap_or(0) != 1
        },
        source_message_deleted: {
          let src: Option<String> = r.get(8).ok();
          let deleted_at: Option<String> = r.get(12).ok();
          let exists: Option<i64> = r.get(14).ok();
          src.map(|v| !v.trim().is_empty()).unwrap_or(false)
            && (exists.unwrap_or(0) != 1
              || deleted_at
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .is_some())
        },
      })
    })?;
    Ok(rows.flatten().collect())
  }

  pub fn set_memory_scope(
    &self,
    memory_id: &str,
    make_global: bool,
    conversation_id: Option<&str>,
  ) -> Result<()> {
    let conn = self.conn.lock();
    let row: Option<(String, String, i64, String, Option<String>)> = conn
      .query_row(
        "SELECT content,normalized_content,importance,scope,conversation_id FROM memories WHERE id=?1 LIMIT 1",
        params![memory_id],
        |r| Ok((r.get(0)?, r.get(1).unwrap_or_default(), r.get(2)?, r.get(3).unwrap_or_else(|_| "global".to_string()), r.get(4).ok())),
      )
      .optional()?;
    let Some((content, normalized_cached, importance, scope_raw, old_conv)) = row else {
      anyhow::bail!("memory not found");
    };

    let current_scope = normalize_memory_scope(&scope_raw);
    let target_scope = if make_global { "global" } else { "conversation" };
    let target_conv = if make_global {
      None
    } else {
      conversation_id
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .or_else(|| old_conv.as_deref().map(str::trim).filter(|x| !x.is_empty()))
    };

    if target_scope == "conversation" && target_conv.is_none() {
      anyhow::bail!("conversation_id required when converting to conversation scope");
    }

    if current_scope == target_scope
      && old_conv.as_deref().map(str::trim).filter(|x| !x.is_empty()) == target_conv
    {
      return Ok(());
    }

    let normalized = if normalized_cached.trim().is_empty() {
      normalize_for_match(&content)
    } else {
      normalized_cached
    };
    let now = now_iso();
    let dup: Option<(String, String, i64)> = conn
      .query_row(
        "SELECT id,content,importance FROM memories WHERE id<>?1 AND normalized_content=?2 AND scope=?3 AND ((conversation_id IS NULL AND ?4 IS NULL) OR conversation_id=?4) LIMIT 1",
        params![memory_id, normalized, target_scope, target_conv],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
      )
      .optional()?;

    if let Some((dup_id, dup_content, dup_importance)) = dup {
      let merged_content = merge_memory_content(&dup_content, &content);
      let mut merged_importance = dup_importance.max(importance).clamp(1, 10);
      if !looks_low_value_memory(&merged_content) {
        merged_importance = (merged_importance + 1).min(10);
      }
      conn.execute(
        "UPDATE memories SET content=?1, normalized_content=?2, importance=?3, updated_at=?4, last_used_at=?4 WHERE id=?5",
        params![
          merged_content,
          normalize_for_match(&merged_content),
          merged_importance,
          now,
          dup_id
        ],
      )?;
      conn.execute("DELETE FROM memories WHERE id=?1", params![memory_id])?;
      return Ok(());
    }

    conn.execute(
      "UPDATE memories SET scope=?1, conversation_id=?2, updated_at=?3, last_used_at=?3 WHERE id=?4",
      params![target_scope, target_conv, now, memory_id],
    )?;
    Ok(())
  }

  pub fn backup_to(&self, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
      fs::create_dir_all(parent)?;
    }
    let temp = target.with_file_name(format!(".tmp_backup_{}.db", Uuid::new_v4()));
    if temp.exists() {
      let _ = fs::remove_file(&temp);
    }
    let conn = self.conn.lock();
    let _ = conn.query_row("PRAGMA wal_checkpoint(FULL)", [], |_| Ok(()));
    let escaped = escaped_sqlite_path(&temp);
    conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))?;
    drop(conn);
    if target.exists() {
      fs::remove_file(target)?;
    }
    if fs::rename(&temp, target).is_err() {
      fs::copy(&temp, target)?;
      fs::remove_file(&temp)?;
    }
    Ok(())
  }

  pub fn restore_from(&self, source: &Path) -> Result<()> {
    if !source.exists() {
      anyhow::bail!("backup file not found");
    }
    let probe = Connection::open(source).context("open backup sqlite failed")?;
    let check: String = probe
      .query_row("PRAGMA integrity_check", [], |r| r.get(0))
      .unwrap_or_else(|_| "failed".to_string());
    if !check.eq_ignore_ascii_case("ok") {
      anyhow::bail!("backup integrity_check failed: {check}");
    }
    drop(probe);

    let wal = self.path.with_extension("db-wal");
    let shm = self.path.with_extension("db-shm");
    {
      let mut conn = self.conn.lock();
      let _ = conn.query_row("PRAGMA wal_checkpoint(FULL)", [], |_| Ok(()));
      let placeholder = Connection::open_in_memory().context("open temp sqlite failed")?;
      let old = std::mem::replace(&mut *conn, placeholder);
      drop(old);
      for p in [&self.path, &wal, &shm] {
        if p.exists() {
          let _ = fs::remove_file(p);
        }
      }
      fs::copy(source, &self.path)?;
      let fresh = Connection::open(&self.path).context("reopen sqlite failed after restore")?;
      apply_runtime_pragmas(&fresh)?;
      let _ = std::mem::replace(&mut *conn, fresh);
    }
    Ok(())
  }
}
