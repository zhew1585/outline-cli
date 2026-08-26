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
2. **Given** 同一操作的两次响应
   **When** 渲染
   **Then** 列的**排序规则**来自 schema（操作的属性，与响应无关）；**出现哪些列**取决于响应实际携带了
   哪些字段——因为 OpenAPI 的 `nullable: false` 只约束「出现时不为 null」，不代表字段必然存在，且
   vendored spec 里所有响应 schema 的 `required` 均为空。同字段集的两次响应表头相同；
   稀疏响应不得出现「整列全空」或「真实存在的字段被空列挤掉」
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
  - [x] `rank_schema_columns`：容器字段丢弃 → identity → label → 时间戳 → 其余非空 → 可空；
        返回**完整排名**而非前四名
  - [x] 全部并列关系用 schema 声明顺序决胜
  - [x] 调用方按「响应里至少一行有该字段」过滤排名，再取前 `MAX_TABLE_COLUMNS` 个
  - [x] schema 一个候选都没贡献时回退数据驱动（`select_data_columns`，即原策略）
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
- **为什么最终按数据裁剪 schema 列（R1 finding 6 的处置）**：原实现把 `nullable = false` 当成「字段必然
  存在」，只要任一选中列命中就保留其余全空列。这在 OpenAPI 里不成立——non-nullable 只说「出现时不能是
  null」——而且 vendored spec 里 32 个带 properties 的 component schema 的 `required` 全部为空，所以
  「必然存在」这个信息**根本不在 schema 里**（也因此没有把 `required` 加进 `FieldSpec`：它对全部 schema
  都是 false，是纯粹的死重量）。
  实测退化：`[{"id":"d1","icon":"🔥"}]` 得到 id/title/createdAt/updatedAt 四列，后三列每行全空，
  而真实存在的 `icon` 被四列上限挤掉。原测试 `schema_columns_do_not_depend_on_which_optional_fields...`
  恰好用了只含 id 的稀疏行，却只断言表头不变，于是把退化当成了稳定性——审查者点得对。
  **改为**：schema 提供**完整排名**（操作级、确定），调用方丢掉「没有任何一行携带」的字段后再取前四。
  `nullable` 的语义随之更正为「非空字段当它出现时永远不是空单元格，因此更值得占一列」，不再声称「必然存在」。
  新的确定性表述：**排序**与响应无关；**出现哪些列**只取决于响应携带的字段集合，与行序、map 迭代序无关。
  **R2 追加**：R1 的过滤只看「key 是否存在」，而 nullable 字段常常是「存在且显式为 null」——渲染出来就是
  空单元格。因此判据改为**有内容**（`has_content`）：缺失、`null`、空白字符串都不算；`false` 与 `0` 算，
  它们是读者想看的值。不变量升级为「被选中的列不可能每行都空」，并对多种 payload 形状统一断言
  （`no_selected_column_is_ever_blank_in_every_row`）。
  新增测试：`a_sparse_response_shows_the_fields_it_has_not_empty_columns`（逐列断言「不得每行都空」）、
  `a_present_field_is_never_crowded_out_by_an_absent_one`、
  `the_schema_still_supplies_the_ranking_not_the_payload`、
  `schema_columns_are_stable_for_responses_carrying_the_same_fields`。
- **既有 golden 未变**：`render()` 只是多了一个 schema 参数，空切片即原策略。数据驱动策略保留为回退，
  两条路径各自有 golden。新增的 schema golden 里 `createdAt` 在 `updatedAt` 之前（schema 声明顺序），
  而数据驱动 golden 里 `updatedAt` 在前（那条策略的固定优先级表）——两者各自内部一致，互不影响。
- **体积**：190 个操作 × 平均 ~15 个字段的静态表使 release 二进制从 2 567 312 增到 2 783 488 字节
  （其中也含 clap_complete/toml/directories），预算 ~5 MB。启动仍 3.38 ms（静态表零解析）。
- **`documents.search` 之类的操作**：`SearchResult` 只有 `context`(string)/`ranking`(number)/`document`
  (object)，策略给出 `context, ranking`——没有 identity 也不崩，这是 schema 的实际形状。
