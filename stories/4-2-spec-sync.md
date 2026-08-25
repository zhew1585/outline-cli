# Story 4.2: spec sync

Status: review

## Story

As a 想用最新端点的用户,
I want `otl spec sync` 拉取上游 spec,
so that 新端点无需等 CLI 发版。

## Acceptance Criteria

1. **Given** 执行 `otl spec sync`
   **When** 上游有更新
   **Then** 运行时解析一次并以 bincode 落缓存（key 为 spec hash + CLI 版本 + IR schema 版本，原子 rename 写入），`otl api list` 立即含新端点
2. **Given** 缓存文件损坏
   **When** 任意命令启动
   **Then** 自动废弃缓存回退内置 IR，不崩溃
3. **Given** 未执行 `spec sync`
   **When** 运行任意命令
   **Then** 绝不联网检查 spec、绝无自动更新检查（NFR4），且启动路径零 OpenAPI 解析、`otl --help` 冷启动 <10ms（NFR1）

## Tasks / Subtasks

- [x] Task 1: 共用 spec 编译器 (AC: 1, 3)
  - [x] 新 crate `crates/speccompile`（package `spec-compile`）：OpenAPI JSON → 中性 IR 数据结构
  - [x] build.rs 与运行时同一套解析代码；build.rs 只保留静态表渲染
  - [x] panic 全部改为 typed error（`CompileError`），递归深度受限（引用环不再是挂死或 panic）
  - [x] 操作名/请求路径安全校验（`is_safe_op_name` / `is_safe_path`）
  - [x] `tests/spec_parity.rs`：vendored spec 编译期产物与运行时产物逐字段相等
- [x] Task 2: 文档下载通道 (AC: 1)
  - [x] `engine::fetch::fetch_document`：单次无认证 GET、16 MiB 上限、UTF-8 校验、错误响应体不回显
  - [x] URL 校验（http/https、有 host、无 userinfo），错误信息不回显 URL
  - [x] 新增 `EngineError::InvalidDocumentUrl`（退出码 2）与 `EngineError::UnusableDocument`（退出码 1）
- [x] Task 3: bincode IR 缓存 (AC: 1, 2)
  - [x] `crates/otl/src/spec/cache.rs`：magic + 布局版本 + SHA-256 + bincode body
  - [x] 缓存 key 三要素：spec hash（provenance）+ CLI 版本 + IR schema 版本，任一不符整体废弃
  - [x] 位置经 `directories` 解析（Linux `~/.cache/outline-cli`、macOS `~/Library/Caches/outline-cli`、Windows `%LOCALAPPDATA%\outline-cli\cache`）；`OTL_CACHE_DIR` 覆盖
  - [x] 写入：同目录 temp → fsync → rename；Unix 上直接以 0600 创建；失败清理 temp
  - [x] 读取：尺寸上限 + bincode limit + 校验和 + 版本 + 逐 op 路径安全校验
- [x] Task 4: 运行时表解析 (AC: 1, 2, 3)
  - [x] `ops::table()`：`OnceLock` 惰性解析，缓存优先、内置兜底
  - [x] 缓存不可用 → stderr 一行警告（区分 damaged / outdated）+ 修复命令，退出码不受影响
  - [x] `--help`/`--version` 不触发任何缓存 I/O
- [x] Task 5: 命令 (AC: 1)
  - [x] `otl spec sync [--url URL] [--spec PATH] [--force]`、`otl spec reset`
  - [x] 双态输出：`--json` 结构化报告，TTY 人类可读；进度到 stderr
  - [x] 同 hash 且缓存可用 → 不重写（`--force` 强制）；损坏缓存必重建
  - [x] provenance 只记 origin / `local file`，不落完整 URL 或文件路径
- [x] Task 6: 测试与文档 (AC: 1-3)
  - [x] wiremock 端到端：sync 后 `api list` 含新端点且可 dispatch
  - [x] 损坏/截断/空/异物/目录占位缓存 → 全部退 0 回退内置
  - [x] 版本不匹配（布局版本 / IR schema / CLI 版本）→ 废弃且判定为 outdated
  - [x] `tests/no_phone_home.rs`：网络入口收敛 + 本地命令在出口全断时仍成功
  - [x] `docs/exit-codes.md` 登记新错误类；README 更新（唯一通道表述、spec 生命周期）

