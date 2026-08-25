# Story 1-5b: schema 驱动表格列

Status: review

## Story

As a 在终端读列表输出的用户,
I want 表格列由操作的响应 schema 决定而不是由某一次响应的内容决定,
so that 同一个操作每次渲染出同样的列，且没有任何端点需要手写渲染代码。

（Story 1.5 的 follow-up：1.5 落地时 IR 还没有响应 schema，列只能从数据里猜，`render.rs` 留了
`TODO(story-1.5b)`。本 story 接上 IR。）

## Acceptance Criteria

1. **Given** IR 中带有操作的响应字段描述
   **When** 在 TTY 上渲染列表响应
   **Then** 关键列由 schema 自动挑选，列选择是一套通用策略（无任何端点专属渲染代码或 IR 数据）
2. **Given** 同一操作的两次响应携带了不同的可选字段
   **When** 渲染
   **Then** 表头相同（列集合是操作的属性，不是某次响应的属性）
3. **Given** schema 未描述的响应（raw `--body`、spec 未声明响应、spec 漂移）
   **When** 渲染
   **Then** 回退到 1.5 的数据驱动策略，不出现空表
4. **Given** 既有 golden file
   **When** 跑测试
   **Then** 全部继续通过（数据驱动路径行为未变），schema 驱动路径新增独立 golden

## Tasks / Subtasks

- [x] Task 1: IR 增加响应形状 (AC: 1)
  - [x] `engine::ir::FieldSpec { name, ty, format, nullable, read_only }`，`OpSpec::response_fields`
  - [x] `IR_SCHEMA_VERSION` 4 → 5，版本历史注释更新，生成代码里的编译期断言同步
  - [x] engine 保持通用：`FieldSpec` 只描述「一个响应条目的字段」，信封在哪由 spec 编译器决定
- [x] Task 2: build.rs 编译响应字段 (AC: 1, 3)
  - [x] 从 `responses.200.content.application/json.schema` 出发，取 `properties.data`，数组则取 `items`
  - [x] `$ref` / `allOf` 展开复用既有 `resolve_ref` / `param_type` / `extract_facets`
  - [x] `Facets` 增加 `read_only`（`readOnly`）
  - [x] build 依赖的 `serde_json` 开 `preserve_order`：schema 声明顺序是列排序的关键信号
- [x] Task 3: 通用列选择策略 (AC: 1, 2, 3)
  - [x] `select_schema_columns`：容器字段丢弃 → identity → label → 时间戳 → 其余必填 → 可空
  - [x] 全部并列关系用 schema 声明顺序决胜
  - [x] schema 的列在响应里完全不命中时回退数据驱动（`select_data_columns`，即原策略）
- [x] Task 4: 接线 (AC: 1)
  - [x] `render::render(payload, mode, schema)`，`api.rs` 传 `&op.response_fields`
- [x] Task 5: 测试 (AC: 1-4)
  - [x] `tests/ir_table.rs` +6 用例（字段编译、声明顺序、facet、无响应描述的操作、覆盖率下限、无重名）
  - [x] `tests/render_golden.rs` +9 用例，新增 golden `schema_table_documents.txt` / `schema_table_collections.txt`
  - [x] 既有 golden 一字未改（数据驱动路径行为不变）

## Dev Notes

- **列选择策略（唯一一套，适用于所有端点）**，全部基于任何 OpenAPI schema 都能表达的 facet：
  1. `ParamType::Json`（对象/数组/union）直接丢弃——不可能是单元格。
  2. **identity**：第一个 `format: uuid` 且非 nullable 的字段；没有则退回名为 `id` 的字段。
  3. **label**：第一个非 nullable 的「裸字符串」（`type: string` 且无 `format`——id/时间戳/URL/email 都带
     format），优先取 schema **没有**标 `readOnly` 的那个。
  4. 其余非 nullable 的 `date-time`，按声明顺序。
  5. 再是其余非 nullable 字段，最后是 nullable 字段。
- **`readOnly` 是这套策略的关键信号**。`Collection` 的声明顺序是 `id, url, urlId, name, ...`：只按顺序会
  选中 `url` 当 label。`url`/`urlId` 是 `readOnly`（服务端派生），`name` 不是（用户起的名字）。同一条规则
  在 `Document` 上选出 `title`（`text` 输在声明顺序、`url` 输在 readOnly），在 `User`/`Group` 上选出 `name`。
  没有任何一行代码提到 documents 或 collections。
