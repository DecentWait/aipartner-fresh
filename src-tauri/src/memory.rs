use anyhow::Result;

use crate::{db::Database, models::Message};

const SUMMARY_KEEP_RECENT: usize = 18;
const SUMMARY_SCAN_LIMIT: i64 = 6000;
const SUMMARY_SEGMENT_SIZE: usize = 18;
const SUMMARY_MIN_SEGMENT: usize = 8;

fn split_lines(text: &str) -> Vec<String> {
  text
    .split(['\n', '。', '；', '，', ';', '.', '!', '?', '！', '？'])
    .map(str::trim)
    .filter(|x| x.chars().count() >= 10)
    .map(str::to_string)
    .collect()
}

fn contains_any(text: &str, keys: &[&str]) -> bool {
  keys.iter().any(|k| text.contains(k))
}

fn clip_chars(input: &str, max_chars: usize) -> String {
  if input.chars().count() <= max_chars {
    return input.to_string();
  }
  input.chars().take(max_chars).collect()
}

fn flatten_spaces(input: &str) -> String {
  input
    .replace('\r', " ")
    .replace('\n', " ")
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

fn add_unique_line(base: &str, line: &str, max_lines: usize) -> String {
  let clean = flatten_spaces(line.trim());
  if clean.is_empty() {
    return base.to_string();
  }
  let mut items: Vec<String> = base
    .split('\n')
    .map(str::trim)
    .filter(|x| !x.is_empty())
    .map(str::to_string)
    .collect();

  if items.iter().any(|x| x == &clean) {
    return items.join("\n");
  }

  items.push(clean);
  if items.len() > max_lines {
    let start = items.len() - max_lines;
    items = items[start..].to_vec();
  }
  items.join("\n")
}

fn detect_language_style(text: &str) -> &'static str {
  let mut zh = 0usize;
  let mut en = 0usize;
  for ch in text.chars() {
    if ch.is_ascii_alphabetic() {
      en += 1;
    } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
      zh += 1;
    }
  }
  if zh > en * 2 {
    "zh"
  } else if en > zh * 2 {
    "en"
  } else {
    "mixed"
  }
}

fn try_extract_after(text: &str, key: &str) -> Option<String> {
  let idx = text.find(key)?;
  let rest = text[(idx + key.len())..].trim();
  if rest.is_empty() {
    return None;
  }
  let cut = rest
    .split([',', '。', '，', '.', '!', '！', '?', '？', '\n', ' '])
    .next()
    .unwrap_or("")
    .trim();
  if cut.chars().count() < 1 {
    None
  } else {
    Some(clip_chars(cut, 24))
  }
}

fn should_store_as_global_memory(line: &str, lower: &str) -> bool {
  contains_any(
    line,
    &[
      "全局",
      "跨会话",
      "所有会话",
      "所有对话",
      "每个会话",
      "每个对话",
      "默认都按这个",
      "以后都按这个",
      "永久记住",
    ],
  ) || contains_any(
    lower,
    &[
      "global",
      "across chats",
      "across conversations",
      "in every chat",
      "remember this globally",
      "permanently remember",
    ],
  )
}

pub fn maybe_extract_memories(
  db: &Database,
  conversation_id: Option<&str>,
  source_message_id: Option<&str>,
  user_text: &str,
) -> Result<()> {
  let text = user_text.trim();
  if text.chars().count() < 10 {
    return Ok(());
  }

  let lower = text.to_lowercase();
  let should_extract = contains_any(
    text,
    &[
      "记住",
      "长期",
      "一直",
      "习惯",
      "偏好",
      "目标",
      "计划",
      "以后",
      "事实",
      "重要信息",
      "请记住",
      "以后都这样",
      "长期偏好",
      "长期目标",
      "永久",
      "全局",
      "跨会话",
      "所有会话",
      "所有对话",
      "每个会话",
      "每个对话",
      "默认按这个",
      "以后都按这个",
      "总是按这个",
      "记下来",
    ],
  ) || contains_any(
    &lower,
    &[
      "remember",
      "long-term",
      "habit",
      "preference",
      "goal",
      "plan",
      "always",
      "important",
      "fact",
      "remember this",
      "from now on",
      "keep this in mind",
      "please remember",
    ],
  );

  if !should_extract {
    return Ok(());
  }

  let lines = split_lines(text);
  if lines.is_empty() {
    return Ok(());
  }

  for line in lines.into_iter().take(8) {
    let line_lower = line.to_lowercase();
    let importance = if contains_any(&line, &["最重要", "必须", "永远"]) || line_lower.contains("must") {
      9
    } else if contains_any(&line, &["长期", "一直", "持续"]) || line_lower.contains("long-term") {
      8
    } else if contains_any(&line, &["目标", "计划"]) || line_lower.contains("goal") || line_lower.contains("plan") {
      7
    } else {
      6
    };

    if should_store_as_global_memory(&line, &line_lower) {
      db.upsert_memory(
        &line,
        importance,
        "global",
        None,
        source_message_id,
        conversation_id,
      )?;
    } else {
      db.upsert_memory(
        &line,
        importance,
        "conversation",
        conversation_id,
        source_message_id,
        conversation_id,
      )?;
    }
  }

  db.decay_memories()?;
  Ok(())
}

