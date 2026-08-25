# Story 1.1: 首次端到端调用

Status: done

## Story

As a 使用 API key 的开发者,
I want 在全新环境用 `otl api documents.info id=<id>` 调通我的 Outline 实例,
so that 引擎最小闭环（IR 编译 + 分发器 + 认证 + 请求）被真实验证。

## Acceptance Criteria

1. **Given** Cargo workspace 含 engine 与 otl 两 crate，vendored spec 已入库
   **When** 执行 `cargo build`
   **Then** build.rs 将 spec 中 documents.* 子集编译为 IR 静态表并嵌入二进制，构建成功
2. **Given** 已设置 `OUTLINE_API_KEY` 与 base URL（env `OUTLINE_URL`）
   **When** 执行 `otl api documents.info id=<真实文档id>`
   **Then** 以 POST JSON 携带 Bearer 头发起请求，stdout 输出响应 data 部分
3. **Given** 未设置 `OUTLINE_API_KEY`
   **When** 执行任意 api 命令
   **Then** 发起网络请求前报出可读错误并以非零退出码退出

## Tasks / Subtasks

- [x] Task 1: Cargo workspace 脚手架 (AC: 1)
  - [x] 根 Cargo.toml 定义 workspace，成员 `crates/engine`、`crates/otl`
  - [x] engine 为纯库 crate（thiserror 错误），otl 为二进制 crate（binary name = `otl`，anyhow）
  - [x] rustfmt.toml / clippy 基线；`#![forbid(unsafe_code)]` 两 crate 都加
- [x] Task 2: vendor 上游 spec (AC: 1)
  - [x] 下载 https://raw.githubusercontent.com/outline/openapi/main/spec3.json 到 `crates/otl/spec/spec3.json`（用 JSON 版，见 Dev Notes）
  - [x] 记录 vendor 来源 commit/日期到 `crates/otl/spec/VENDOR.md`
- [x] Task 3: engine IR 数据结构 (AC: 1)
  - [x] `OpSpec { name, path, params }`、`ParamSpec { name, ty, required }`、`ParamType { String, Integer, Boolean, Number, Json }`
  - [x] 全部 derive Serialize/Deserialize + 常量 `IR_SCHEMA_VERSION`
  - [x] engine 不出现任何 Outline 字样（架构红线）
- [x] Task 4: build.rs IR 编译管线 (AC: 1)
  - [x] otl 的 build.rs：serde_json 解析 spec3.json，只筛选 path 以 `/documents.` 开头的 operation
  - [x] 从 requestBody schema 提取参数名/类型/必填（只处理标量，本 story 忽略复杂类型，标记为 Json）
  - [x] 产出 Rust 源码或 bincode blob 到 OUT_DIR，`include!`/`include_bytes!` 进二进制
  - [x] `cargo:rerun-if-changed=spec/spec3.json`
- [x] Task 5: engine 请求通道最小版 (AC: 2)
  - [x] `Client::new(base_url, token)` + `execute(op: &OpSpec, args: &[(String, String)]) -> Result<serde_json::Value, EngineError>`
  - [x] k=v 组装 JSON body（本 story 只做 string 直通，类型转换留给 Story 1.3，但结构上按 ParamType 分发）
  - [x] reqwest blocking + rustls：`reqwest = { version = "0.13", default-features = false, features = ["blocking", "json", "rustls-tls"] }`
  - [x] POST `{base}/api/{op.name}`，头：authorization Bearer / content-type / accept 均 application/json
- [x] Task 6: otl CLI 入口 (AC: 2, 3)
  - [x] clap：`otl api <operation> [k=v...]`，operation 候选来自 IR 表
  - [x] 配置读取：`OUTLINE_URL`（必填，报错含设置示例）、`OUTLINE_API_KEY`（缺失时不发请求，stderr 可读错误，退出码 2）
  - [x] 成功输出响应 JSON 的 `data` 字段（无 data 则整体）到 stdout，pretty print
- [x] Task 7: 测试 (AC: 1-3)
  - [x] wiremock 集成测试：模拟 `/api/documents.info` 返回 Outline 信封 `{"data": {...}}`，断言请求方法/头/体
  - [x] assert_cmd：缺 OUTLINE_API_KEY → 退出码 2 + stderr 含提示；缺 OUTLINE_URL 同理
  - [x] build.rs 管线单测：对 vendored spec 断言 documents.* 操作数 > 10 且含 documents.info/documents.search

## Dev Notes

