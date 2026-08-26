# Story 4.3: doctor

Status: review

## Story

As a 排障的用户,
I want 一条命令看清环境健康,
so that 问题自查不开 issue。

## Acceptance Criteria

1. **Given** 执行 `otl doctor`
   **When** 诊断运行
   **Then** 输出：认证状态、实例连通性、线上 API 与本地 spec 差异（缺失/已弃用操作）、spec 缓存健康
2. **Given** 任一检查发现阻塞性问题
   **When** 报告打印完毕
   **Then** 退出码等于「第一条阻塞发现在其它命令里本来会产生的那个码」（本地问题 2 / 需重新认证 4 /
   实例侧 3·5·6·7·8），**不新增退出码**；仅告警（缓存被丢弃、本地表落后、spec 源不可达、环境里有明文 key）
   退 0
3. **Given** Story 2-6 AC 5/6 交给 doctor 的部分
   **When** 报告凭证健康
   **Then** 输出凭证文件路径、存在性、权限是否合规、各 profile 有哪些凭证类型，且 Windows 上明示
   「不设权限位、保护完全依赖 profile 目录 ACL」；**绝不打印任何凭证值或其片段**
4. **Given** `--offline`
   **When** 诊断运行
   **Then** 不发出任何网络请求（实例与 spec 源都不碰），其余检查照常报告

## Tasks / Subtasks

- [x] Task 1: 报告模型与双态渲染 (AC: 1, 2)
  - [x] `commands/doctor/report.rs`：`Status{Ok,Warn,Problem(ExitCode),Skipped}` + `Check` + `Report`
  - [x] `Report::blocking()`＝**第一条** Problem（依赖序＝该修顺序），`exit_code()` 由它决定
  - [x] stdout 为数据（人类行 / `--json` 对象），stderr 只有 `main` 打印的那条阻塞摘要
  - [x] 人类渲染在 **sink 层**过 `stdio::scrub_terminal_controls` 并强制单行；`--json` 按 `text.rs` 的
        既有决定豁免（payload 要能 round-trip）
  - [x] golden file 测试（合成报告，不含机器相关值）
- [x] Task 2: 本地检查 (AC: 1, 2, 3)
  - [x] `configuration`：config 文件位置/实际读到的文件/profile 及其来源层/文件里定义了哪些 profile
  - [x] `instance`：经 `auth::resolve_instance`（含 TLS 规则），报 origin 与 URL 来源层
  - [x] `credentials`：直接调 `auth::report::credential_health()`（2-6 留的接缝），加平台保护说明
  - [x] `credential`：经 `auth::resolve_credential`，报 method / 被遮蔽的候选 / 过期与 scope
  - [x] 阻塞码一律取自 `auth::exit_code_of`（新增：`map_auth_error` 的借用版，同一张表）
- [x] Task 3: 连通性 (AC: 1, 2, 4)
  - [x] 唯一一次实例请求：`auth.info` 走 engine execute 通道（与 `auth login` 收尾同一条调用）
  - [x] 凭证只能来自 `resolve_credential` 的 `Resolved`，`into_client` 消费它——没有第四条凭证路径
  - [x] 401→4、5xx→6、传输失败→7、429 耗尽→8：全部由既有 mapper 决定
- [x] Task 4: spec 检查与线上差异 (AC: 1, 2, 4)
  - [x] `local-spec`：`ops::table()` / `ops::is_synced()` / `cache::load()`；损坏或过期＝**告警**且回退内置
  - [x] `online-spec`：`spec::upstream_table()`（复用 `spec sync` 的 fetch+compile 入口，**不写缓存**）
  - [x] 三类差异：`missing`（线上有本地无）、`withdrawn`（本地有线上已不声明）、
        `deprecated`（线上标记弃用且本地仍可调用）
  - [x] 缓存 hash 与刚抓到的文档 hash 对比，明说「缓存就是这份文档编出来的」或「不是」
  - [x] `spec-compile`：`CompiledOp.deprecated`（只到编译器层，**不进 IR**，不动 IR schema 版本）
- [x] Task 5: 文档与登记 (AC: 2, 3)
  - [x] `docs/exit-codes.md`：登记 doctor 的四条规则（first-not-worst / 告警不阻塞 / 报告必打印 / 不自己分类）
  - [x] README：`## Checking your environment`；并修正 Design 里「唯一例外是 spec sync」的表述
  - [x] `project-context.md`：运行时解析 OpenAPI 的例外归属改述为「模块」而非「命令」
- [x] Task 6: 测试 (AC: 1-4)
  - [x] 26 个单测（report / checks / drift）+ 13 个 wiremock 端到端 + 2 个 golden + 1 个 speccompile
  - [x] 每个断言都做过回退验证（17 处变异全部转红，清单见 Dev Notes）
  - [x] 全部端到端用 `tests/common/mod.rs` 的 `isolate` + 自己的临时目录

## Dev Notes

- **先读 `project-context.md`**。本 story 触碰的红线：不打印凭证（含 debug）、凭证获取只走既有三个入口、
  HTTP 只走既有三条通道、文本清洗、退出码是公共 API、库层禁 unwrap、文件 <800 行 / 函数 <50 行、
  Windows 分支显式处理、不 phone home。

