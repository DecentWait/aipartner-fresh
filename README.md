# AIPartner v1.0 Stable

AIPartner 是本地优先的 Windows 长期 AI 伙伴，支持长期记忆、分段摘要、上下文预算、每会话模型设置、流式输出和 TXT 附件。
Local-first Windows desktop AI partner with long-term memory, segmented summaries, context budgeting, per-chat model settings, streaming responses, and TXT attachment support.
文档同步日期：2026-05-12（已覆盖当前实现功能）。

## 技术栈
- Desktop: Tauri 2
- Backend: Rust
- Frontend: React + TypeScript + Vite
- Database: SQLite (rusqlite bundled)
- Rendering: Markdown + GFM + KaTeX + Highlight.js

## 数据目录（与 exe 同级）

```text
AIPartner/
├── AIPartner.exe
├── data/
│   ├── AIPartner.db
│   ├── files/
│   ├── backups/
│   ├── exports/
│   └── logs/
├── config/
│   └── settings.json
└── cache/
```

- 不写 AppData / 注册表。
- 直接备份整个 `AIPartner/` 可迁移到另一台电脑继续使用。

## 核心能力（当前实现）
- SQLite 迁移系统：`schema_migrations` + 启动自动迁移 + 迁移前自动备份。
- 多会话聊天：新建、重命名、删除、搜索、置顶、排序、每会话独立模型设置。
- 流式输出：`content` 真流式；`reasoning_content` 有则流式显示、无则兼容。
- 单条消息软删除：`deleted_at/deleted_reason`，删除后不再进入上下文/摘要流程。
- 长期机制：`memories`（global + conversation）、`user_profile`、`style_profile` 持续更新。
- 分段摘要：`summaries` 分段压缩旧聊天，长会话不塞全历史。
- Context Budget：`max_context_tokens / max_recent_messages / max_memory_items`。
- 每会话固定指令与完整参数覆盖：`system_prompt + provider/model/base_url/temperature/max_tokens/max_context_tokens/max_recent_messages/max_memory_items`。
- DeepSeek 定向优化（最小实现）：
  - 每会话 `thinking` 覆盖（`enabled/disabled`）
  - 每会话 `reasoning_effort` 覆盖（`high/max`）
  - 仅对 `deepseek-v4-pro/deepseek-v4-flash` 生效，其他 provider 不受影响。

## 最小 txt 上传功能（会话附件）

只支持 `.txt`，不支持 PDF/Word/Excel/图片，不做 embedding/RAG。

- 上传入口：聊天页内 `Upload .txt`。
- 作用域：附件只绑定当前 conversation。
- 存储路径：`data/files/`。
- 文件限制：单文件最大 1MB，空文件拒绝。
- 文件名安全：后端会清理文件名并生成唯一存储名，防路径穿越。
- 删除：支持当前会话附件删除（软删除 `deleted_at`），删除后不再进入上下文。

### txt 与上下文注入规则
- 默认只注入当前会话 txt 的 `summary`，不默认注入全文。
- txt summary 受 `max_context_tokens` 预算限制。
- 只注入当前会话附件，不跨会话注入。
- 仅当用户明确要求“按 txt 原文/全文/引用文件内容”时，才临时注入原文片段，且仍受 context budget 限制。
- txt 内容和 txt summary 不会自动写入 `memories` 表。
- 只有用户在正常聊天里明确提出“记住某信息”等意图时，才会走现有记忆抽取流程。

## 模型配置
- 全局配置：`provider / model / base_url / api_key`。
- provider 支持：`deepseek / openai / openrouter / ollama / custom`。
- 支持每会话覆盖：
  - `provider`
  - `model`
  - `base_url`
  - `temperature`
  - `max_tokens`
  - `max_context_tokens`
  - `max_recent_messages`
  - `max_memory_items`
  - `system_prompt`
  - `thinking_override`（DeepSeek：`enabled/disabled`）
  - `reasoning_effort_override`（DeepSeek：`high/max`）
- 配置保存在 SQLite + `config/settings.json`，重启自动恢复。

### DeepSeek thinking / reasoning_effort（当前实现）

- 会话设置可保存：
  - `thinking_override`：`enabled` / `disabled` / 空（空=回退默认）
  - `reasoning_effort_override`：`high` / `max` / 空（空=回退默认）
- 默认回退：
  - `thinking` 默认 `enabled`
  - `reasoning_effort` 默认 `high`
- 生效范围：
  - 仅在 provider=DeepSeek 且模型为 `deepseek-v4-pro` / `deepseek-v4-flash` 时注入请求。
  - 其他模型/提供方不注入这两个参数。
- 兼容处理：
  - 当 `thinking=enabled` 时，不发送 `temperature`（避免“设置但不生效”的混淆）。
- 当前边界：
  - 已支持 `reasoning_content` 流式展示与保存。
  - 尚未实现工具调用链路（`tools/tool_calls/role=tool`），因此未启用“工具调用场景下必须回传 reasoning_content”的完整协议流转。

## 记忆层规则（global / conversation / 来源）

- `memories` 分为两层：
  - `global`：跨会话可用，删除来源会话后仍保留。
  - `conversation`：仅在对应会话中注入。
