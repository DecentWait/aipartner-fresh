use anyhow::Result;
use serde_json::Value;

use crate::{db::Database, models::ImportResult};

pub fn import_conversations_json(db: &Database, file_path: &str) -> Result<ImportResult> {
  let raw = std::fs::read_to_string(file_path)?;
  let root: Value = serde_json::from_str(&raw)?;
  let Some(conversations) = root.as_array() else {
    anyhow::bail!("invalid conversations.json: root must be array");
  };

  let mut imported_conversations = 0usize;
  let mut imported_messages = 0usize;

  for conv in conversations {
    let Some(conv_obj) = conv.as_object() else {
      continue;
    };
    let title = conv_obj
      .get("title")
      .and_then(Value::as_str)
      .map(str::trim)
      .filter(|x| !x.is_empty())
      .unwrap_or("Imported Chat");
    let conv_row = db.create_conversation(Some(title))?;
    imported_conversations += 1;

    let updated_fallback = conv_obj
      .get("updated_at")
      .and_then(Value::as_str)
      .unwrap_or("1970-01-01T00:00:00+00:00");

    let mut seq = 0usize;
    let mut all_items: Vec<(String, usize, String, String, String)> = Vec::new();
    if let Some(mapping) = conv_obj.get("mapping").and_then(Value::as_object) {
      for (_node_id, node) in mapping {
        let Some(node_obj) = node.as_object() else {
          continue;
        };
        let Some(message_obj) = node_obj.get("message").and_then(Value::as_object) else {
          continue;
        };
        let inserted_at = message_obj
          .get("inserted_at")
          .and_then(Value::as_str)
          .unwrap_or(updated_fallback)
          .to_string();
        let Some(fragments) = message_obj.get("fragments").and_then(Value::as_array) else {
          continue;
        };
        let mut think_buf = String::new();
        for frag in fragments {
          let Some(fobj) = frag.as_object() else {
            continue;
          };
          let ftype = fobj
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase();
          let content = fobj
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
          if content.is_empty() {
            continue;
          }
          match ftype.as_str() {
            "REQUEST" => {
              seq += 1;
              all_items.push((
                inserted_at.clone(),
                seq,
                "user".to_string(),
                content,
                String::new(),
              ));
            }
            "THINK" => {
              if !think_buf.is_empty() {
                think_buf.push('\n');
              }
              think_buf.push_str(&content);
            }
            "RESPONSE" => {
              seq += 1;
              all_items.push((
                inserted_at.clone(),
                seq,
                "assistant".to_string(),
                content,
                think_buf.clone(),
              ));
              think_buf.clear();
            }
            _ => {}
          }
        }
      }
    }

    all_items.sort_by(|a, b| {
      if a.0 == b.0 {
        a.1.cmp(&b.1)
      } else {
        a.0.cmp(&b.0)
      }
    });

    for (ts, _seq, role, content, reasoning) in all_items {
      db.insert_imported_message(&conv_row.id, &role, &content, &reasoning, &ts)?;
      imported_messages += 1;
    }
  }

  Ok(ImportResult {
    imported_conversations,
    imported_messages,
  })
}