- **退出码语义为什么这样定**（这是本 story 唯一的公共 API 决策，先论证后登记）：
  - 候选 A「永远退 0，只是报告」：CI 里 `otl doctor` 就没有任何用处——一个 world-readable 凭证文件
    或压根没配凭证的机器，脚本无法从退出码看出来。
  - 候选 B「发现问题就退 1」：把「本地少个环境变量」和「内部错误」混成一个码，与既有表冲突
    （1 是 generic failure）。
  - 候选 C「新增一个 doctor 专属码（比如 10）」：新增码就是新增公共 API，而它承载的信息量为零——
    调用方还要再读报告才知道该干什么。
  - **选定 D**：**不新增码，取第一条阻塞发现在其它命令里本来会产生的那个码**。语义与全表一致
    （2＝本地修、4＝重新认证、7＝网络），脚本已有的分支逻辑直接复用；实现上 code 只能来自
    `auth::exit_code_of`，所以诊断不可能与被诊断的命令给出不同的码。
  - **first-not-worst**：检查按依赖序跑，早出现的问题既是后面失败的原因、也是该先修的东西。
    取「最严重」会把「`OUTLINE_URL` 没设」报成网络错误。
  - **告警永不改码**：缓存被丢弃、本地表落后、spec 源不可达、环境里有明文 key —— 都不妨碍 `otl` 工作。
    尤其 spec 源是第三方主机，让它的 404 把一个能用的环境判成坏的，doctor 就没法进 CI。
  - **报告先打印再退出**：`--json` 消费方每次都拿到同一个对象；阻塞摘要另外走 stderr（由 `main` 打印）。

- **「已弃用操作」到底指什么**（FR23 / SPEC.md「doctor 能发现本地 spec 缺失或已弃用的操作」）：
  两种读法，本 story 都实现了，因为单靠任一种都会被挑：
  1. **diff 读法**：本地表里有、线上文档已不再声明 → `withdrawn`。这是 `spec sync` 的 added/removed
     同一套词汇，也是「差异报告」的字面含义。
  2. **flag 读法**：OpenAPI 的 `deprecated: true`。实测 vendored 文档里 **operation 级 `deprecated` 为 0 条**
     （19 处 `"deprecated": true` 全在 schema/参数层），所以只做这一种会得到一份永远为空的报告。
     为此在 **spec-compile** 加了 `CompiledOp.deprecated`（唯一构造点，约 10 行），
     **故意不进 `engine::ir`**：IR 是 dispatch 表，加一个位就要动 `IR_SCHEMA_VERSION`，
     那会作废所有用户已有的缓存——而唯一需要这个位的消费方（doctor）本来就自己编译刚抓到的文档。
     报告只取「线上标记弃用 ∩ 本地仍可调用」的交集：本地压根没有的操作被弃用不是用户的问题。
  - failure-modes.md #8 里还有一条「弃用操作 IR 标记并警告一个版本」——那是**调用时**警告，需要
    IR 带弃用位，属于另一条 story 的范围；4.3 的 AC 只要求 doctor 的报告。**故意留下的缺口**。

- **为什么 doctor 不自己抓文档**：`tests/no_phone_home.rs` 把 `fetch_document` / `get_text` /
  `UPSTREAM_SPEC_URL` 收敛在 `crates/engine/src/fetch.rs` 与 `crates/otl/src/commands/spec.rs`。
  doctor 调 `commands::spec::upstream_table()`，于是**那张收敛表一个字都不用改**——
  「codebase 里只有三处 `.send()`」这个不变量原样成立。代价是 `commands/spec.rs` 从 559 涨到 626 行（限 800）；把它拆到新文件反而要放宽收敛表，那是更差的交易。

- **doctor 与 `auth info` 的一处**故意**分歧**：完全没有凭证时，`auth info` 打 `method: none` 且退 0
  （它是**描述**命令），doctor 判定为 Problem/2（它是**判定**命令：这个环境不能用）。两者读的是同一个
  `resolve_credential`，只是对同一状态的**结论**不同，这在 `credential()` 的注释里写明了。

- **凭证文件不可用时 doctor 一个字节都不发**：`choose()` 先 `store.load()`，失败即 `Refused`，
  连通性检查被跳过。测试 `an_over_wide_credential_file_exits_two_before_anything_is_sent` 断言
  mock 服务器收到 **0** 个请求，并且**故意同时导出了 `OUTLINE_API_KEY`**——否则「没发请求」只是因为
  没东西可发，这个断言就不敏感（回退验证 M14 第一次跑出 GREEN，就是这个原因，随后把 env key 加进去才转红）。

- **文本清洗在 sink 层**：doctor 的报告走 stdout，不经 `write_diagnostic_line` 的清洗，而它要插入
  config 文件里的 profile 名、环境变量给的路径、抓到的文档里的 operation 名、服务端的错误消息。
  所以 `report::human_line()` 对**每一行**做 `stdio::scrub_terminal_controls` 并把换行压成空格：
  一个带换行的外来值不能伪造成另一条检查的结论。`--json` 按 `text.rs` 既有的决定豁免。