pub fn update_profiles_from_user_text(db: &Database, user_text: &str) -> Result<()> {
  let raw = user_text.trim();
  if raw.is_empty() {
    return Ok(());
  }
  let text = flatten_spaces(raw);
  if text.is_empty() {
    return Ok(());
  }
  let lower = text.to_lowercase();

  let mut user = db.get_user_profile();
  let mut style = db.get_style_profile();
  let mut user_changed = false;
  let mut style_changed = false;

  if let Some(name) = try_extract_after(&text, "叫我")
    .or_else(|| try_extract_after(&text, "你可以叫我"))
    .or_else(|| try_extract_after(&lower, "call me "))
    .or_else(|| try_extract_after(&lower, "my name is "))
  {
    if user.preferred_name != name {
      user.preferred_name = name;
      user_changed = true;
    }
  }

  if contains_any(&text, &["目标", "计划", "想要", "我要", "希望", "以后", "长期"])
    || contains_any(&lower, &["goal", "plan", "roadmap", "i want", "i need"])
  {
    let v = add_unique_line(&user.long_term_goals, &clip_chars(&text, 220), 24);
    if v != user.long_term_goals {
      user.long_term_goals = v;
      user_changed = true;
    }
  }

  if contains_any(&text, &["喜欢", "兴趣", "爱好", "关注"])
    || contains_any(&lower, &["interest", "hobby", "love", "enjoy"])
  {
    let v = add_unique_line(&user.interests, &clip_chars(&text, 220), 24);
    if v != user.interests {
      user.interests = v;
      user_changed = true;
    }
  }

  if contains_any(&text, &["经历", "最近", "今天", "昨天", "上周", "刚刚", "发生"])
    || contains_any(&lower, &["experience", "recently", "today", "yesterday", "happened"])
  {
    let v = add_unique_line(&user.important_experiences, &clip_chars(&text, 260), 30);
    if v != user.important_experiences {
      user.important_experiences = v;
      user_changed = true;
    }
  }

  if contains_any(&text, &["用中文", "中文回答", "中文为主"]) {
    if user.language_preference != "zh" {
      user.language_preference = "zh".to_string();
      user_changed = true;
    }
  }
  if contains_any(&text, &["用英文", "英文回答", "英语回答"])
    || contains_any(&lower, &["answer in english", "use english"])
  {
    if user.language_preference != "en" {
      user.language_preference = "en".to_string();
      user_changed = true;
    }
  }

  if contains_any(&text, &["请你", "希望你", "以后", "回复时", "别", "不要"])
    || contains_any(&lower, &["please", "from now on", "reply", "do not"])
  {
    let v = add_unique_line(&user.notes, &clip_chars(&text, 260), 32);
    if v != user.notes {
      user.notes = v;
      user_changed = true;
    }
    let sp = add_unique_line(&style.explicit_preferences, &clip_chars(&text, 260), 32);
    if sp != style.explicit_preferences {
      style.explicit_preferences = sp;
      style_changed = true;
    }
  }

  if contains_any(&text, &["简洁", "简短", "短一点", "直接给结论"])
    || contains_any(&lower, &["concise", "brief", "short answer"])
  {
    if style.detail_level != "concise" {
      style.detail_level = "concise".to_string();
      style_changed = true;
    }
  }
  if contains_any(&text, &["详细", "展开", "多讲点", "详细一点"])
    || contains_any(&lower, &["detailed", "step by step", "more detail"])
  {
    if style.detail_level != "detailed" {
      style.detail_level = "detailed".to_string();
      style_changed = true;
    }
  }

  if contains_any(&text, &["直接", "别客套", "直说", "不用安慰"])
    || contains_any(&lower, &["direct", "be blunt", "straight to point"])
  {
    if style.tone != "direct" {
      style.tone = "direct".to_string();
      style_changed = true;
    }
  }
  if contains_any(&text, &["温和", "友好", "鼓励", "耐心"])
    || contains_any(&lower, &["gentle", "friendly", "encourage", "patient"])
  {
    if style.tone != "gentle" {
      style.tone = "gentle".to_string();
      style_changed = true;
    }
  }

  if contains_any(&text, &["技术细节", "术语", "代码", "专业点"])
    || contains_any(&lower, &["technical", "terminology", "code-level", "professional"])
  {
    if style.technical_level != "technical" {
      style.technical_level = "technical".to_string();
      style_changed = true;
    }
  }
  if contains_any(&text, &["通俗", "简单点", "小白", "听不懂"])
    || contains_any(&lower, &["plain language", "simple", "easy to understand"])
  {
    if style.technical_level != "plain" {
      style.technical_level = "plain".to_string();
      style_changed = true;
    }
  }

  let lang_style = detect_language_style(&text).to_string();
  if style.language_style != lang_style {
    style.language_style = lang_style;
    style_changed = true;
  }

  if user_changed {
    db.save_user_profile(&user)?;
  }
  if style_changed {
    db.save_style_profile(&style)?;
  }
  Ok(())
}