- 删除会话后：
  - 不删除 `global` memory。
  - 被删除会话的 `conversation` memory 不再进入任何会话上下文。
  - 其他会话与其他会话记忆不受影响。
- 记忆来源字段：
  - `source_conversation_id`
  - `source_message_id`
- 记忆页会显示：
  - scope、importance、created_at、last_used_at
  - 来源会话标题（可解析时）
  - 来源消息片段（可解析时）
- 降级显示：
  - 旧数据无来源：显示“旧版本记忆 / Unknown”
  - 来源会话不存在：显示“来源会话已删除”
  - 来源消息不存在或已删除：显示“来源消息已删除”
- `system_prompt`、txt 内容/summary、每会话参数不会自动写入 `memories`。
- 仅在正常聊天中触发“记住/长期偏好/目标/事实”等明确记忆意图时才抽取 memory。

## 每会话固定指令与参数回退规则

- 每个会话可保存自己的固定指令 `system_prompt`（会话规则）。
- `system_prompt` 只作用于当前会话，保存后立即生效，清空后立即失效。
- `system_prompt` 不会自动写入 `memories`，也不会写入 `summaries`。
- `system_prompt` 与 txt 附件 summary 分开管理，不互相覆盖。
- 发送消息时会合并“会话设置 + 全局设置”，字段为空时自动回退全局：
  - `conversation.provider` 为空 -> 用全局 `provider`
  - `conversation.model` 为空 -> 用全局 `model`
  - `conversation.base_url` 为空 -> 用全局 `base_url`
  - `conversation.temperature` 为空 -> 用全局 `temperature`
  - `conversation.max_tokens` 为空 -> 用全局 `max_tokens`
  - `conversation.max_context_tokens` 为空 -> 用全局 `max_context_tokens`
  - `conversation.max_recent_messages` 为空 -> 用全局 `max_recent_messages`
  - `conversation.max_memory_items` 为空 -> 用全局 `max_memory_items`
  - `conversation.system_prompt` 为空 -> 不注入会话固定指令
  - `conversation.thinking_override` 为空 -> DeepSeek 使用默认 `enabled`
  - `conversation.reasoning_effort_override` 为空 -> DeepSeek 使用默认 `high`
- `api_key` 继续使用全局 provider 配置，不做每会话独立 key。

### 上下文注入优先级（当前实现）

1. 当前会话 `system_prompt`（若设置）
2. 核心 persona / 基础系统规则
3. `user_profile` + `style_profile`
4. 当前会话 txt summary（或按需原文片段）
5. 相关长期记忆（global + conversation）
6. 分段 summaries
7. 最近消息 + 当前输入（在预算不足时优先保留当前输入和最近消息）

### 推荐会话参数示例

PGEE-I 句子训练会话：
- `system_prompt` = EC/CE Sentence Trainer 规则
- `max_tokens` = 2048
- `max_context_tokens` = 12000
- `max_recent_messages` = 20
- `max_memory_items` = 5

PGEE-I 整卷训练会话：
- `system_prompt` = Full-Paper Real-Exam Trainer 规则
- `max_tokens` = 4096（必要时 8192）
- `max_context_tokens` = 24000（必要时 32000）
- `max_recent_messages` = 20
- `max_memory_items` = 3

代码开发会话：
- `temperature` = 0.3
- `max_tokens` = 4096
- `max_context_tokens` = 24000
- `max_recent_messages` = 25
- `max_memory_items` = 6

## 开发运行
```bash
cd frontend
npm.cmd install

cd ../src-tauri
cargo tauri dev
```

## 构建
```bash
cd frontend
npm.cmd run build

cd ../src-tauri
cargo check
cargo tauri build
```

## 产物路径
- `src-tauri/target/release/aipartner.exe`
- `src-tauri/target/release/bundle/nsis/AIPartner_1.0.0_x64-setup.exe`
- `src-tauri/target/release/bundle/msi/AIPartner_1.0.0_x64_en-US.msi`

## 附录 A：AIPartner v1.0 当前工程已实现功能（完整清单）

1. 桌面与本地化
- Tauri 2 桌面应用，可打包 Windows 可执行程序。
- 所有数据保存到 exe 同级目录。
- 启动自动创建 `data/files/backups/exports/logs`、`config`、`cache`。

2. SQLite 与迁移
- 本地数据库：`data/AIPartner.db`。
- `schema_migrations` 版本迁移管理。
- 启动自动迁移，迁移前自动备份。
- `ensure_column` 幂等，重复启动不会重复改表。
- 兼容旧库升级，保留历史数据。
- 已含 `conversation_files`（txt 附件）表迁移。

3. 会话管理
- 新建、重命名、删除会话。
- 会话搜索。
- 会话置顶/取消置顶。
- 会话上移/下移排序。
- 启动恢复上次会话。

4. 消息能力
- 发送消息、停止生成。
- 编辑用户消息并重生回答。
- 从指定用户消息重新回答。
- 单条消息软删除（`deleted_at/deleted_reason`）。
- 消息复制。

