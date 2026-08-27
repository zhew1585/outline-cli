---
project_name: 'outline-cli (otl)'
user_name: 'weizhe'
date: '2026-08-25'
sections_completed: ['technology_stack', 'language_rules', 'architecture_boundaries', 'testing_rules', 'security_rules', 'cli_contract', 'workflow_rules', 'dont_miss_rules']
existing_patterns_found: 0
status: 'complete'
rule_count: 31
optimized_for_llm: true
---

# Project Context for AI Agents

_This file contains critical rules and patterns that AI agents must follow when implementing code in this project. Focus on unobvious details that agents might otherwise miss._

规范来源：specs/spec-outline-cli/（SPEC.md、stack.md、failure-modes.md）与 planning/epics.md。
冲突时以 SPEC 为准。

---

## Technology Stack & Versions

- Rust stable（edition 2021+），Cargo workspace：`crates/engine`（通用 OpenAPI RPC 引擎）+ `crates/otl`（Outline UX 层，产出二进制 `otl`）。
- 核心依赖：clap（derive + 运行时动态构建）、serde/serde_json、anyhow（应用层）+ thiserror（engine 库层）、reqwest（rustls-tls，禁用默认 native-tls）、toml、directories、clap_complete、bincode。**不用 keyring**（凭证走本地文件）。
- 测试：wiremock、assert_cmd、golden file 快照；发布：cargo-dist。

## Critical Implementation Rules

### Language-Specific Rules（Rust）

- engine crate 错误用 thiserror 定义类型化错误；otl crate 边界才允许 anyhow；库层禁止 `unwrap()/expect()`（测试代码除外）。
- 不可变优先：默认 `let` 不可变，数据变换返回新值；共享状态用 `Arc<...>` 而非可变全局。
- 禁止 `unsafe`（无豁免）。
- IR 数据结构全部 `#[derive(Serialize, Deserialize)]` 且版本化（bincode 缓存含 schema 版本号，不兼容即整体废弃重建）。

### 架构边界规则（最关键）

- **engine crate 禁止 import/引用任何 Outline 特定内容**（无 Outline 字样、无 vendored spec、无 OAuth 细节）。违反 = 架构腐化，PR 必拒。
- **所有 HTTP 请求必须经唯一请求通道**（engine 的 execute 路径）：本地校验、429 退避、错误映射、token 续期都只实现在这一处。任何命令不得自己拿 reqwest 发请求。例外只有以下三条，各自独立封装，且**新增例外必须在此登记**：
  1. OAuth token 端点交互；
  2. 附件 S3 上传；
  3. `spec sync` 的公开文档抓取（`engine::fetch`，2026-08-26 加入）。
- 第 3 条例外的理由与附加义务（其余两条同理适用「独立封装」要求）：
  - **为什么不能走 execute**：spec 文档在第三方主机（CDN/镜像），用户对它没有凭证。走带 Bearer 的通道意味着要么把 token 发给第三方（凭证泄漏），要么给唯一通道加一个「有时不带凭证」的条件分支——在安全关键路径上引入条件，比多一条通道更难审查。
  - **不带 token ≠ 可以没有退避**：该通道**必须**复用 engine 的 `RetryPolicy` 与 `Throttle` 原语（429 按 Retry-After/带抖动退避重试、重试耗尽为独立错误、每次尝试都过节流），行为与唯一通道一致。
  - **错误域必须分开**：文档主机不是 API，其 401/403/超时不得经 Outline 的错误映射（否则会提示用户检查 `OUTLINE_API_KEY`/`OUTLINE_URL`，而两者根本没参与）。用独立错误类型，退出码语义不变、文案各自负责。
  - **响应体不可信**：不回显第三方错误页、限制读取体积、校验编码，解析前一律视为敌意输入。
  - 该通道内只允许存在一处 `.send()`；`crates/otl/tests/no_phone_home.rs` 断言 reqwest 与裸 socket 只出现在这两条通道所在文件。
- build.rs 只做 spec → IR 编译；IR 是静态数据表，禁止走"每端点生成函数"路线。
- 运行时禁止解析 OpenAPI YAML（唯一例外：`crates/otl/src/commands/spec.rs` 这一个模块——`spec sync` 用它编译并落缓存，`otl doctor` 用它的 `upstream_table()` 在内存里编译一次做差异比对、**不写缓存**。两者都只在用户显式敲命令时发生。例外的归属是「这一个模块」而不是「这一个命令」：新的调用方必须复用同一个入口，否则 `tests/no_phone_home.rs` 对 `fetch_document` / `UPSTREAM_SPEC_URL` 的文件收敛规则会当场拒绝。）

