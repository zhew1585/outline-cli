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
- [x] Task 3: 凭证接口点与实例作用域 (AC: 1)
  - [x] `trait TokenSource`：profile 解析只产出 base URL + 认证方式，token 获取交给实现方
  - [x] v1 唯一实现 `EnvApiKey`：无 profile 时读全局 `OUTLINE_API_KEY`（Epic 1 行为不变），
        有 profile 时**只**读 `OUTLINE_API_KEY_<PROFILE>`，不回退全局
  - [x] profile 名 → 变量名映射（ASCII 字母数字大写，其余变 `_`）；无法命名时显式报错
  - [x] 两个 profile 映射到同一变量名 → 报错（选中时才检查）
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
  因此 `api_key` / `token` 出现在顶层或某个 profile 里都是硬错误（专用诊断指向 `credentials.toml`）。
  拦截用一个 `DeniedSecret` marker 类型，其 `Deserialize` 走 `IgnoredAny`——值从不被物化，也就不可能
  出现在 Debug、日志或错误消息里。更深层嵌套（如 `[profiles.work.nested]` 里写 `api_key`）由
  `deny_unknown_fields` 以「unknown key」拒绝——同样是硬错误，但不是那条专用诊断；README 措辞已按此收敛
  （R1 顺带指出的措辞不一致）。
- **TOML 错误的泄漏面（R1 finding 3 的处置）**：原实现只去掉了 `Display` 的源码片段，保留 `message()`，
  理由是「parser 措辞不含值」。这个边界是错的，已实测证伪：`auth = "<secret>"` →
  `unknown variant \`<secret>\``；`profiles = "<secret>"` → `invalid type: string "<secret>"`；
  裸键即秘密时 → `unknown field \`<secret>\``。
  **改为：诊断里不出现任何 parser 产出的文本。** 只由三部分组成——(1) 本模块自己写的描述，
  (2) 自己按 span 算出的行号，(3) 完整 schema 的静态文本。parser 的 message 仅用于**分类**
  （前缀匹配），且无法识别的措辞一律落到通用桶（fail safe：不认识就不转发）。
  代价是不再指名出错的键；补偿是把整份 schema 列出来（比指名更有用，且与文件内容完全无关）。
  证据是表驱动测试 `no_config_file_value_is_ever_echoed_into_a_diagnostic`：13 种让解析失败的位置，
  每种都把同一个可识别 secret 放在**值**的位置，断言 Display 与 Debug 都不含它，且仍带行号。
- **诊断里的名字一律过 `sanitize_name`（R1 finding 7 的处置）**：TOML 的 quoted key 可以塞 ESC 与换行，
  原实现里 `available.join(", ")` 与 `[profiles.{profile}]` 是 Display 直出，可向 stderr 注入 ANSI/OSC
  或伪造后续诊断行。现在控制字符换成 U+FFFD、长度截断、名字一律加引号，profile 列表还有条数上限
  （200 个 profile 不会灌满 stderr）。测试断言：输出无 ESC，且没有任何一行以 `error:` 开头
  （伪造诊断行不可能成立）。
- **所有配置层类型都手写 Debug（R1 finding 2 的处置）**：`Overrides` / `Profile` / `ConfigFile` /
  `LoadedConfig` 原来用派生 Debug，会原样吐出 base URL 的 userinfo/path/query，绕过 `Config`/`Settings`
  已做的 redaction。现在四者都手写（URL 只显示 origin），`EnvLayer` 的 per-profile 密钥表只显示条数。
  测试 `no_configuration_type_leaks_a_url_through_debug` 把同一个带 userinfo/path/query 秘密的 URL
  灌进全部 7 个类型，逐个断言。
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

### 凭证为什么按实例作用域（R1 finding 1 的处置理由）

R1 指出：`--profile personal` 会把环境里的全局 `OUTLINE_API_KEY`（很可能是 work 的密钥）发给 personal
的实例——跨 origin 凭证披露，且只需要一个 `--profile` 就会发生。原实现的 `EnvApiKey` 完全忽略
`settings.profile`，而 `profile_e2e.rs` 两个 mock server 都断言同一个 Bearer，把错误语义固化成通过条件。

**选择的方案：per-profile 环境变量 + 显式拒绝，不回退全局。**