5. 流式输出与 reasoning
- `content` 真流式实时显示。
- `reasoning_content` 有则实时显示，无则兼容不报错。
- `content/reasoning_content` 分开展示。
- 流式过程不按 token 写库，结束一次性落库。
- 停止生成时保留已生成内容。
- 不支持 stream 的接口自动降级非流式返回。

6. 模型配置
- 全局配置：`provider/model/base_url/api_key/temperature/max_tokens`。
- provider：`deepseek/openai/openrouter/ollama/custom`。
- 每会话独立覆盖：`provider/model/base_url/temperature/max_tokens/max_context_tokens/max_recent_messages/max_memory_items/system_prompt/thinking_override/reasoning_effort_override`。
- 保存后立即生效，重启自动恢复（SQLite + `config/settings.json`）。

7. 长期记忆与画像
- `memories` 双层：`global` + `conversation`。
- 自动记忆抽取、去重、合并、importance 动态调整、衰减清理。
- `last_used_at` 命中自动更新。
- 记忆保存来源：`source_conversation_id/source_message_id`。
- 删除会话不影响 `global` memory；被删除会话的 `conversation` memory 不再进入上下文注入。
- `user_profile/style_profile` 持续更新并参与后续提示词。

8. 分段摘要与省 token
- `summaries` 分段压缩旧聊天，保留最近消息。
- Context Budget：`max_context_tokens/max_recent_messages/max_memory_items`。
- 长会话不塞完整历史。

9. txt 会话附件（最小可用）
- 仅支持 `.txt` 上传（不支持 PDF/Word/Excel/图片）。
- 单文件 1MB 限制，空文件拒绝。
- 文件落盘：`data/files/`，文件名安全清理与唯一化。
- 附件绑定当前 conversation，不跨会话注入。
- 默认只注入 txt `summary` 参与上下文。
- 用户明确要求原文时，临时注入原文片段（仍受 context budget 限制）。
- 删除附件后（软删除）不再进入上下文。
- txt 内容/summary 默认不自动写入 `memories`。

10. 备份与恢复
- 手动创建备份。
- 从备份恢复（恢复前自动安全备份）。
- 恢复流程包含 SQLite 完整性检查。

## 附录 B：源码使用说明（开发 / 构建 / 发布）

1. 环境要求
- Windows 10/11
- Node.js 18+
- Rust stable（含 cargo）
- Tauri 2 构建依赖（WebView2 / VS Build Tools）

2. 首次安装依赖
```bash
cd frontend
npm.cmd install
```

3. 开发模式运行
```bash
cd src-tauri
cargo tauri dev
```

4. 构建前检查（建议顺序）
```bash
cd src-tauri
cargo check

cd ../frontend
npm.cmd run build
```

5. 产物构建
```bash
cd src-tauri
cargo tauri build
```

6. 产物位置
- `src-tauri/target/release/aipartner.exe`
- `src-tauri/target/release/bundle/nsis/AIPartner_1.0.0_x64-setup.exe`
- `src-tauri/target/release/bundle/msi/AIPartner_1.0.0_x64_en-US.msi`

7. 使用建议
- 推荐优先使用 `nsis` 安装包。
- 若直接拷贝 `aipartner.exe` 独立运行，请保持 `exe` 旁边目录可写，以便创建 `data/config/cache`。
- 备份时直接复制整个 `AIPartner/` 文件夹。

## 附录 C：TXT 大小上限修改方案（完整步骤）

当前默认上限为 1MB（`1_048_576` bytes）。  
如需改为 2MB/4MB 等，请按下面步骤同步修改。

1. 修改前端上传校验常量
- 文件：`frontend/src/App.tsx`
- 常量：`MAX_TXT_FILE_BYTES`
- 默认值：`1_048_576`
- 示例：
  - 2MB：`2_097_152`
  - 4MB：`4_194_304`

2. 修改后端硬校验常量
- 文件：`src-tauri/src/lib.rs`
- 常量：`MAX_TXT_FILE_BYTES`
- 默认值：`1_048_576`
- 注意：必须与前端保持同值，避免前后端限制不一致。

3. 同步用户提示文案（建议）
- 文件：`frontend/src/App.tsx`
  - 错误提示：`TXT file exceeds 1MB limit.`
- 文件：`src-tauri/src/lib.rs`
  - 错误提示：`txt file exceeds 1MB limit`
  - 错误提示：`txt content exceeds 1MB limit after decoding`
- 把 `1MB` 同步改成新上限（如 `2MB`），避免提示和实际限制不一致。

4. 同步文档描述（建议）
- 文件：`README.md`
  - “最小 txt 上传功能（会话附件）”
  - “附录 A：txt 会话附件（最小可用）”
- 文件：`frontend/README.md`
  - “txt 附件前端约束”

5. 修改后验证（建议顺序）
```bash
cd src-tauri
cargo check

cd ../frontend
npm.cmd run build

cd ../src-tauri
cargo tauri build
```

6. 验收要点
- 小于上限的 `.txt` 可以上传并正常显示。
- 大于上限的 `.txt` 会被明确拒绝并提示。
- 上传后会写入 `data/files/`，并记录到 `conversation_files`。
- 删除附件后不再进入上下文注入。