- **声明顺序是承重结构**，而 `serde_json` 默认的 `Map` 是 `BTreeMap`（按字母排序）。字母序下 `Document`
  的裸字符串候选是 `text` < `title`——会把整篇 markdown 正文选成 label。所以 build 依赖开
  `preserve_order`。resolver 2 不在 build 依赖与普通依赖之间统一 feature，运行时的 `serde_json` 仍是排序
  map，`--json` 输出的键序因此完全没变（既有 JSON golden 原样通过，可作证据）。
- **为什么不按数据裁剪 schema 列**：「响应里没出现的列就不显示」听起来贴心，但那会把列集合重新变成某次响应的
  属性——正是 1-5b 要消灭的东西。只有「schema 的列在响应里一个都不命中」时才整体回退数据驱动（spec 漂移、
  raw `--body` 返回了别的形状）。代价是偶尔出现一整列空值，换来确定性。
- **既有 golden 未变**：`render()` 只是多了一个 schema 参数，空切片即原策略。数据驱动策略保留为回退，
  两条路径各自有 golden。新增的 schema golden 里 `createdAt` 在 `updatedAt` 之前（schema 声明顺序），
  而数据驱动 golden 里 `updatedAt` 在前（那条策略的固定优先级表）——两者各自内部一致，互不影响。
- **体积**：190 个操作 × 平均 ~15 个字段的静态表使 release 二进制从 2 567 312 增到 2 783 488 字节
  （其中也含 clap_complete/toml/directories），预算 ~5 MB。启动仍 3.38 ms（静态表零解析）。
- **`documents.search` 之类的操作**：`SearchResult` 只有 `context`(string)/`ranking`(number)/`document`
  (object)，策略给出 `context, ranking`——没有 identity 也不崩，这是 schema 的实际形状。
- **无响应描述的操作**（如 `documents.delete`）`response_fields` 为空，直接走数据驱动/JSON 回退。

### 故意留下的缺口

- 列数上限仍是 4（1.5 定的常量），未做终端宽度自适应；那是独立的 UX 决定。
- `x-cli` overlay（stack.md 提到的输出模板）没有接：那会引入「每端点渲染数据」，与本 story 的通用策略
  取向相反，属于 `--template`（minijinja）后续项。
- `data` 信封的 pointer 只认 `properties.data`，与运行时 `response.get("data")` 是同一约定。若 Outline
  哪天改信封，两处都要改——已在两侧注释里互相点明。

### References

- [Source: stories/sprint-status.yaml#1-5b-schema-driven-columns（follow-up 记录）]
- [Source: planning/epics.md#FR19（双态输出）]
- [Source: project-context.md Critical Don't-Miss Rules（「通用渲染必须 schema 驱动」）]
- [Source: crates/otl/src/render.rs 的 `TODO(story-1.5b)`（本 story 移除）]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Completion Notes List

- `OpSpec` 加字段导致 engine 的 4 个测试文件与 `api_e2e.rs` 的结构体字面量需要补 `response_fields`
  （机械改动，各 1-3 行）。
- 质量门：fmt / clippy `-D warnings` / `cargo test --workspace`（334 通过 0 失败）全绿；bench 3.38 ms。

### File List

- crates/engine/src/ir.rs（`FieldSpec`、`OpSpec::response_fields`、版本 5）
- crates/engine/src/lib.rs（导出 `FieldSpec`）
- crates/engine/tests/{execute.rs, rate_limit.rs, validation.rs, pagination.rs}（补字段）
- crates/otl/build.rs（响应字段编译）
- crates/otl/Cargo.toml（build 依赖 `preserve_order`）
- crates/otl/src/render.rs（schema 驱动列选择 + 数据驱动回退）
- crates/otl/src/commands/api.rs（传入 `op.response_fields`）
- crates/otl/tests/{ir_table.rs, render_golden.rs, api_e2e.rs}
- crates/otl/tests/golden/{schema_table_documents.txt, schema_table_collections.txt}（新增）
- README.md
