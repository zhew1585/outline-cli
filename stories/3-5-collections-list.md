# Story 3.5: collections list

Status: done

## Story

As a 用户,
I want 列出全部 collection,
so that 拿到 id 供其他命令使用。

## Acceptance Criteria

1. **Given** 已认证
   **When** 执行 `otl collections list`
   **Then** 表格输出名称/id/文档数，自动分页拿全

## Tasks / Subtasks

- [x] Task 1: 命令骨架 (AC: 1)
  - [x] `commands/collections.rs`：`otl collections list [--limit N] [--no-counts]`
  - [x] `main.rs` 追加 `Collections(CollectionsArgs)` variant + match arm
- [x] Task 2: 自动分页拿全 (AC: 1)
  - [x] `session.call_rows("collections.list", &[], limit)`：跨页合并，截断有 stderr 警告
- [x] Task 3: 文档数列 (AC: 1)
  - [x] 每个 collection 调一次 `collections.documents`，用显式栈（非递归）数导航树节点
  - [x] 读不到的 collection 显示 `?`，只在结尾汇总一条 stderr 警告，不让列表失败
  - [x] `--no-counts` 完全跳过这轮请求；`--json` 输出服务端原始行（不注入合成字段）
  - [x] R1 修复：数到 `MAX_COUNTED_NODES` 上限时显示 `100000+` 并 stderr 警告，不再冒充精确值
  - [x] R1 修复：列表被 CLI 页上限截断时退出码 9（`--limit` 截断仍为 0）
- [x] Task 4: 测试 (AC: 1)
  - [x] golden file `tests/golden/collections_list_table.txt`（含 CJK 名称的对齐、`?` 与 0 两种计数）
  - [x] 单测：扁平/嵌套计数、非数组载荷、10 万层深树不爆栈、未知计数渲染
  - [x] `tests/collections_list.rs`：`--json` 是原始行且不额外发请求、两页合并、`--limit` 警告、403→4、缺配置→2

## Dev Notes

- **API 没有文档数字段**。vendored spec 的 `Collection` schema 是
  `id/url/urlId/name/description/data/sort/index/color/icon/permission/templateManagement/sharing/commenting/createdAt/updatedAt/deletedAt/archivedAt/archivedBy/sourceMetadata`
  ——没有任何 count。因此"文档数"只能推导，选择了 `collections.documents`（该 collection 的导航结构）
  并数节点：这正是"这个 collection 里有多少篇文档"的语义。
  代价是每个 collection 一次请求，所以：
  - 只在人类可读模式做（`--json` 是原始数据，不能凭空多出 API 无法确认的字段）；
  - 给了 `--no-counts` 逃生舱；
  - 单个 collection 读不到（权限/404）只显示 `?` 并汇总警告，不拖垮整个列表。
- **导航树是服务端数据，深度不可控**：递归计数一旦遇到深树就是进程 abort，所以用显式栈 +
  `MAX_COUNTED_NODES` 上界。对应单测构造 10 万层深树（注意：`json!` 作用在非字面量表达式上会走
  `to_value` 深拷贝，测试里必须用 move 构造，否则测试自身变成 O(n²)）。
- **`collections.documents` 的 `id` 是 `format: uuid`**：真实实例的 collection id 就是 UUID，
  但若某实例返回非 UUID，engine 会本地拒绝，该行计数落到 `?`——降级而非崩溃。
- **计数有三种可区分状态**（R1 finding 9）：精确数字、`<n>+`（走到节点上限，只能算下界）、
  `?`（结构读不到）。把上限截断显示成精确的 `100000` 是错误事实，不是四舍五入。
- **列宽按终端显示宽度算**：复用既有 `render` 的 grapheme + `unicode-width` 布局，
  所以 CJK 名称后面的列不会错位（golden file 锁住了这一点）。

### References

- [Source: planning/epics.md#Story 3.5]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-6 自动分页]
- [Source: crates/otl/spec/spec3.json（`Collection` schema 无 count 字段）]

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-26

### Completion Notes List

- 已知缺口：`--json` 不含文档数。这是刻意的——注入 API 无法确认的字段会让脚本把推导值当权威值。
  需要计数的脚本可以自己 `otl api collections.documents id=<id> | jq`。
- `otl collections list` 的表格三列固定为 NAME / ID / DOCUMENTS（精选命令允许挑列，不允许 per-endpoint 渲染分支）。

### File List

- crates/otl/src/commands/collections.rs
- crates/otl/src/commands/mod.rs、crates/otl/src/main.rs（追加式）
- crates/otl/tests/collections_list.rs
- crates/otl/tests/golden/collections_list_table.txt