### Testing Rules

- TDD：先写失败测试再实现；整体覆盖 80%+。
- 单测用 wiremock 模拟 Outline API，禁止单测打真实网络；契约测试（打真实 workspace）只在 CI 专属 job，用 `OUTLINE_TEST_*` env 且缺省跳过。
- 输出渲染一律 golden file 测试；CLI 行为用 assert_cmd 端到端。
- 测试实例地址等敏感值不进代码，从 env 注入。

### 安全与凭证规则

- 凭证只存唯一的凭证文件（配置目录下 `credentials.toml`，与 `config.toml` 分离）；除该文件外，任何凭证禁止落其他文件/日志/错误消息（含 debug 输出、doctor 报告）。凭证文件之外的位置一律视为泄漏。
- 凭证文件规则（不可妥协）：创建即以 0600 打开（禁止"先创建再 chmod"）；读取前校验权限，过宽即拒用并给修复命令；写入走同目录 temp → fsync → rename 原子路径，temp 同为 0600；Windows 无权限位，依赖 profile 目录 ACL 且必须在 `auth info`/`doctor` 明示。
- 凭证写入必须原子；token 刷新必须单飞（并发请求只触发一次刷新）。
- 原子写的临时文件名**必须随机**且以 `create_new`（O_EXCL）创建：可预测的名字（PID 等）能被预放 symlink 劫持写入目标，预放普通文件还会让 mode 参数失效（不创建文件的 open 不设权限）。用 `tempfile` 的 `NamedTempFile`（Unix 上直接 0600，失败时 drop 即清理）而非手拼名字。
- refresh_token 每次轮换：刷新成功后旧 token 立即作废，持久化失败必须显式报错而非静默。
- 禁止任何 phone home：无遥测、无自动更新检查、无未经用户命令的网络请求。

### CLI 行为契约

- 所有输出遵守双态：stdout 为数据（人类可读或 --json），stderr 为诊断/警告/进度；非 TTY 自动去色去分页。
- 退出码表是公共 API：新增错误类型必须登记退出码文档，已发布退出码不得改义。
  `docs/exit-codes.md` 是唯一来源，README 与 `crates/otl/skill/SKILL.md` 里的表都是它的生成块
  （`crates/otl/tests/exit_code_tables.rs`，`UPDATE_EXIT_CODE_TABLES=1` 重写）。新增码要同时填
  **Agent summary** 列——skill 的表由 `Meaning: Agent summary` 拼出，只写 Meaning 等于没写。
- 终止性错误在 JSON 态是 stderr 上的对象 `{"error":{exit_code,code,message}}`（`src/failure.rs`），
  `code` 即 `ExitCode::name()`，不是第二套分类法。clap 自己的用法错误与非终止警告仍是散文，因此
  调用方只能把退出码当成"一定存在"的事实。
- 精选命令的 flag 与输出格式变更受 semver 约束；`otl api` 输出明示不稳定。
  `otl api list --json` / `describe --json` 的 `curated_command` 字段是两者之间的反向索引，表在
  `crates/otl/src/commands/api/curated.rs`，由 `tests/curated_index.rs` 双向钉住。
- 分页永不静默截断：截断必有 stderr 警告。

### `--help` 与 SKILL 是契约，不是文档（都有机器守着）

- **每个 `#[arg]` 都必须有文档注释。** 空 doc comment 照样编译、照样出现在 `--help` 里、后面什么都
  没有——读者比"这个 flag 不存在"更糟，因为它看起来是有说明的。`tests/help_coverage.rs` 遍历
  `otl::cli::Cli` 的整棵命令树断言这一点。
- **每个会打印数据的命令都必须在 `after_long_help` 里声明 `JSON shape:` 段。** 脚本绑定的是形状，
  而形状恰恰是过去没人写的部分：`otl docs list` 有两种形状（带 query 走 documents.search，元素是
  `{context, ranking, document}`；不带走 documents.list，元素直接是 document），四个命令返回自己
  拼的对象而非 operation 的对象。同一个测试守这条；例外要进 `NO_DATA_OUTPUT` /
  `DOCUMENTED_ELSEWHERE` 并写明理由。
- **`Cli` 定义住在 `crates/otl/src/cli.rs`（库里）而不是 `main.rs`**，就是为了让上面这些测试能用
  `CommandFactory` 遍历真正发布的那棵树。测试里另写一份副本 = 守着副本。