- **平台差异用 `cfg!` 而不是 `#[cfg]`**：`protection_note()` 两个分支在所有平台都编译，
  文本在所有平台可测，配对关系由 `assert_eq!(protection_note().is_some(), cfg!(windows))` 钉住
  （回退验证 M12：把条件换成 `cfg!(unix)` 后该测试在 macOS 上立刻转红，所以这条断言是敏感的）。

- **golden file 用合成报告**：真实报告含机器相关值（凭证路径、操作数、时钟），拿它做 golden 等于
  把开发机钉进仓库。合成报告覆盖四种 status、多行 detail、以及一个带 ESC/BEL 的敌意 summary，
  于是 golden 同时是**布局**和**清洗**的证据。

- **回退验证清单（17/17 全部转红）**：见 Dev Agent Record。

### Project Structure Notes

```
crates/
  speccompile/src/lib.rs                 # + CompiledOp.deprecated（不进 IR）
  otl/
    src/commands/spec.rs                 # + fetch_remote() / compile_document() / upstream_table()
    src/commands/doctor/
      mod.rs                             # 参数、编排、退出码语义（模块文档即契约）
      report.rs                          # Status/Check/Report + 双态渲染 + sink 层清洗
      checks.rs                          # configuration / instance / credentials / credential / connectivity
      drift.rs                           # local-spec / online-spec（缺失·撤回·弃用）
    src/auth/mod.rs                      # + exit_code_of()（map_auth_error 的借用版）
    src/errors.rs                        # + engine_exit_code()（classify 的借用版）
    tests/doctor_e2e.rs                  # 13 个 wiremock 端到端
    tests/doctor_golden.rs               # 2 个 golden
    tests/golden/doctor_report.txt
```

### References

- [Source: planning/epics.md#Story 4.3、FR23]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-7（doctor 差异报告）]
- [Source: specs/spec-outline-cli/failure-modes.md #7 #8 #10]
- [Source: stories/2-6-credential-file-hygiene.md AC 5/6 与其 Dev Notes 的归属说明]
- [Source: docs/exit-codes.md]
- [Source: project-context.md 全文]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### 回退验证（每条都是：改坏被测逻辑 → 确认测试转红 → 复原）

驱动脚本按表逐条打补丁并跑相关目标，结果 17/17 转红：

| # | 改坏了什么 | 转红的测试 |
|---|-----------|-----------|
| M1 | `Report::blocking()` 恒返回 `None` | 10 个（4 个端到端退出码 + report/mod 单测 + 2 个 golden） |
| M2 | `human_line()` 不做清洗 | `human_output_is_scrubbed_and_kept_on_one_line` + 2 个 golden |
| M3 | `Check::detailed()` 不拆多行 | 同上 2 个 |
| M4 | 权限过宽的凭证文件报成正常 | 端到端 `an_over_wide_...` + 单测 `an_over_wide_credential_file_is_a_problem_that_names_it` |
| M5 | 没有凭证只算 Warn | 端到端 `no_credential_anywhere_...` + 单测 `a_missing_credential_names_every_way_to_get_one` |
| M6 | `--offline` 被忽略 | 端到端 `offline_contacts_neither_...` + 单测 `connectivity_is_skipped_offline_...` |
| M7 | 实例不可达只算 Warn | 端到端 7 / 4 / hostile 三个 |
| M8 | 不计算 deprecated | 端到端 `a_deprecated_operation_...` + 单测 `only_deprecations_of_...` |
| M9 | 不计算 missing | 端到端 `the_online_comparison_...` + 单测 `an_operation_only_online_...` |
| M10 | spec 源不可达算 Problem | 端到端 `an_unreachable_spec_source_is_a_warning_...` + hostile |
| M11 | 缓存损坏报成正常 | 端到端 `a_damaged_spec_cache_is_a_warning_...` |
| M12 | 平台说明的条件换成 `cfg!(unix)` | 单测 `the_windows_note_appears_exactly_where_it_is_true` + `a_healthy_credential_file_...` |
| M13 | 不比对缓存/文档 hash | 单测 `the_cache_hash_is_compared_with_the_document_that_was_fetched` |
| M14 | 凭证文件读不了就退回用 env key | 端到端 `an_over_wide_credential_file_exits_two_before_anything_is_sent`（**第一次是 GREEN，见 Dev Notes**） |
| M15 | 报告里回显 `OUTLINE_API_KEY` 的值 | 端到端 `the_report_never_carries_a_credential_or_a_fragment_of_one` |
| M16 | config 文件不可用只算 Warn | 端到端 `an_unparsable_config_file_exits_two_...` |
| M17 | `is_deprecated()` 恒 false | speccompile 单测 + 端到端 deprecated |

### Completion Notes

- 新增 42 个测试（26 单测 + 13 端到端 + 2 golden + 1 speccompile）。
- 不新增退出码；`docs/exit-codes.md` 登记了 doctor 的四条规则，README 的表由它派生（未变）。
- 网络入口零新增：`tests/no_phone_home.rs` 的收敛表未改动。