## Dev Notes

- **先读 `project-context.md`**。本 story 触碰的红线：engine 禁止 Outline 内容、运行时禁止解析 OpenAPI（唯一例外
  `spec sync`）、库层禁 unwrap、IR 版本化、缓存写入原子、不 phone home。
- **共用编译器为什么是新 crate**：build script 只能用 build-dependencies。把编译器放进 `engine` 会把
  reqwest/rustls 拖进 host 构建（三平台 CI 上很贵）；`spec-compile` 只依赖 serde_json + thiserror。代价是它的
  `BodyKind`/`ScalarKind` 是 `engine::ir` 的镜像枚举，由 `otl::spec::to_ir` 单点穷举映射 + parity 测试兜住。
- **为什么 `--spec` 挂在 `spec sync` 而不是全局 flag**：stack.md 要求「`--spec` 允许开发时覆盖」，但全局 flag 意味着
  运行时解析 OpenAPI 的第二条路径，与 NFR1 与 startup guard 的约束冲突。`spec sync --spec <path>` 同样满足开发覆盖
  语义（编译一次落缓存，之后所有命令生效），且解析仍只发生在 sync 这一条路径。`otl spec reset` 退出该状态。
- **缓存 key 里的 spec hash**：加载时无法用 hash 做查找（运行时手里没有 spec 可以算 hash），所以 hash 是 provenance +
  「是否需要重写」的判据，CLI 版本与 IR schema 版本才是准入校验。三者都在 header 里。
- **缓存是独立信任边界**：文件可能被截断、位翻转、被别的进程写、被上一版 CLI 留下。加载顺序为
  尺寸 → magic → 布局版本 → 校验和 → bincode → IR schema 版本 → CLI 版本 → 每个 op 的 name/path 安全性。
  任一失败即整体废弃，**不做迁移**。
- **路径校验是安全需求不是洁癖**：engine 以 `format!("{base}{path}")` 拼 URL。若 IR 里出现 `@evil.example/x`，
  `https://host` + 该 path = `https://host@evil.example/x`，host 变成 userinfo，Bearer token 直接送给攻击者。
  因此下载的 spec 与缓存文件两侧都强制「纯绝对路径」白名单（禁 `@ : ? # % //` 与 `..`）。
- **两条 HTTP 通道的取舍**：spec 下载既不能带 token（第三方主机），也没有 429 退避/节流/信封映射的意义，塞进
  `Client::send` 会污染那条通道的不变量。所以 HTTP 仍全部在 engine 内，但分成「认证请求通道」与「明文文档通道」两处，
  由 `tests/no_phone_home.rs` 断言只有这两处出现 `.send()`。
- **启动预算**：实测 `otl --help` 3.64 ms（阈值 10 ms，release，hyperfine -N，794 runs）；缓存解析只在真正需要
  操作表时发生，实测 113 op / 16 KB 缓存约 +0.3 ms（`api list` 6.0 → 6.3 ms）。release 二进制 2.65 MB。
- **stale 也会警告**：CLI 升级后缓存必然失效。静默回退会让「昨天还能用的端点今天报 unknown operation」变成谜题，
  所以 stale 与 damaged 都出一行 stderr 警告并给出 `otl spec sync` / `otl spec reset`。
- **不在范围内**：doctor（4.3）、profile 体系（4.1）、OAuth（Epic 2）、overlay 文件（x-cli 扩展）落地。
  overlay 只在编译器留了 `CompileOptions` 这个扩展点，本 story 不实现。

### Project Structure Notes

```
crates/
  speccompile/            # 新：共用 OpenAPI -> IR 编译器（无 vendor 特定内容）
    src/{lib.rs, schema.rs}
  engine/
    src/fetch.rs          # 新：明文文档通道
    src/error.rs          # +2 variants
  otl/
    build.rs              # 瘦身：只渲染静态表
    src/spec/{mod.rs, cache.rs}   # 新：spec 生命周期 + bincode 缓存
    src/ops.rs            # 表解析（缓存优先，内置兜底）
    src/commands/spec.rs  # 新：sync / reset
    tests/{spec_cache.rs, spec_sync_e2e.rs, spec_parity.rs, no_phone_home.rs}
```