- **SKILL.md 里出现的每条命令行都必须真实存在。** `tests/skill_surface.rs` 抽出所有 fenced 代码块
  里的 `otl …` 行，逐个核对子命令路径、flag 名、value-enum 取值，以及它提到的每个 `OUTLINE_*` 变量。
  注意它用 `Cli::command()` 之后必须 `build()`：未 build 的树上 `--json` 这类 global flag 只挂在
  root，也没有 `--help`/`--version`。

### Development Workflow Rules

- Conventional commits（feat/fix/refactor/docs/test/chore/perf/ci），无 AI attribution。
- 文件 <800 行（典型 200-400），函数 <50 行，嵌套 <4 层；按 feature 组织模块。
- 硬编码值一律提为常量或配置（端口清单、重试参数、缓存路径等）。
- MVP 顺序不可跳：先 documents.* 子集打通 IR 管线，再铺全端点，再抛光命令。

### Critical Don't-Miss Rules（反模式清单）

- 不要给精选命令手写响应结构体以外的每端点渲染代码。通用渲染必须 schema 驱动。
- 不要在 oneOf/anyOf 参数上硬造 flag 映射。直接引导 `--body`。
- 不要在 Windows 上假设 Unix 路径与权限行为。路径一律经 directories；权限位是 Unix-only，Windows 分支必须显式处理而非假装成功。
- 不要假设 `fs::rename` 在 Windows 上会替换已存在的目标——它不会，第二次写入直接失败。原子替换走 `tempfile::NamedTempFile::persist`（内部用 `MOVEFILE_REPLACE_EXISTING`）或显式 `#[cfg(windows)]` 分支；且「替换已有文件」必须有测试（CI 三平台矩阵会跑到）。
- 不要把来自不受信 spec/服务器的文本直接打到终端。控制字符（ANSI/OSC 转义、`\r`、`\n`、`\t`、bidi override）要么丢弃（纯展示文本），要么整体拒绝（有语义的标识符），并且长度必须有上限。
- **`--json` 的清洗豁免只覆盖「服务器发来的 payload」这一条路径**（`render::render`，理由是必须逐字节
  round-trip，由 `render_golden` 钉住）。**otl 自己撰写的 JSON 不在豁免内**——`otl doctor` 的报告、
  `otl api describe` 的契约、以及**每一个 `otl auth` 结果**（其 `account`/`workspace`/`scope` 来自
  服务端）都掺了外来文本，没有任何东西 round-trip 它们，一律走 `render::render_json_scrubbed`；
  human 形态若是「自己拼的行列表」，逐行过 `stdio::scrub_to_one_line`（外来值不得伪造出一行）。
  三处都曾按「`--json` 就是 payload」类推而漏掉清洗，且是分三轮审查才找齐的
  （describe 设计时、doctor 在 4.6 R1、auth 在 4.6 R2）。守卫是
  `crates/otl/tests/authored_json.rs` 的登记表：新增任一渲染器的调用点都必须在 `EXEMPT` /
  `SCRUBBED` / `RENDER` 里登记并写明理由，否则测试当场红。已知的知情例外只有一个：
  `otl docs export --json` 逐字携带 document id（Story 3.6 的决定，脚本要拿它重试）。
- 不要实现非目标清单里的东西（pull/push 同步、TUI、watch、MCP、device flow、离线队列）。
- DCR 注册后必须持久化 registration_access_token。丢了服务器上就删不掉了。

---

## Usage Guidelines

**For AI Agents:**

- 实现任何代码前先读本文件。
- 严格遵守所有规则；拿不准时选更严格的一边。
- 出现新模式时更新本文件。

**For Humans:**

- 保持精简，只留 agent 会漏的内容。
- 技术栈变化时更新；定期清理已成常识的规则。

Last Updated: 2026-08-27 (agent surface: `--help` 与 SKILL.md 纳入机器守卫，新增 `JSON shape:` 段、
`curated_command` 反向索引、JSON 态结构化错误；`Cli` 迁入 `src/cli.rs`)
Previous: 2026-08-27 (Story 4.6: `--json` 清洗豁免的范围收窄为「服务器响应」，自撰 JSON 必须 scrub)
Previous: 2026-08-26 (Story 4.3: 运行时 OpenAPI 解析例外的归属从「命令」改述为「模块」，doctor 复用同一入口且不写缓存)
