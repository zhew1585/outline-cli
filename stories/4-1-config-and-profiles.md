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
- **诊断里的路径一律过 `sanitize_path`（R2 finding 3 的处置）**：R1 只清理了 profile 名，漏了 `--config`
  / `OUTLINE_CONFIG` 的**路径**——`Path::display()` 对非 UTF-8 有损但对控制字节原样放行，因此
  `--config $'/x/\e]8;;http://evil\aFORGED\e]8;;\a\nerror: forged'` 能在终端里种一个可点击超链接并伪造
  一行 `error:`。现在路径与名字走同一套处理（控制字符换 U+FFFD、长度截断，路径上限 200 字符）。
- **诊断里的名字一律过 `sanitize_name`（R1 finding 7 的处置）**：TOML 的 quoted key 可以塞 ESC 与换行，
  原实现里 `available.join(", ")` 与 `[profiles.{profile}]` 是 Display 直出，可向 stderr 注入 ANSI/OSC
  或伪造后续诊断行。现在控制字符换成 U+FFFD、长度截断、名字一律加引号，profile 列表还有条数上限
  （200 个 profile 不会灌满 stderr）。测试断言：输出无 ESC，且没有任何一行以 `error:` 开头
  （伪造诊断行不可能成立）。
- **所有配置层类型都手写 Debug（R1 finding 2 + R2 finding 2 的处置）**：`Overrides` / `Profile` /
  `ConfigFile` / `LoadedConfig` 原来用派生 Debug，会原样吐出 base URL 的 userinfo/path/query，绕过
  `Config`/`Settings` 已做的 redaction。现在四者都手写（URL 只显示 origin），`EnvLayer` 的 per-profile
  密钥表只显示条数。
  R2 指出还漏了一整类载荷：**名字本身**。`default_profile = "<secret>"` 与 `[profiles.<secret>]` 都是
  用户写进文件的值，`ConfigError` 的派生 Debug 还会原样吐出 `name`/`available`/`path`。
  现在的边界写死为：**Debug 里不出现任何 profile 名**（只显示 `***` / `<unset>`），名字只在 `Display`
  出现且过 sanitize + 截断。理由是两者的用途不同——`Display` 是用户为了改自己的文件而必须看到的一次性
  诊断（有界、已清理），`Debug` 是会落进日志、panic 消息与 error chain 的无界机器面，没有「必须看到」的
  需求，所以按最严处理。`ConfigError` 因此改为手写 Debug 转发 Display，彻底关掉第二条未过滤通道。
  测试 `no_configuration_type_leaks_a_url_through_debug`（URL 载荷，7 个类型）、
  `no_configuration_type_leaks_a_profile_name_through_debug`（名字载荷，6 个类型）、
  `config_error_debug_never_exposes_raw_names_or_paths`（4 个变体）。
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

### profile 同时作用于凭证与 origin（R2-1 → R3 裁决后的最终形态）

演化过程记录在此，因为三轮下来这条是全 story 最难的取舍：

1. **R1**：`--profile` 会把全局 `OUTLINE_API_KEY` 发给该 profile 的实例（跨 workspace 泄漏）。
   修法：profile 只读 `OUTLINE_API_KEY_<PROFILE>`，不回退全局。理由是「警告收不回已发出的凭证」。
2. **R2**：审查者指出我对「`OUTLINE_URL` 指向别处」只警告放行，与上一条自相矛盾。我照自己的论据改成
   **在 `resolve_settings` 里拒绝**——即 profile 在场时 env 不再是 URL 的一层。
3. **R3 裁决：采用替代方案，R2 的修法被判 REGRESSION。** 理由我接受：
   - 它删掉了 URL 的 env 层，直接违反未修改的 Story 4.1 AC2（flag > env > file 逐项生效）；
   - 「更早报错」没有实质优势：错误本来就发生在配置装载阶段、秘密与网络请求之前，替代方案给出的错误
     一模一样清晰；
   - 代价是真实的：把环境配置人为分成两类「ambient 程度」破坏了已发布的通用优先级模型，而且用原始
     字符串比较制造了等价 URL 误报（`http://h:9` vs `http://h:9/` 被判成不同实例）。

**最终形态**：解析与释放分离。

- `resolve_settings` 严格 flag > env > file，**每一个键都一样**，包括 base URL；并记录
  `UrlSource`（Flag / Env / Profile）与该 profile 自己声明的 `profile_url`。
