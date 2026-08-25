# Story 3.4: docs update

Status: done

## Story

As a 维护者,
I want 命令行更新文档,
so that 修订不进网页编辑器。

## Acceptance Criteria

1. **Given** 已有文档 id
   **When** `otl docs update <id> --title 新标题` 或经 stdin 提供新内容
   **Then** 更新成功并输出更新后的元信息

## Tasks / Subtasks

- [x] Task 1: 命令实现 (AC: 1)
  - [x] `commands/docs/update.rs`：`documents.update`，`id=` + 可选 `title=` / `text=` / `publish=true`
  - [x] 正文来源与 create 共用 `commands/docs/content.rs`（`--file` 优先，全空白视为未提供）
  - [x] 三者全无 → 用法错误（退出码 2），不发无意义请求
  - [x] 空正文 → 拒绝（防误清空），作为 `content::read` 之外的第二道防线
- [x] Task 2: 输出 (AC: 1)
  - [x] 与 create 共用 `commands/docs/detail.rs`：id / title / updated / revision / url / status
- [x] Task 3: 测试 (AC: 1)
  - [x] `request_args` 单测：title 单独/正文单独/publish 单独/两者同时/全无→2/空正文→2 且消息含 "erase"
  - [x] `tests/docs_write.rs`：`--title` 精确请求体（脚本空 stdin 不得变成 `text=""`）、stdin 正文、404→5

## Dev Notes

- **最危险的失效模式是"静默清空文档"**，因此有两层防护：
  1. `content::read` 把全空白来源报成"没有正文"（脚本里 stdin 常是 `/dev/null` 或已关闭）；
  2. `request_args` 仍然检查一遍空正文并报错，指向 `otl api documents.update id=<id> text=`。
  第二层在正常路径上不可达，但它是纯函数、被单测直接覆盖，值得留着。
- **不做的事**：`documents.update` 还有 `editMode=patch` + `findText`、`lastRevision` 乐观锁、
  `collectionId` 移动、`insightsEnabled` 等。精选命令刻意只覆盖 title/正文/publish；
  其余走 `otl api documents.update`。`lastRevision` 冲突检测是 v2 的 pull/push 才需要的能力
  （SPEC Non-goals 已为其"留口"，本 story 不实现）。
- **输出复用 create 的汇报格式**，两个命令的成功输出形状一致，脚本可以同样解析。

### References

- [Source: planning/epics.md#Story 3.4]
- [Source: specs/spec-outline-cli/SPEC.md#Non-goals（pull/push 与 revision 冲突检测留待 v2）]

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-26

### Completion Notes List

- `--publish` 单独给出也算"有要更新的东西"（把草稿转正是一个合法的无内容变更）。
- `documents.update` 的 `id` 在 spec 里没有 `format: uuid`（接受 urlId），所以短 id 也能用。

### File List

- crates/otl/src/commands/docs/update.rs
- crates/otl/src/commands/docs/{content.rs, detail.rs}（与 3.3 共用）
- crates/otl/tests/docs_write.rs
