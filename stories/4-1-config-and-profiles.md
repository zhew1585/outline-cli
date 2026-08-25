# Story 4.1: 配置与 profile

Status: review

## Story

As a 拥有多个 workspace 的用户,
I want 命名 profile 切换实例,
so that 工作/个人/自托管随手切。

## Acceptance Criteria

1. **Given** 用户配置文件（TOML）定义多个 profile（base URL、认证方式）
   **When** `--profile work` 或 `OUTLINE_PROFILE=work`
   **Then** 请求指向该 profile 的实例与凭证
2. **Given** flag、env、配置文件同时设置同一项
   **When** 解析配置
   **Then** 优先级 flag > env > 配置文件生效
3. **Given** 配置文件不存在 / 未知 profile / TOML 格式错误
   **When** 执行任意命令
   **Then** 前者按纯 env 路径正常工作，后两者在发请求前报可读错误并以文档化退出码（2）退出

## Tasks / Subtasks

- [x] Task 1: 配置文件模型 (AC: 1)
  - [x] `ConfigFile { default_profile, profiles: BTreeMap<String, Profile> }`，`Profile { url, auth }`
  - [x] `deny_unknown_fields`：配置文件里的拼写错误必须报错，不能静默失效
  - [x] 凭证键（`api_key` / `token`）结构性拦截：反序列化为丢弃值的 marker，报错指向 `credentials.toml`
  - [x] 路径经 `directories::ProjectDirs` 解析（Linux/macOS/Windows 三分支由 crate 处理并写入文档注释）
- [x] Task 2: 分层解析 (AC: 2)
  - [x] `Overrides`（flag 层）/ `EnvLayer`（env 层）/ `ConfigFile`（文件层）全部是纯数据
  - [x] `resolve_settings` 为纯函数，逐项（per-key）取 `flag ?? env ?? file`
  - [x] `EnvLayer::from_values` 作为测试缝：测试不改进程环境（`std::env::set_var` 在 Rust 2024 是 unsafe 且线程不安全）
  - [x] 空白 env 值视为未设置（`export OUTLINE_URL=` 不得遮蔽配置文件）
- [x] Task 3: 凭证接口点（不实现凭证存储） (AC: 1)
  - [x] `trait TokenSource`：profile 解析只产出 base URL + 认证方式，token 获取交给实现方
  - [x] v1 唯一实现 `EnvApiKey`（`OUTLINE_API_KEY`），保留 Epic 1 行为
  - [x] `AuthMethod::Oauth` 返回 `UnsupportedAuthMethod`，Epic 2 接上凭证文件即可，无需改动 profile 解析
- [x] Task 4: 错误与退出码 (AC: 3)
  - [x] 新增 `ConfigError` 变体全部映射到退出码 2，登记 `docs/exit-codes.md`
  - [x] TOML 解析错误只报「行号 + 解析器措辞」，不带源码片段（片段会回显用户误放的凭证）
  - [x] 显式命名（`--config` / `OUTLINE_CONFIG`）的文件缺失即报错；默认位置缺失不报错
- [x] Task 5: CLI 接线 (AC: 1, 2)
  - [x] 全局 flag `--profile` / `--url` / `--config`（`main.rs` 追加式改动）
  - [x] `api::run` 接受 `&Overrides`，`Config::from_env()` → `Config::load(overrides)`
- [x] Task 6: 测试 (AC: 1-3)
  - [x] `tests/config_profiles.rs`：29 个解析层单测（tempfile + 数据层，绝不碰真实配置文件）
  - [x] `tests/profile_e2e.rs`：10 个 assert_cmd + wiremock 端到端用例，断言请求真的打到该 profile 的实例
  - [x] 既有 e2e 测试助手加 `OUTLINE_CONFIG=""` + `env_remove("OUTLINE_PROFILE")`，避免读开发者真实配置

## Dev Notes

- **凭证绝不进配置文件**：`config.toml` 的定位是「可分享、可进 git」，`credentials.toml` 才是凭证家。
  因此 `api_key` / `token` 出现在配置文件任何层级都是硬错误。拦截用一个 `DeniedSecret` marker 类型，
  其 `Deserialize` 走 `IgnoredAny`——值从不被物化，也就不可能出现在 Debug、日志或错误消息里。
- **TOML 错误的泄漏面**：`toml::de::Error` 的 `Display` 会渲染带注解的源码片段。用户如果误把 token 写进
  配置文件，`Display` 就会把它打回终端。实现只用 `message()`（解析器措辞，不含源文本）＋自己按 span 算出的
  行号。两个测试钉住这一点（含「未闭合字符串里藏 secret」的 PoC）。