- 新增唯一的「凭证释放边界」`release_token`，在把秘密交给请求通道之前问一个**不同的问题**：这个
  origin 是这个凭证的归属地吗？
  - `UrlSource::Flag` → 放行（`--url` 与 `--profile` 写在同一条命令里，是显式重定向）；
  - `UrlSource::Profile` → 放行（origin 由 profile 自己声明）；
  - `UrlSource::Env` 且与 profile 声明的**规范化 origin** 一致 → 放行；
  - `UrlSource::Env` 且不一致 → `ConflictingUrl`，退出 2，密钥根本没被取出；
  - `UrlSource::Env` 且 profile 未声明 url → `UnboundProfileCredential`（无从建立绑定）。
- 比较用 `engine::base_url_origin` 的规范化 origin，因此尾斜杠、host 大小写、默认端口都不会误报
  （R3 finding 4）；路径差异不改变「哪台服务器收到凭证」，因此按 origin 判定。

**检查在共享边界，不在 EnvApiKey 里**（裁决明确要求，Epic 2 合并时会复验这个接缝）：

```rust
pub struct BindingChecked(());              // 私有字段：模块外无法构造
pub trait TokenSource {
    fn fetch(&self, s: &Settings, checked: &BindingChecked) -> Result<String, ConfigError>;
}
pub fn release_token(source: &impl TokenSource, s: &Settings) -> Result<String, ConfigError> {
    let checked = check_credential_binding(s)?;   // 唯一构造 BindingChecked 的地方
    source.fetch(s, &checked)
}
```

`BindingChecked` 只有一个私有字段，所以**模块外根本调不到 `fetch`**。

**R4 补完：不可伪造的令牌 + 可伪造的输入 = 不成立。** R4 指出我只锁了一半——令牌造不出来，但签发令牌
所依据的 `Settings` 是公开结构体，外部可以直接构造 `url_source: UrlSource::Flag` 骗过闸门；更直接的是
`EnvLayer` 的 `api_key` / `profile_api_keys` 本身就是公开字段，压根不用过闸门就能读到秘密。审查者说得
对，我的集成测试自己的 `settings_for` helper 就是这条路径的现成证明。现在三处一起锁：

1. `Settings` 字段私有 + 无公开构造函数 → 只能由 `resolve_settings` 产出（读取走 accessor）；
2. `EnvLayer` 的秘密字段私有且**不提供 accessor** → 秘密只能经 `release_token` 取得（非秘密部分给
   `profile()` / `url()` / `config_path()` 与 `with_*` 构造器）；
3. `BindingChecked` 私有字段 → `fetch` 只能由 `release_token` 调用。

三者缺一，闸门就是装饰。测试用**编译失败**验证（不是运行时断言——伪造路径根本没有能触达的运行时值）：
`the_gates_inputs_cannot_be_forged_from_outside_the_crate` 把 4 段攻击代码用 `rustc` 编译到真实
rlib 上，要求全部因私有性失败，同时先编译一段「合法用法」确保探针本身没坏。已做变异验证：把
`api_key` 改回 `pub` 会立刻得到 `SAFE RUST CAN STILL read the global API key straight off EnvLayer`。

另有 `the_binding_gate_applies_to_every_token_source`（假 source 也被拦）与
`the_gate_still_governs_an_honestly_resolved_flag_redirect`（真 `--url` 仍放行，逃生门没被误伤）。

**三项确认（裁决要求）**：
- AC2 同项三层优先级：`the_url_key_itself_resolves_flag_over_env_over_file`——同一个 URL 键上依次断言
  file → env 覆盖 → flag 覆盖，并断言 `url_source` 记录正确（R2 那个「三层各给一个键」的测试只证明了
  层间不互删，已按 R3 意见换回同项阶梯，并作为独立测试保留）。
- 跨 origin 零请求：`an_env_url_pointing_away_from_the_profile_sends_nothing` 与
  `a_profile_without_a_url_cannot_bind_an_env_url` 都断言 `received_requests()` 为空。
- 共享边界：见上；另有 `no_profile_means_no_binding_question` 证明无 profile 时不受影响。

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

### R2 复核处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| R2-1 | BLOCKER | 已修：`OUTLINE_URL` 与 profile URL 冲突改为拒绝（`ConflictingUrl`）；profile 无 URL 时不再由 env 顶上；e2e 测试反转为断言「另一实例零请求」 |
| R2-2 | BLOCKER | 已修：Debug 不再输出任何 profile 名（含 `default_profile` 与表键）；`ConfigError` 改为手写 Debug 转发 Display，去掉派生 Debug 这条未过滤的通道 |
| R2-3 | MAJOR | 已修：新增 `sanitize_path`，所有进诊断的路径都过它；端到端断言 OSC/BEL/换行不落 stderr |
| R2-6 | MAJOR | 已修：`profile_api_key_suffix(name, case_insensitive)`，Windows 走大小写折叠、POSIX 保持敏感；平台规则做成参数，两种行为在任何平台都可测 |
| R2-7 | MINOR | 已修：`MAX_PROFILE_VAR_CHARS = 64` 在**派生处**设界（截断后的变量名是错误建议），超长 profile 名走 `ProfileApiKeyVarUnnameable` |