- 有 profile 时读 `OUTLINE_API_KEY_<PROFILE>`；没有就报错并指名该设哪个变量，退出 2，不发请求。
- 全局 `OUTLINE_API_KEY` 只服务「无 profile」路径（Epic 1 行为一字不变）。
- 全局变量存在但 profile 变量缺失时，错误消息明说「全局变量存在但被故意不用，因为那会把一个 workspace
  的密钥交给另一个 workspace 的服务器」——用户不会以为是 bug。

**为什么不选其他方案：**

- 「回退全局 + 警告」：警告不阻止请求，密钥已经发出去了。凭证泄漏不可撤回，警告只能用于不可撤回性不成立的
  情况（见下面 URL 影子警告）。
- 「配置文件里写 `api_key_env = "..."` 指定变量名」：能免掉重命名成本，但引入新的配置键与新的解析面，
  而收益只是省一次 `export`。已否决，需要时再加不破坏兼容。
- 「按 origin 而不是按 profile 绑定凭证」：v1 没有 per-origin 凭证存储，那是 Epic 2 凭证文件的形状。

**边界（与 Epic 2 track 不重叠）**：本 story 只改「env API key 在 profile 切换下的语义」，没有实现任何
凭证文件读写。`TokenSource` 仍是唯一接口点；Epic 2 的 profile-scoped 凭证实现直接作为第二个 impl 接上，
`EnvApiKey` 的规则不需要成为它的约束。

**已知代价（接受）**：原来只有 `OUTLINE_API_KEY` 的用户，一旦使用 `default_profile` 或 `--profile`，
必须改成带后缀的变量名。错误消息把该设的变量名直接打出来，且项目处于 pre-1.0（README 明示接口可能变化）。

**测试**：`profile_e2e.rs` 的 mock server 现在只接受**自己的** Bearer，其他一律 401；新增
`one_profiles_key_is_never_sent_to_another_profiles_instance` 断言退出 2 且
`received_requests()` 为空——即「密钥根本没上路」，而不只是「响应失败」。

### env URL 与 profile URL 不一致时只警告不拒绝

`OUTLINE_URL` 仍然按 AC 的优先级压过 profile 的 `url`（逐项 flag > env > file 不变），但此时 profile 的
凭证会发往 profile 没有声明的实例，因此 stderr 出一条警告。这里选警告而非拒绝，因为两个变量都在同一个
环境里由同一个用户设置，且凭证仍是该 profile 自己的——与 finding 1 的「一个 workspace 的密钥去另一个
workspace」不同级。`--url` 是「我就是要重定向」的显式表达，不警告。警告不打印 URL 本身（base URL 的
path/query 可能带凭证）。

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

### R1 对抗审查处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| 1 | BLOCKER | 已修：per-profile `OUTLINE_API_KEY_<PROFILE>`，不回退全局；错误测试改为「密钥没上路」断言 |
| 2 | BLOCKER | 已修：`Overrides`/`Profile`/`ConfigFile`/`LoadedConfig` 手写 Debug，全部只显示 origin |
| 3 | BLOCKER | 已修：诊断不再含任何 parser 文本，只有自有描述 + 行号 + 静态 schema；13 例表驱动测试 |
| 7 | MAJOR | 已修：`sanitize_name` 用于所有进诊断的名字；列表有条数上限；无 ESC / 无伪造行断言 |
| — | 顺带 | 已修：README 关于嵌套凭证键的措辞收敛为「顶层或 profile 内」 |

### File List

- Cargo.toml, Cargo.lock（新增 toml / directories / clap_complete workspace 依赖）
- crates/otl/Cargo.toml
- crates/otl/src/config/{mod.rs, error.rs, file.rs}（重写；R1 修复后单文件超 800 行铁律，按职责拆为三个模块）
- crates/otl/src/main.rs（追加全局 flag）
- crates/otl/src/commands/api.rs（`Config::load(overrides)`）
- crates/otl/tests/config_profiles.rs（新增）
- crates/otl/tests/profile_e2e.rs（新增）
- crates/otl/tests/{api_e2e.rs, api_list.rs, api_params.rs, paging_e2e.rs, startup_guard.rs, contract_smoke.rs}（测试助手隔离配置文件）
- docs/exit-codes.md, README.md