- **优先级必须逐项**：整体覆盖（某层存在就丢弃下层）是最容易写错的地方——`OUTLINE_URL` 只该覆盖 URL，
  不该连带丢掉 profile 选的认证方式。`precedence_is_applied_per_key_not_per_layer` 专门盯这个。
- **`OUTLINE_CONFIG=`（设置但为空）= 不读配置文件**。这是脚本/CI/测试把自己钉在纯 env 路径上的唯一办法，
  否则调用者的 `~/.config/outline-cli/config.toml` 会影响结果。既有 e2e 测试全部改用它，测试因此不再
  读开发者的真实配置（原本会读，属隐性 bug）。
- **`Config` 结构未变**（仍是 `{ base_url, api_key }`），Debug redaction 的既有 5 个测试原样通过；
  profile 名等新信息放在独立的 `Settings` 里，`Settings` 的 Debug 同样只显示 origin。
- **平台差异**：`ProjectDirs::from("", "", "outline-cli")` → Linux `~/.config/outline-cli`、
  macOS `~/Library/Application Support/outline-cli`、Windows `%APPDATA%\outline-cli\config`。
  `directories` 找不到 home 时 `config_dir()` 返回 `None`，此时降级为纯 env 路径而不是报错
  （headless / service account 场景）。测试只断言路径形状（绝对路径 + 文件名），三平台通用。
- **`config_dir()` 是公开 API**：Epic 2 的凭证文件应放在同一目录（`CREDENTIALS_FILE_NAME` 常量已在此声明，
  仅供错误消息与 auth 层复用），避免两个 track 各自实现一遍路径解析。
- **文件大小上限 64 KiB**：配置文件被整体读进内存，管道/设备文件的 metadata 不可信，因此 metadata 与读取
  两处都设上限（与 `--body` 的处理方式一致）。
- **范围克制**：项目级 `.outline.toml`（SPEC 明确推迟）、用户 alias 表、`--auth` flag、写配置文件的命令
  （`otl config set`）都不做。配置只读不写，`toml` 依赖因此只开 `parse` + `serde` feature。

### 故意留下的缺口

- `auth = "oauth"` 的 profile 目前必然报 `UnsupportedAuthMethod`（退出码 2，消息给出改回 `api-key` 的
  指引）。OAuth 是 Epic 2 的范围；本 story 只把接口点留好（`TokenSource`），不实现凭证存储。
- 没有「单 profile 时自动选中」的魔法：必须显式 `default_profile` 或 `--profile`。加了这个便利，第二个
  profile 出现时行为会静默改变。

### References

- [Source: planning/epics.md#Story 4.1、FR24]
- [Source: specs/spec-outline-cli/stack.md#命令与配置（v1 范围，已定）]
- [Source: specs/spec-outline-cli/SPEC.md#Constraints（自托管 base URL 可配置）]
- [Source: project-context.md 安全与凭证规则、Critical Don't-Miss Rules（Windows 路径）]
- [Source: docs/exit-codes.md]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Completion Notes List

- `toml` 实测版本 1.1.4（`+spec-1.1.0`）；`directories` 6.0；`clap_complete` 4.6.9。
- 既有测试助手的改动（6 处）是本 story 发现的既有隐性 bug 的修复：那些测试原本会读开发者的真实
  `config.toml`，一旦真实文件里有 `default_profile`，`missing_url_exits_2_with_example` 之类的断言就会失败。
- 质量门：`cargo fmt --all -- --check` / `cargo clippy --all-targets --all-features -- -D warnings` /
  `cargo test --workspace`（334 通过 0 失败）全绿；`scripts/bench-startup.sh` 实测 3.38 ms（阈值 10 ms）。

### File List

- Cargo.toml, Cargo.lock（新增 toml / directories / clap_complete workspace 依赖）
- crates/otl/Cargo.toml
- crates/otl/src/config.rs（重写）
- crates/otl/src/main.rs（追加全局 flag）
- crates/otl/src/commands/api.rs（`Config::load(overrides)`）
- crates/otl/tests/config_profiles.rs（新增）
- crates/otl/tests/profile_e2e.rs（新增）
- crates/otl/tests/{api_e2e.rs, api_list.rs, api_params.rs, paging_e2e.rs, startup_guard.rs, contract_smoke.rs}（测试助手隔离配置文件）
- docs/exit-codes.md, README.md