fn first_sentence(input: &str, max_chars: usize) -> String {
  let clean = flatten_spaces(input);
  if clean.is_empty() {
    return String::new();
  }
  let mut out = String::new();
  for ch in clean.chars() {
    out.push(ch);
    if matches!(ch, '。' | '！' | '？' | '.' | '!' | '?') {
      break;
    }
    if out.chars().count() >= max_chars {
      break;
    }
  }
  clip_chars(out.trim(), max_chars)
}

fn summary_line(role: &str, content: &str) -> Option<String> {
  let body = first_sentence(content, 170);
  if body.chars().count() < 8 {
    return None;
  }
  match role {
    "user" => Some(format!("- User: {body}")),
    "assistant" => Some(format!("- Assistant: {body}")),
    _ => None,
  }
}

fn build_summary(items: &[Message]) -> String {
  let mut lines: Vec<String> = Vec::new();
  for m in items {
    if let Some(line) = summary_line(&m.role, &m.content) {
      if !lines.iter().any(|x| x == &line) {
        lines.push(line);
      }
    }
    if lines.len() >= 16 {
      break;
    }
  }

  if lines.is_empty() {
    return String::new();
  }

  let mut out = String::from("Earlier conversation summary segment:\n");
  out.push_str(&lines.join("\n"));
  clip_chars(&out, 3200)
}

pub fn maybe_update_summary(db: &Database, conversation_id: &str) -> Result<()> {
  let marker = db.latest_summary_end_created_at(conversation_id)?;
  let pending = db.list_messages_after(conversation_id, marker.as_deref(), SUMMARY_SCAN_LIMIT)?;
  if pending.len() <= SUMMARY_KEEP_RECENT + 2 {
    return Ok(());
  }

  let summarize_len = pending.len().saturating_sub(SUMMARY_KEEP_RECENT);
  if summarize_len < SUMMARY_MIN_SEGMENT {
    return Ok(());
  }

  let mut start = 0usize;
  while summarize_len.saturating_sub(start) >= SUMMARY_MIN_SEGMENT {
    let remain = summarize_len - start;
    let take = remain.min(SUMMARY_SEGMENT_SIZE);
    let slice = &pending[start..start + take];
    let summary = build_summary(slice);
    if summary.trim().is_empty() {
      break;
    }

    if let (Some(first), Some(last)) = (slice.first(), slice.last()) {
      db.append_summary_segment(
        conversation_id,
        &summary,
        &first.id,
        &last.id,
        &first.created_at,
        &last.created_at,
        slice.len() as i64,
      )?;
    }

    start += take;
  }
  Ok(())
}
