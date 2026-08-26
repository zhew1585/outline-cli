# Story 3.3: docs create

Status: done

## Story

As a 记录者,
I want 管道或文件直接建文档,
so that 笔记一条命令入库。

## Acceptance Criteria

1. **Given** `cat notes.md | otl docs create --title "Notes" --collection <id>`
   **When** 执行
   **Then** stdin 作为内容创建文档，输出新文档 id 与 URL
2. **Given** `--file notes.md`
   **When** 执行
   **Then** 文件内容作为文档内容，效果等同 stdin

## Tasks / Subtasks

- [x] Task 1: 正文读取模块 (AC: 1, 2)
  - [x] `commands/docs/content.rs`：`--file` 优先；否则 stdin 非 TTY 时读到尾；stdin 是 TTY 视为"未提供"
  - [x] 8 MiB 上限（元数据先判 + 有界读双保险），超限/非 UTF-8/打不开都是用法错误（退出码 2）
  - [x] 全空白来源统一视为"未提供正文"（见 Dev Notes）
- [x] Task 2: 单文档汇报模块 (AC: 1, 2)
  - [x] `commands/docs/detail.rs`：Table 模式打 id / title / updated / revision / url / status 对齐键值块
  - [x] `render::render_pairs`（纯追加）：值只清控制符、不截断（URL 必须完整）
  - [x] URL 拼不出来只警告不失败——文档已经建好了，报失败会诱导用户重试造重复
- [x] Task 3: 命令实现 (AC: 1, 2)
  - [x] `commands/docs/create.rs`：`documents.create`，`text=`（+ `title` / `collectionId` / `parentDocumentId`）
  - [x] 有 `--collection` 或 `--parent` 时默认 `publish=true`，`--draft` 关闭；两者都没有时不发 `publish` 并提示
- [x] Task 4: 测试 (AC: 1, 2)
  - [x] `request_args` 单测：publish 默认/`--draft`/无目标时参数缺省/无 title 不发空串
  - [x] content 单测：TTY stdin=无正文、文件读取、空白来源=无正文、超限、非 UTF-8、上限边界
  - [x] golden file `tests/golden/docs_detail_pairs.txt`；标题里的 ESC/换行不能伪造额外行
  - [x] `tests/docs_write.rs`：stdin 精确请求体（`publish` 是布尔不是字符串）、`--file` 等价、无正文→2、缺文件→2

## Dev Notes

- **`--file` 优先而非冲突报错**：无法在不读取的情况下判断被重定向的 stdin 是否真有内容，
  而读一个永不关闭的管道会永久阻塞。脚本里 stdin 常常是 `/dev/null`，为用 `--file` 就报错是错误行为。
  因此 `--file` 直接赢，并写进 flag 帮助文本。
- **全空白 = 没有正文**：这条规则同时挡住两个方向的事故：
  `otl docs update <id> --title X` 在脚本里（stdin 是 `/dev/null`）不能被读成"把正文清空"，
  空管道也不该存一篇空白文档。真要清空正文得显式走 `otl api documents.update id=<id> text=`。
- **默认发布是刻意偏离 API 默认值**：Outline 的 `publish` 默认 false，即建成草稿，
  草稿对工作区其他人不可见。AC 的场景是"笔记一条命令入库"，草稿不算入库。
  所以给了目标（collection/parent）就默认发布，`--draft` 保留原语义；
  没给目标时 Outline 根本无法发布，于是不发该参数并在 stderr 明说是草稿。
  输出里始终有一行 `status published|draft`，不让用户猜。
- **`--collection` 必须是 UUID**：vendored spec 把 `collectionId` 标为 `format: uuid`，
  engine 在发请求前本地拒绝非 UUID（退出码 2）。这与服务端行为一致，且属于 fail-fast。

### References

- [Source: planning/epics.md#Story 3.3]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-2 success（`cat notes.md | otl docs create --title X`）]
- [Source: project-context.md「所有请求经唯一通道」「库层禁 unwrap」]

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-26

### Completion Notes List

- `MAX_CONTENT_BYTES` 与 `otl api` 的 `--body` 上限同量级（8 MiB）。服务端另有更小的 `maxLength`（1_536_000），
  但那是 spec 里的 facet，未编进 IR，所以本地上限只负责"别把巨型/无尽输入变成 OOM"。
- 非 TTY 且非空白的 stdin 才算正文，因此 assert_cmd 下 `.write_stdin("")` 能稳定测到"无正文"分支。

### File List

- crates/otl/src/commands/docs/{content.rs, create.rs, detail.rs}
- crates/otl/src/render.rs（追加 `render_pairs` + `scrub_control_chars`）
- crates/otl/tests/docs_write.rs
- crates/otl/tests/golden/docs_detail_pairs.txt