### R3 复核处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| 裁决 | REGRESSION | 已按替代方案重做：解析恢复严格 flag > env > file，绑定检查移到共享的 `release_token` 边界，用 `BindingChecked` 私有构造使其无法被绕过 |
| R3-1 | BLOCKER | 同上；并恢复「同一 URL 键的三层优先级」测试 |
| R3-2 | BLOCKER | 已修：`ConfigError` 的 Debug 改为**结构化**（只输出变体名 + 非文本标量），不再转发 Display；Display 侧所有自由文本字段（`reason`/`location`/`variable`）也过 `sanitize_text`，使其对任意公开构造都有界。原测试确实把 secret 放进 `available` 却没断言它不出现——已补上，并新增覆盖全部 12 个变体、每个字符串字段都埋 secret 的测试 |
| R3-3 | MAJOR | 已修：`Overrides`/`EnvLayer`/`ConfigSource`/`LoadedConfig` 的 Debug 不再输出配置路径（只显示 set/unset），与 profile 名同一规则 |
| R3-4 | MAJOR | 已修：绑定检查改用规范化 origin 比较（`engine::base_url_origin`），9 组等价/非等价用例 + 一个尾斜杠的 e2e |
| R3-5 | MAJOR | 已修：`sanitize_*` 不再只看 `char::is_control()`，新增 bidi（U+202A–202E、U+2066–2069、U+200E/F、U+061C）与零宽/不可见（U+200B–200D、U+2060–2064、U+00AD、U+180E、U+FEFF）。注意 profile 名此前之所以安全是因为走 `{:?}` 的 Rust Debug 转义（Cf 类被转义），而路径走 Display，两条路径不同 |
| R3-8 | MAJOR | 已修：`ConflictingUrl` 与 `UnboundProfileCredential` 登记进 `docs/exit-codes.md`；并新增 `every_config_error_is_registered_in_the_exit_code_document`——**穷尽 match** 把每个变体映射到文档关键词，新增变体不改文档就编译不过 |
| R3-9 | MINOR | 已修：profile 无 URL 的错误不再建议 `set OUTLINE_URL`（那会撞上绑定检查），改为建议加 `url =` 或 `--url` |

R3 已 VERIFIED：R2-6（Windows/POSIX 双规则参数化执行）、R2-7（64 字符上限在派生处生效）。

### R4 复核处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| 裁决落地 | PARTIAL | 规范选择被确认正确；「结构性不可绕过」不成立的部分已补完（见上：Settings / EnvLayer / BindingChecked 三处同时私有化，编译失败测试 + 变异验证） |
| R4-1 | BLOCKER | 已修，如上 |
| R4-3 | MINOR | 已修：非法 URL 不再误报「跨实例冲突」。分三种情况——**解析结果**无法确定 origin（发不出去，交给请求通道给出精确的 invalid base URL）、**profile 声明的 url** 无法解析（绑定根本建立不了，新增 `InvalidProfileUrl` 指向 profile 自己的配置）、两者都能解析则比规范化 origin。原注释与实际控制流相反，已一并改正 |

R4 已 VERIFIED（不再改动）：AC2 同项三层优先级、跨 origin 零请求、R3-2/3/4/5/8/9。

### R5 复核处置（2026-08-26）：闸门强度取决于**模块布局**，不是 `pub` 关键字

| # | 级别 | 处置 |
|---|------|------|
| 合并前专项 | NOT ESTABLISHED → 已修 | 见下 |
| R5-1 | BLOCKER | 已修：安全状态与秘密容器各自迁入**私有叶子模块** |
| R5-2 | MINOR | 已修（见 4-4）：文档覆盖度检查改为**子句级**，可检出混合句 |

**问题**：R4 我把 `Settings` / `EnvLayer` 的字段写成私有就宣称「结构性不可绕过」。审查者指出这在 Rust 里
不成立——**私有字段对定义模块及其所有后代可见**。`config` 里声明的私有字段，`config` 自己以及将来任何
`config::credentials`（正是 Epic 2 凭证源最可能落的位置）都能直接读写。审查者还给了现成反证：
`config/file.rs` **今天就在**读父模块 `Profile` 的私有字段。我的编译失败探针是以**外部 crate** 身份编译的，
只覆盖公共 API 边界，模拟不了 crate 内子模块的隐私规则——这是我验证方法的盲区，不只是实现的。