- **无响应描述的操作**（如 `documents.delete`）`response_fields` 为空，直接走数据驱动/JSON 回退。

### R1 对抗审查处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| 6 | MAJOR | 已修：schema 只提供排名，出现哪些列由响应字段集决定；不再把 `nullable=false` 当作「必然存在」；`required` 明确不入 IR（全 spec 皆空，纯死重量）；四个新测试钉住稀疏响应行为 |
| R2-4 | MAJOR | 已修：过滤条件由「key 存在」改为「有内容」——`null`、缺失、空白字符串都不算内容，`false` / `0` 算。原实现下 `[{"id":"1","a":null,"b":null,"c":null,"d":"useful"}]` 会渲染三个全空列并把 `d` 挤掉 |
| R3-6 | MAJOR | 已修：`has_content` 判的是**原始字符串**，而单元格经 `sanitize_cell` 后可能完全不可见——`"\u{1b}"` 会变成空格，`"\u{200b}"` 保留但宽度 0，多个这类字段能占满四列。现在判据是「渲染后是否占据终端列」（`renders_visibly`：grapheme 既可打印又宽度 >0）。另外审查者指出我的 invariant 测试用 `split_whitespace().nth(i)` 对齐列不可靠（空的中间列会让后面的值左移），已改为**独立重实现**的 payload 级判据，完全不解析对齐后的表格 |
| R4-2 | MAJOR | 已修：R3 只把内容判据加在 schema 候选路径上，`from_schema` 为空时转入的 `select_data_columns` 仍只排容器、不查内容——而「schema 一个候选都没有」恰恰意味着所有排名字段都是空的，于是 fallback 又按名字优先级选中同一批空字段，把真正有内容的挤出四列。现在两条路径共用同一个 `has_content`，并新增「两条路径都不得出现全空列」的统一不变量测试 |
| R5 | VERIFIED | R4-2（三条路径统一走 `has_content`）经复核确认；本轮无新发现。测试文件按关注点拆分为 `render_golden.rs`（数据驱动 + 布局）与 `render_schema.rs`（schema 驱动列），两者均在 800 行铁律内 |
| R6-3 | **MAJOR** | 已修：表格单元格只清 `is_control()`，U+202E / U+2066 / U+200B / U+FEFF 直达 stdout，而**未闭合的 RLO 会把整行后续内容视觉重排**（不止它自己那一格）。同一个仓库的 `config/error.rs` 与 `engine/sanitize.rs` 都已经处理这些字符——又是一次「三处修了两处」。根因是三处各有一套过滤器，所以修法不是再加一处，而是把**分类**收进新的 `otl::text`（穷尽 enum：Control / BidiFormat / Invisible / Joiner），各表面只决定**如何渲染**每一类：诊断全部替换为可见标记，单元格把控制符换空格、把有作用域的 bidi 换 U+FFFD（篡改要看得见）、把零宽丢弃（宽度要诚实），三者都保留 ZWJ——否则 emoji 连字与波斯语拼写会被破坏。新增 4 个渲染测试 + `otl::text` 自己的分类单测 |
| R7 | VERIFIED | R6-3 经独立攻击与实机二进制验证通过：`invoice\u{202e}gnp.exe` 在表格里渲染为 `invoice\u{fffd}gnp.exe`，family emoji 的 3 个 ZWJ 完整保留，CJK/grapheme/截断上界无退化。审查者确认四分类是 R6 已验证集合的忠实拆分，且穷尽 match 会让新增类别编译失败 |
| R7-2 | MINOR | 已修：JSON 路径不清洗（有意），但 `text.rs` 的「every surface」措辞过宽。已收敛为「渲染给人读的每个表面」，`--json` 作为**明示豁免**并写清代价，新增测试把豁免钉成被检查的决定 |
| — | 验证 | 审查者独立确认 resolver-2 的 build-dep feature 隔离成立（`.fingerprint` 里 runtime `["default","std"]` 与 build-script `[...,"preserve_order",...]` 两套 artifact 并存） |

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
