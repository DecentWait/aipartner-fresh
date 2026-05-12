# AIPartner Frontend v1.0

前端基于 React + TypeScript + Vite。
文档同步日期：2026-05-12（与主 README 保持一致）。

## 命令
```bash
npm.cmd install
npm.cmd run dev
npm.cmd run build
```

## 当前职责
- ChatGPT 风格会话 UI。
- 消息流式渲染（content / reasoning_content 分离）。
- 多会话管理（新建/删除/重命名/搜索/置顶/排序）。
- 设置页（provider/model/base_url/api_key、context budget、主题）。
- 每会话独立聊天设置：
  - `provider/model/base_url`
  - `temperature/max_tokens`
  - `max_context_tokens/max_recent_messages/max_memory_items`
  - `system_prompt`（会话固定指令）
  - `thinking_override`（DeepSeek：`enabled/disabled`）
  - `reasoning_effort_override`（DeepSeek：`high/max`）
- 聊天页会话操作：
  - `Apply Preset` 一键套用参数模板（`friend / pgee_sentence / pgee_fullpaper / coding / quickqa`）
  - 预设仅覆盖模型与参数，不内置也不覆盖 `system_prompt` 文本。
- 系统提示词文本不在前端源码硬编码，按数据库中保存的会话/全局配置读取。
- 长期记忆查看与手动 global/conversation 标记。
- 当前会话 `.txt` 附件最小入口：上传、列表、删除。
- 聊天页 UI 折叠优化：
  - 侧边栏平滑折叠/展开（隐藏后聊天区自动扩展）。
  - 顶部工具（侧栏开关/标签切换）与标题信息合并同栏。
  - 顶部信息区支持 `Show Info / Hide Info`。

## 记忆来源显示（最小增强）
- 记忆页显示：scope、importance、created_at、last_used_at。
- 记忆页显示来源信息：
  - 来源会话标题（可解析时）
  - 来源消息片段（可解析时）
- 安全降级：
  - 旧数据无来源：显示“旧版本记忆 / Unknown”
  - 来源会话不存在：显示“来源会话已删除”
  - 来源消息不存在或已删除：显示“来源消息已删除”
- 删除会话不会清空 global memory；被删除会话的 conversation memory 不再注入上下文。
- memory 抽取仅来自正常聊天中的明确记忆意图，不会自动从 system_prompt/txt summary/会话参数写入。

## 每会话设置回退规则（前端行为）
- 会话字段为空时，自动回退全局设置。
- 会话字段有值时，只覆盖当前会话，不影响全局设置和其他会话。
- `api_key` 仍通过全局 provider 管理，不做每会话独立保存。

## DeepSeek 定向参数（当前实现）
- 仅在 provider=DeepSeek 且模型为 `deepseek-v4-pro` / `deepseek-v4-flash` 时，后端会注入：
  - `thinking`（enabled/disabled）
  - `reasoning_effort`（high/max）
- `thinking=enabled` 时不发送 `temperature`。
- 当前未实现工具调用 UI/链路（`tools/tool_calls`），本项目仍以“长期对话伙伴”场景为主。

## txt 附件前端约束
- 只允许 `.txt`。
- 单文件大小上限来自设置页 `TXT Max File Size (KB)`（默认 1MB）。
- 后端硬限制范围 `1KB ~ 32MB`。
- 上传后显示文件名、大小、创建时间、summary。
- 删除后立即从当前会话附件列表移除。
- 不支持 PDF/Word/Excel，不做 embedding/RAG。

## 说明
- 完整功能清单、构建发布与源码使用说明以项目根目录 `README.md` 为准。
- TXT 大小上限修改方案见根目录 `README.md` 的“附录 C：TXT 大小上限修改方案（完整步骤）”。