**新模块布局**（每个都是**叶子**，互不为祖先）：

| 模块 | 独占能力 | 谁都不能 |
|------|---------|---------|
| `config::resolved` | 声明 `Settings`/`UrlSource`，`resolve_settings` 是唯一构造者 | 其他任何模块伪造 `url_source: Flag` |
| `config::secret` | 声明 `EnvKeys`（密钥存储），`EnvApiKey::fetch` 是唯一读出路径 | 其他任何模块读到密钥；`config` 只能构造与数条数 |
| `config::release` | 声明 `BindingChecked`，`release_token` 是唯一签发者 | 其他任何模块跳过绑定检查调 `fetch` |

`config/mod.rs` 现在**也**读不到这些——重构过程中编译器立刻报了
`field base_url of struct Settings is private`，这就是边界生效的第一手证据。

**验证方法补齐了 crate 内视角**（`tests/config_isolation.rs`）：
- `a_module_added_inside_config_cannot_forge_the_gates_state`：把**真实的** config 源码整棵复制到临时目录
  编译成独立 crate，先编译未改动版本（许可探针，防止「rustc 全挂」空过），再分别注入 4 个 attacker 兄弟
  模块（伪造 Settings / 读全局密钥 / 读 per-profile 密钥 / 伪造 BindingChecked），要求全部编译失败。
- `the_security_state_lives_in_leaf_modules`：断言三个叶子文件不声明任何子模块，且四个安全类型都不在
  `mod.rs` 里声明——整套论证依赖「无后代」，这条属性必须挡住静默回归。
- 外部 crate 探针保留并扩充到 6 个。
- 已做**双向变异验证**：把 `EnvKeys::global` 改成 `pub(super)` → `A MODULE INSIDE config CAN STILL read
  the global API key out of the layer`；给 `resolved.rs` 加一个真实子模块 → `resolved.rs:154 declares a
  submodule`。

### R6 复核处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| R6-2 | **MAJOR** | 已修：`BindingChecked` 只证明「检查跑过了」，没证明「跑在哪个 Settings 上」。审查者用外部 crate 复现了洗白路径：拿一个合法批准的 token，配上闸门刚拒绝的另一组 settings 调 `fetch`，密钥照样出来。现在 `BindingChecked<'a>(&'a Settings)` 携带它批准的对象，且 **`fetch` 不再接受 settings 参数**——source 只能从 token 里读，洗白在类型上不可表达。新增运行时测试（委托型 source 只能拿到 benign 的全局密钥，拿不到被拒 profile 的密钥）＋外部编译失败探针（旧的洗白写法根本编译不过，报 E0061） |
| R6-4 | MINOR | 已修：叶子守卫原本是**行前缀文本匹配**，`#[path = "x.rs"] mod x;`、`pub(in crate::config) mod`、`pub(crate)mod`（无空格）、跨行 `mod`、`include!()` 全部绕过（审查者实测三种都能编译并伪造状态）。现在先剥注释与字符串再做**词法扫描**，识别 `mod` / `#[path` / `include!`；新增 guard-the-guard：11 种绕过写法必须全部被拦，5 种正常散文（注释里的 `mod.rs`、`modify` 标识符、字符串字面量）必须不误报 |
| R6-5 | MINOR | 已修：`is_privacy_rejection` 接受 `E0609`/`no field`，导致**字段被改名**时探针失败的原因与「私有」无法区分，攻击用例静默变成空转仍报绿。现在每个内部攻击都配一个**正向对照**：把目标字段机械放宽为 `pub(super)` 后必须编译成功；改名会同时打断两半并立刻报「positive control is stale」。已变异验证（把 `global` 改名 → 立即失败） |
| R6-6 | MINOR | 已修：profile 可由 flag / `OUTLINE_PROFILE` / `default_profile` 三种方式选中，而两条诊断一律建议「drop --profile」——对后两种来说是**用户根本无法执行的动作**，而且这恰是「已有 Epic-1 配置在出现 config file 后失效」的那条路径。新增 `ProfileSource` 记录来源，诊断按来源给出 `drop --profile` / `unset OUTLINE_PROFILE` / `remove default_profile` |
| R6-7 | MINOR | 已修：`sanitize_name` 的文档注释是个悬空片段（首句丢失），而它是 `pub` 项，会原样进 `cargo doc` |
| R6-8 | MINOR | 已修：`OUTLINE_PROFILE` 走 `non_blank` 而 `--profile` 不走，导致 `--profile "  work  "` 报未知 profile 而环境变量同值可用。现在两层同规则（`overrides.url` 本来就已经如此，属于 `Overrides` 内部的不一致） |
| 合并前专项 (a) | 确认 | 审查者独立验证：兄弟模块安全、叶子的**后代**确实能伪造——即「叶子性」是真实承重的性质，不是装饰 |
| 合并前专项 (b) | 已修 | 探针 harness 现在**递归复制**目录模块，并自动带上 config 经 `crate::` 引用的兄弟模块（今天是 `text`）。否则 Epic 2 若用 `config/credentials/mod.rs`，harness 会以「the harness is broken」panic，而最省事的「修法」是放宽断言——那等于静默关掉这套安全测试 |