- **先读 `project-context.md`**，全部 30 条规则适用。本 story 最相关的红线：engine 禁止 Outline 特定内容；HTTP 只走 engine 唯一通道；build.rs 只产数据表不产函数；库层禁 unwrap。
- **vendor JSON 而非 YAML**：上游仓库根目录同时有 `spec3.yml` 与 `spec3.json`（2026-08 确认）。用 JSON 让 build.rs 只依赖 serde_json，绕开已弃用的 serde_yaml；`openapiv3` crate（2.2.0）可选用于强类型解析，但本 story 手工按 JSON path 提取即可（只需 paths.*.post.requestBody + operation path），减少 build 依赖。
- **依赖版本（2026-08-25 crates.io 实查）**：clap 4.6、reqwest 0.13（rustls-tls）、serde 1.0.229、thiserror 2.0、anyhow 1.0、wiremock 0.6、openapiv3 2.2（可选）。**bincode 已是 3.x，serde 集成需 `features = ["serde"]` 且 API 与 1.x 不同**；本 story 若直接 codegen Rust 源码进 OUT_DIR 可完全不用 bincode（bincode 留给 Story 4.2 的 spec sync 缓存）。
- **Outline API 事实**：全部端点 POST `{base}/api/<resource>.<method>`，请求响应均 JSON，成功信封 `{"data": ...}`（部分带 `pagination`）。认证 `Authorization: Bearer <key>`。真实测试环境 base URL 从 env 注入（禁止写入代码/测试，见 project-context 测试规则）。
- **启动预算**：IR 用静态表（codegen 的 `static OPS: &[OpSpec]` 或 include_bytes + 惰性解析）。本 story 用 codegen 源码路线最简单且零运行时开销。
- **退出码**：本 story 先定 3 个并写入 `docs/exit-codes.md` 起步：0 成功、1 通用/网络错误、2 用法/配置错误。Story 1.4 会扩展该表，勿硬编码散落，集中一个 `ExitCode` enum。
- **blocking vs async**：用 reqwest blocking，CLI 无需 tokio 运行时（启动预算友好）。若后续 export 并发需要再引入。
- **范围克制**：类型转换（1.3）、错误映射（1.4）、表格输出（1.5）、分页（1.6）、退避（1.7）都不在本 story - 结构上留好位置（execute 单通道、ParamType enum）但不实现。

### Project Structure Notes

```
outline-cli/
  Cargo.toml              # workspace
  rustfmt.toml
  crates/
    engine/               # 通用库：IR 类型、Client、execute、EngineError
      src/lib.rs
      src/ir.rs
      src/client.rs
      src/error.rs
    otl/                  # 二进制：clap 入口、配置读取、输出
      build.rs            # spec3.json -> IR codegen
      spec/spec3.json     # vendored
      spec/VENDOR.md
      src/main.rs
      src/config.rs
      src/commands/api.rs
      tests/api_e2e.rs    # wiremock + assert_cmd
  docs/exit-codes.md
```

- 文件均应远小于 800 行上限；build.rs 若超 200 行考虑抽 `crates/otl/build/` 模块。

### References

- [Source: specs/spec-outline-cli/SPEC.md#Capabilities CAP-3/CAP-4、#Constraints]
- [Source: specs/spec-outline-cli/stack.md#架构、#spec 供给、#依赖基线]
- [Source: specs/spec-outline-cli/failure-modes.md #1 #9]
- [Source: planning/epics.md#Story 1.1]
- [Source: project-context.md 全文]
- 上游 spec: https://github.com/outline/openapi （spec3.json / spec3.yml）

## Dev Agent Record

### Agent Model Used

claude-fable-5 (Claude Code agent), 2026-08-25

### Debug Log References

- 无阻塞性调试问题。
- 唯一环境问题：首次 `cargo test -p engine` 编译因资源限制被中断（exit 137），重跑通过。

### Completion Notes List

- 偏差 1：reqwest 0.13 已将 feature `rustls-tls`（0.12 命名）改名为 `rustls`，另需 `webpki-roots` 提供信任根。
  story 文本按 0.12 命名书写，按 0.13 实际 feature 落地，语义不变（rustls 后端、禁用默认 native-tls）。
- 偏差 2（微小）：wiremock 0.6 需要 async 运行时，测试用 `#[tokio::test(flavor = "multi_thread")]` + `spawn_blocking` 包裹 blocking 客户端/CLI 进程；生产代码仍是纯 blocking，无 tokio 依赖。
- IR 表走 codegen Rust 源码路线（`static OPS: &[engine::ir::OpSpec]`），未用 bincode（留给 Story 4.2）。
- IR 字段用 `Cow<'static, str>` / `Cow<'static, [ParamSpec]>`：静态表零拷贝构造，同时保持 Serialize/Deserialize。
- 生成代码含 `const _: () = assert!(engine::ir::IR_SCHEMA_VERSION == 1);` 编译期锁定 schema 版本。
- vendored spec 实测含 30 个 documents.* 操作；未知参数按 Json/字符串直通（本地校验属 Story 1.4 范围）。
- 质量门全绿：cargo build / cargo test（17 通过 0 失败）/ cargo clippy --all-targets -- -D warnings / cargo fmt --check。
- 架构红线验证：`grep -ri outline crates/engine/` 零命中。

### File List

- Cargo.toml, Cargo.lock, rustfmt.toml
- crates/engine/Cargo.toml
- crates/engine/src/{lib.rs, ir.rs, client.rs, error.rs}
- crates/engine/tests/execute.rs
- crates/otl/Cargo.toml
- crates/otl/build.rs
- crates/otl/spec/{spec3.json, VENDOR.md}
- crates/otl/src/{main.rs, lib.rs, ops.rs, config.rs, exit.rs}
- crates/otl/src/commands/{mod.rs, api.rs}
- crates/otl/tests/{api_e2e.rs, ir_table.rs}
- docs/exit-codes.md