### References

- [Source: specs/spec-outline-cli/SPEC.md#CAP-7、#Constraints（启动 <10ms、不 phone home、overlay 不 fork）]
- [Source: specs/spec-outline-cli/stack.md#spec 供给、#架构（spec sync 路径）]
- [Source: specs/spec-outline-cli/failure-modes.md #1 #8]
- [Source: planning/epics.md#Story 4.2、FR22、NFR1、NFR4]
- [Source: docs/exit-codes.md]
- [Source: project-context.md 全文]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Debug Log References

- bincode 3.0.0 在 crates.io 上是**空占位包**（无 feature、无依赖）；实际可用的 serde 集成是 2.0.1
  （`bincode::serde::{encode_to_vec, decode_from_slice}` + `config::standard().with_limit::<N>()`）。
  Story 1.1 的 Dev Notes 说「bincode 已是 3.x」，据实改用 2.0。
- `engine::base_url_origin` 拒绝带 query 的 URL（它是 base URL 校验器），不能用来取 spec URL 的 origin；
  为此在 `engine::fetch` 暴露 `document_origin`。
- `directories` 在 macOS 上不认 `XDG_CACHE_HOME`，所以测试覆盖走专用的 `OTL_CACHE_DIR`。

### Completion Notes List

- 偏差 1：`--spec` 实现为 `spec sync --spec <path>` 而非全局 flag（理由见 Dev Notes）。
- 偏差 2：额外加了 `otl spec reset`。没有它，用户 sync 到一份坏 spec 后无法自助恢复（需要知道缓存路径去手删）。
- 偏差 3：为缓存错误在 otl 引入 thiserror（anyhow 仍是边界类型）。"stale vs damaged" 需要可判定，字符串不够。
- `crates/engine/src/ir.rs` **未改动一行**（IR 已 derive Serialize/Deserialize 且已有 `IR_SCHEMA_VERSION`，
  `Cow<'static, _>` 反序列化为 Owned 即可）。这是为了不与 Epic 4a 的 schema 驱动列改动冲突。
- `build.rs` 大幅瘦身（432 → 165 行）：解析逻辑整体搬到 `spec-compile`，只留渲染与两个变体名映射函数。
- 共享文件只做追加式改动：`main.rs`（+1 变体 +1 dispatch 行）、`lib.rs`（+1 mod）、`commands/mod.rs`（+1 mod）、
  `errors.rs`（+2 classify 分支）、`engine/src/lib.rs`（+1 mod +文档）。`exit.rs` 未改（无新退出码）。
- `tests/ir_table.rs` 改为显式查内置表（`builtin()` 而非 `ops::find`），否则开发机上一份真实缓存会让它对
  vendored spec 的断言失真。
- `tests/startup_guard.rs`：二进制内容检查从「文件名」改为「文件路径」（上游 URL 恰好以同名文件结尾），
  并加了两条 allowlist（上游 URL 常量、`--spec` flag 名）。守卫语义未削弱：运行时仍不得定位 vendored spec。
- 质量门：`cargo fmt --all -- --check` / `cargo clippy --all-targets --all-features -D warnings` /
  `cargo test --workspace`（341 passed, 0 failed, 1 ignored）/ `scripts/bench-startup.sh`（3.637 ms < 10 ms）全绿。
- 架构红线复查：`grep -ri outline crates/engine crates/speccompile` 零命中。

### File List

- Cargo.toml, Cargo.lock
- crates/speccompile/Cargo.toml, crates/speccompile/src/{lib.rs, schema.rs}
- crates/engine/Cargo.toml（无改动）, crates/engine/src/{lib.rs, error.rs, fetch.rs}, crates/engine/tests/fetch.rs
- crates/otl/Cargo.toml, crates/otl/build.rs
- crates/otl/src/{lib.rs, ops.rs, errors.rs, main.rs}
- crates/otl/src/spec/{mod.rs, cache.rs}
- crates/otl/src/commands/{mod.rs, api.rs, spec.rs}
- crates/otl/tests/{spec_cache.rs, spec_sync_e2e.rs, spec_parity.rs, no_phone_home.rs, ir_table.rs, startup_guard.rs}
- docs/exit-codes.md, README.md
- stories/4-2-spec-sync.md
