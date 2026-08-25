# Story 3.1: docs search

Status: done

## Story

As a 终端工作者,
I want `otl docs search <query>` 快速搜到文档,
so that 不用切浏览器。

## Acceptance Criteria

1. **Given** 已认证
   **When** 执行 `otl docs search 部署`
   **Then** 可读输出结果列表：标题、所属 collection、更新时间、匹配上下文片段
2. **Given** `--json`
   **When** 同一搜索
   **Then** 原始 JSON 输出含文档 id 供脚本消费

## Tasks / Subtasks

- [x] Task 1: 共享会话层 (AC: 1, 2)
  - [x] `crates/otl/src/session.rs`：`Session::open()` 读 `Config::from_env()` + `Client::new`，`call` / `call_data` / `call_rows`
  - [x] `call_rows` 复用 `paging::spec_for` + `Client::execute_paged`，截断与 offset 未确认诊断走 stderr
  - [x] `otl api` 改用同一份 `warn_truncated` / `UNCONFIRMED_OFFSET_NOTICE`（消除重复实现）
- [x] Task 2: schema 驱动的列选择 (AC: 1)
  - [x] `crates/otl/src/fields.rs`：`Column { header, pointer, format }` + `rows()`，值经 RFC 6901 pointer 取出
  - [x] `render::render_columns`（纯追加）复用既有 layout / sanitize / 截断
  - [x] 时间戳统一缩短为 `YYYY-MM-DD HH:MM UTC`，非该形状原样透出
- [x] Task 3: 命令实现 (AC: 1, 2)
  - [x] `commands/docs/search.rs`：`documents.search`，`query=`（+ `--collection` → `collectionId=`）
  - [x] Table 模式四列 TITLE / COLLECTION / UPDATED / MATCH
  - [x] collection id → name 用一次 `collections.list`（自动分页），失败降级为显示 id + stderr 警告
  - [x] `--json` 输出合并后的原始 hit 数组（含 `document.id`）
- [x] Task 4: 测试 (AC: 1, 2)
  - [x] golden file `tests/golden/docs_search_table.txt`（含 CJK/多行片段/未解析 id 分支）
  - [x] `tests/docs_search.rs`：`--json` 含 id、非 TTY 无 ANSI、请求体含 query/collectionId、跨页合并、`--limit` 截断警告、401→4、缺配置→2

## Dev Notes

- **精选命令不得手写渲染**：所有表格都由 `fields::Column` 声明列 + `render::render_columns` 布局，
  新命令只增加列表，不增加渲染分支。
- **`--collection` 用 deprecated 的 `collectionId`**：vendored spec 推荐 `filters`（DocumentFilter 数组），
  但数组是复杂类型，`key=value` 表达不了（engine 会报 ComplexParam）。标量路径只有 `collectionId`；
  需要结构化过滤的用户走 `otl api documents.search --body @filter.json`。
- **collection 名称解析的取舍**：搜索结果只带 `collectionId`，终端里不可读。多花一次 list 请求换可读性，
  仅在 Table 模式（交互场景）执行；`--json` 是原始数据，不做解析也不注入合成字段。
  该请求失败只警告不失败——搜索本身已经成功了。
- **表格只在 TTY 出现**，因此 golden file 测试放在模块内单测（直接调 `table()`），
  端到端测试覆盖 JSON 契约与网络行为。

### References

- [Source: planning/epics.md#Story 3.1]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-2 CAP-5 CAP-6]
- [Source: project-context.md「不要给精选命令手写每端点渲染代码」「所有请求经唯一通道」]

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-26

### Completion Notes List

- `Column` 的 COLLECTION 列 pointer 故意指向不存在的 `/-collection-name`：值由命令在取出行后覆写，
  保持 `fields::rows` 通用（不引入 per-command 解析回调）。
- `fields::text_at` 对容器/`null`/缺失一律返回空串：表格单元格是标量位。

### File List

- crates/otl/src/{session.rs, fields.rs}
- crates/otl/src/render.rs（追加 `render_columns`）
- crates/otl/src/commands/docs/search.rs
- crates/otl/src/commands/api.rs（改用共享诊断）
- crates/otl/tests/{docs_search.rs, common/mod.rs}
- crates/otl/tests/golden/docs_search_table.txt