### R7 复核处置（2026-08-26）

R7 判定：R6 的 1 BLOCKER + 2 MAJOR + 6 MINOR **全部 VERIFIED**，无 NOT FIXED、无 REGRESSION，
**本 track 可以合并**。审查者做了实机验证：真实 zsh 5.9 装进临时 `$fpath` 后 `_comps[otl]=_otl`
（已注册）、bash 3.2 source 后 `_otl` 存在、fish 3.6 里 `complete -C "otl api documents."` 返回
123 条候选、洗白调用实机编译失败 `E0061`。

| # | 级别 | 处置 |
|---|------|------|
| R7-(d) | 注释（非缺陷） | 已写入 `release.rs` 模块文档：闸门的保证是「**本次调用传入的** settings」。一个**蓄意**的 source 可以自己经公开的 `resolve_settings` 解出另一份能过闸的 `Settings B`（如带 `--url` 的重定向），对 B 再入 `release_token`，把 B 的凭据交给以为在问 A 的调用者。这等价于 source 直接硬编码凭据——闸门防的是**意外**（链式 fallback 误用、新 source 不知道有检查），那类失败现在不可表示；已在二进制内的代码要故意作恶有更简单的路子。把边界写清楚比暗示更强的保证有用 |
| R7-1 | MINOR | 已修：加入第四类 `Joiner` 后有四处旧表述没跟上（`render.rs` 两处、`config/error.rs` 一处、README 一处）。修法不只是改数字——**prose 里不再写类别数量**：`clean_char` 的文档改为「每一类各有答案，所以下面是穷尽 match」，诊断侧改为「问的是是否被分类，新增类别自动覆盖」。这样加第五类时散文不会失效，代码侧则由穷尽 match 强制 |
| R7-2 | MINOR | 已修：`text.rs` 首句「every surface」过宽——JSON 路径确实原样发射 bidi。收敛为「every surface that RENDERS text for a human」，并新增一节明确 `--json` 是**有意豁免**（它是 payload，契约是 jq 可消费且能原样往返；清洗会为保护非目标消费者而破坏数据），连同代价一并写明（`--json | cat` 到终端仍可能被重排；受保护的形态是表格，也就是 TTY 的默认）。新增 `json_mode_is_exempt_from_hazard_scrubbing` 把这条豁免钉成**被检查的决定**而非疏漏——已变异验证：让 JSON 走清洗会立刻红 |

**给 Epic 2 的接口说明**：凭证文件源实现 `TokenSource` 即可，无需知道闸门存在。三条约束（R6 复核后收紧）：

1. **不要**放成 `config::resolved` / `config::secret` / `config::release` 的**子模块**；扁平兄弟文件
   `config/credentials.rs` 是推荐位置，config 之外也安全。
2. 优先用**扁平文件**而非目录模块。harness 现在支持目录模块，但扁平形式更简单，也是审查者的建议。
3. `fetch` 现在**没有** settings 参数——想服务的 settings 只能从 `checked.settings()` 取。这不是纪律
   要求，是签名层面的：R6 之前那条「拿批准过的 token 去配另一组 settings」的洗白路径现在写都写不出来。
   保证的边界见 `release.rs` 模块文档的「What the gate does and does not guarantee」。
4. 叶守卫只扫 `resolved.rs` / `secret.rs` / `release.rs` 三个文件。`config/credentials.rs` **内部**再加
   子模块不在守卫范围内，但它不持有任何闸门状态，无害；真正要守住的是「不在那三个叶里加任何子模块」。

R2 已 VERIFIED：R1-3（TOML 文本只用于分类）、R1-4（两层白名单）、R1-7（控制字符清理，并额外确认 bidi
经 Rust Debug 转义后无法改变终端方向状态）。

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
