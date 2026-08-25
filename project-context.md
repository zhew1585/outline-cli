---
project_name: 'outline-cli (otl)'
user_name: 'weizhe'
date: '2026-08-25'
sections_completed: ['technology_stack', 'language_rules', 'architecture_boundaries', 'testing_rules', 'security_rules', 'cli_contract', 'workflow_rules', 'dont_miss_rules']
existing_patterns_found: 0
status: 'complete'
rule_count: 30
optimized_for_llm: true
---

# Project Context for AI Agents

_This file contains critical rules and patterns that AI agents must follow when implementing code in this project. Focus on unobvious details that agents might otherwise miss._

规范来源：specs/spec-outline-cli/（SPEC.md、stack.md、failure-modes.md）与 planning/epics.md。
冲突时以 SPEC 为准。

---

## Technology Stack & Versions

- Rust stable（edition 2021+），Cargo workspace：`crates/engine`（通用 OpenAPI RPC 引擎）+ `crates/otl`（Outline UX 层，产出二进制 `otl`）。
- 核心依赖：clap（derive + 运行时动态构建）、serde/serde_json、anyhow（应用层）+ thiserror（engine 库层）、reqwest（rustls-tls，禁用默认 native-tls）、keyring、directories、clap_complete、bincode。
- 测试：wiremock、assert_cmd、golden file 快照；发布：cargo-dist。

## Critical Implementation Rules

### Language-Specific Rules（Rust）

- engine crate 错误用 thiserror 定义类型化错误；otl crate 边界才允许 anyhow；库层禁止 `unwrap()/expect()`（测试代码除外）。
- 不可变优先：默认 `let` 不可变，数据变换返回新值；共享状态用 `Arc<...>` 而非可变全局。
- 禁止 `unsafe`（无豁免）。
- IR 数据结构全部 `#[derive(Serialize, Deserialize)]` 且版本化（bincode 缓存含 schema 版本号，不兼容即整体废弃重建）。

### 架构边界规则（最关键）

- **engine crate 禁止 import/引用任何 Outline 特定内容**（无 Outline 字样、无 vendored spec、无 OAuth 细节）。违反 = 架构腐化，PR 必拒。
- **所有 HTTP 请求必须经唯一请求通道**（engine 的 execute 路径）：本地校验、429 退避、错误映射、token 续期都只实现在这一处。任何命令不得自己拿 reqwest 发请求（唯二例外：OAuth token 端点交互、附件 S3 上传，各自独立封装）。
- build.rs 只做 spec → IR 编译；IR 是静态数据表，禁止走"每端点生成函数"路线。
- 运行时禁止解析 OpenAPI YAML（唯一例外：`spec sync` 命令路径）。

### Testing Rules

- TDD：先写失败测试再实现；整体覆盖 80%+。
- 单测用 wiremock 模拟 Outline API，禁止单测打真实网络；契约测试（打真实 workspace）只在 CI 专属 job，用 `OUTLINE_TEST_*` env 且缺省跳过。
- 输出渲染一律 golden file 测试；CLI 行为用 assert_cmd 端到端。
- 测试实例地址等敏感值不进代码，从 env 注入。

### 安全与凭证规则

- 凭证只存系统钥匙串（keyring）；任何凭证禁止落普通文件/日志/错误消息（含 debug 输出、doctor 报告）。
- 凭证写入必须原子；token 刷新必须单飞（并发请求只触发一次刷新）。
- refresh_token 每次轮换：刷新成功后旧 token 立即作废，持久化失败必须显式报错而非静默。
- 禁止任何 phone home：无遥测、无自动更新检查、无未经用户命令的网络请求。

### CLI 行为契约

- 所有输出遵守双态：stdout 为数据（人类可读或 --json），stderr 为诊断/警告/进度；非 TTY 自动去色去分页。
- 退出码表是公共 API：新增错误类型必须登记退出码文档，已发布退出码不得改义。
- 精选命令的 flag 与输出格式变更受 semver 约束；`otl api` 输出明示不稳定。
- 分页永不静默截断：截断必有 stderr 警告。

### Development Workflow Rules

- Conventional commits（feat/fix/refactor/docs/test/chore/perf/ci），无 AI attribution。
- 文件 <800 行（典型 200-400），函数 <50 行，嵌套 <4 层；按 feature 组织模块。
- 硬编码值一律提为常量或配置（端口清单、重试参数、缓存路径等）。
- MVP 顺序不可跳：先 documents.* 子集打通 IR 管线，再铺全端点，再抛光命令。

### Critical Don't-Miss Rules（反模式清单）

- 不要给精选命令手写响应结构体以外的每端点渲染代码。通用渲染必须 schema 驱动。
- 不要在 oneOf/anyOf 参数上硬造 flag 映射。直接引导 `--body`。
- 不要在 Windows 上假设 Unix 路径/钥匙串行为。一律经 directories/keyring 抽象。
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

Last Updated: 2026-08-25
