# Story 2.1: OAuth 浏览器登录（预注册路径）

Status: review (R1 fixes applied)

## Story

As a 不想手工管理 API key 的用户,
I want `otl auth login` 拉起浏览器完成授权,
so that 用工作区身份安全登录。

## Acceptance Criteria

1. **Given** 配置了 client_id（管理员预注册）
   **When** 执行 `otl auth login`
   **Then** 自 `/.well-known/oauth-authorization-server` 发现端点，从固定端口清单依次绑定回环端口，浏览器打开授权页（PKCE S256，scope `read write`，随机 state）
2. **Given** 用户在浏览器完成授权
   **When** 回调命中本地服务器
   **Then** state 严格校验，授权码换取 tokens，access/refresh token 原子写入凭证文件（创建即 0600），终端提示登录成功身份
3. **Given** 已登录
   **When** 执行 `otl auth info`
   **Then** 显示当前用户、workspace、认证方式与 scope

## Tasks / Subtasks

- [x] Task 1: 元数据发现 (AC: 1)
  - [x] `auth/metadata.rs`：GET `{base}/.well-known/oauth-authorization-server`
  - [x] 必填 `authorization_endpoint` / `token_endpoint`，可选 registration/revocation
  - [x] **同源校验**：所有广播端点必须与实例 origin 相同，否则拒绝（见 Dev Notes 安全决策 1）
  - [x] `code_challenge_methods_supported` 不含 S256 时拒绝登录
- [x] Task 2: 回环回调服务器 (AC: 1, 2)
  - [x] `auth/loopback.rs`：常量 `CALLBACK_PORTS = [8586, 18586, 28586, 38586]`、`CALLBACK_PATH = "/callback"`
  - [x] 字面量绑定 `127.0.0.1`（不用 `localhost`，避免解析到 `::1`；不用 `0.0.0.0`）
  - [x] 非阻塞 accept + 轮询，整体 deadline（`AUTH_TIMEOUT = 240s`），超时给可读错误
  - [x] 只接受 GET 且路径为 `/callback`；其他请求答 404 后继续等（浏览器会发 favicon 等）
  - [x] 请求行读取上限 `MAX_REQUEST_LINE_BYTES`，连接数上限 `MAX_CONNECTIONS`
- [x] Task 3: PKCE 与 state (AC: 1, 2)
  - [x] `auth/pkce.rs`：32 字节 verifier（base64url 43 字符，符合 RFC 7636 长度与字符集）、S256 challenge
  - [x] 16 字节 state（22 字符）；`getrandom` 直取 CSPRNG
  - [x] state 校验在读取 `code` 之前，不匹配即中止且不换取 token
- [x] Task 4: 授权 URL 与 token 交换 (AC: 1, 2)
  - [x] `auth/oauth.rs`：授权 URL 经 `Url::query_pairs_mut` 组装（参数含 `&` 也无法注入额外 query）
  - [x] token 端点 `application/x-www-form-urlencoded`（RFC 6749）
  - [x] 每次调用声明所携带的 secret 列表，供错误文本脱敏
- [x] Task 5: 凭证落盘与身份提示 (AC: 2, 3)
  - [x] `OAuthSession` 记录 access/refresh/绝对过期时间/scope/client_id/token 与 revocation 端点
  - [x] 原子写入（Story 2.6 的 `secret_file`），创建即 0600
  - [x] 登录成功后经**引擎唯一请求通道**调 `auth.info` 取身份，缓存 account/workspace
  - [x] `auth info` 展示 profile/instance/method/account/workspace/scope/过期时间/凭证文件路径
- [x] Task 6: 测试 (AC: 1-3)
  - [x] `tests/auth_oauth_e2e.rs`：测试扮演浏览器（读 stderr 里的授权 URL，自行发起回调），端到端跑通 login
  - [x] 断言 PKCE S256 / scope / 随机 state / loopback redirect_uri / 固定端口清单
  - [x] state 不匹配 → 退出码 4 且**未发生** token 交换（断言 wiremock 收到 0 次 `/oauth/token`）
  - [x] 授权被拒 → 退出码 4，错误码与描述可读，不落盘
  - [x] 单测覆盖 metadata 校验、PKCE 向量（RFC 7636 附录 B）、回调 query 解析、超时

## Dev Notes

- **先读 `project-context.md`**。本 story 最相关红线：engine 禁止 OAuth 内容（全部落 otl）、除凭证文件外任何位置不得出现凭证、硬编码值提常量。
- **R3 [29] 修正：issuer 比较两侧都过同一个 URL parser**。R2 用**用户输入的 `OUTLINE_URL` 原文**
  当预期 issuer，而 endpoint 同源校验用的是 parser 规范化后的 origin——两套基准。
  于是任何等价但非字节相同的写法（大小写主机名、显式 `:443`、`0177.0.0.1` 这类合法数字形式）
  在 `otl api` / `auth info` / `set-key` 下都正常，唯独 `auth login` 报「服务器身份不对」——
  把用户的输入格式问题指控成元数据攻击。现在 `canonical_issuer()` 两侧都跑 `Url::parse`
  再 `strip_one_slash`。规范化没有变松：不同租户路径、重复斜杠仍然拒绝，有测试钉住。
- **安全决策 0-fix（R2：TLS 强制覆盖所有命令，不只 login）**：R1 只在 `metadata::discover` 里做检查，
  于是 `otl api` / `auth info` / env key / 已存 session 走 `http://remote-host` 时 bearer 仍然明文上网。
  现在 `auth::instance_origin()`（`open_session` 与 `open_store` 的共同入口）调用 `require_secure`，
  所以**每条**需要凭证的命令都过这条规则。engine 保持通用、仍然接受 http——「凭证不得明文传输」是
  产品策略，属于 otl，不属于通用 RPC 引擎。
  另外**凭证文件里的 endpoint 在使用时重新校验**（refresh 的 token endpoint、logout 的 revocation
  endpoint、purge 的 registration management URI）：那些值来自磁盘，文件可被手工编辑、可能早于本规则、
  也可能是从别的机器拷来的，而它们马上就要接收 refresh token。
- **安全决策 0（R1 后新增：TLS 强制）**：`auth/transport.rs` 要求实例 URL 与所有广播端点为 `https://`，
  唯一例外是**回环 IP 字面量**（`127.0.0.0/8`、`[::1]`）。理由：授权码、PKCE verifier、refresh token、
  client secret、撤销令牌全部在请求体里；明文 HTTP 下这些对路径上任何人可读，而 refresh token 是长期凭证。
  例外只给字面量、**不给 `localhost` 这个名字**——名字要过解析器，hosts 文件或 DNS 应答就能把它指到别处。
  这也是为什么 e2e 测试用 `http://127.0.0.1:PORT` 的 wiremock 是合法路径而非掩盖问题：它走的正是这条
  文档化的例外；另有 `a_plaintext_remote_instance_is_refused_before_any_request` /
  `a_plaintext_localhost_by_name_is_refused_too` 两条反向测试钉住远程明文与按名回环都被拒。
- **安全决策 0b（R1 后新增：禁用重定向）**：OAuth 端点客户端与 engine 请求通道都设
  `redirect::Policy::none()`。307/308 会保留方法并**重放请求体**，而 reqwest 的跨源敏感头剥离只处理
  header、不碰 body，且只在**跨 host** 时触发——同 host 的 `https:`→`http:` 降级会保留 Authorization。
  同源校验校验的是「我们选的 URL」，管不到服务器在请求执行期间把我们送去哪。发现请求同样禁用，
  否则是一个可达内网的 SSRF 原语。RPC 风格 API 的 POST 没有任何正当重定向理由。
- **安全决策 0c（R1 后新增：RFC 8414 issuer 必须存在且精确匹配）**：比较的是**完整标识符**而非 origin——
  同一 host 上的两个租户只差路径，而这正是 origin 比较会放过的情形。issuer 是服务器控制的文本，
  报错时只说「期望值是什么」，不回显它。
- **安全决策 1（同源端点，故意收紧）**：`metadata.rs` 要求 `authorization_endpoint` / `token_endpoint` /
  `registration_endpoint` / `revocation_endpoint` 与实例 origin 完全一致（scheme+host+port）。
  理由：授权码、PKCE verifier、refresh token 都会 POST 到 token 端点，被篡改的元数据文档
  只要改一个字段就能把凭证送到别处。Outline 自托管自己提供这些端点，所以代价为零。
  副作用：若将来遇到「Outline 前面挂独立授权服务器」的部署，会得到明确报错而非静默泄漏——
  这是有意的取舍，需要时再加显式 opt-in。
- **安全决策 2（state 先于 code）**：`loopback::CallbackServer::finish` 先比对 state，再看 query
  里其他任何字段。顺序是安全属性：一个不属于本次登录的重定向，其 `code` 不应被交换。
- **固定端口清单是公共契约**：管理员预注册应用时必须允许这四个 redirect URI。可以追加，
  **不可重排/改号**，否则已注册的应用会静默失效。测试 `the_fixed_port_list_is_the_documented_contract` 钉住它。
- **授权 URL 打到 stderr**：双态输出契约里 stdout 只放数据。登录进度与「浏览器没打开就访问这个 URL」
  属于诊断/提示，走 stderr；`auth login` 的结果（身份、凭证文件路径）走 stdout。
- **浏览器打开是 best-effort**：走 `crate::browser`（develop 的 Epic 3 模块，见下方集成记录），
  平台命令 `open` / `cmd /C start ""` / `xdg-open`，并尊重 `$BROWSER`。
  URL 作为 argv 传递而非拼进 shell 字符串。失败只降级为「手动访问这个 URL」，不算登录失败。
- **身份查询走唯一通道**：`auth.info` 用 `engine::Client::execute` + IR 里的算子，不自己发 HTTP。
  单测 `the_identity_operation_exists_in_the_compiled_spec` 防止 IR 变更把它弄丢。
- **超时选 240s**：与 `scripts/test_oauth.py` 实测脚本一致，够完成一次含 SSO 的同意流程。

### References

- [Source: planning/epics.md#Story 2.1]
- [Source: specs/spec-outline-cli/stack.md#认证实现]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-1]
- [Source: scripts/test_oauth.py]（实测：PKCE S256 可用、公共客户端可用、access token 3600s）
- RFC 6749（授权码）、RFC 7636（PKCE）、RFC 8414（元数据发现）

## develop 集成记录（Phase 6）

合并 develop 时，Epic 2 与已落地的三条 track（4a 配置/Profile、发布流水线、Epic 3 精选命令）
在五个接缝上对齐。每一条都记在这里，因为它们都改变了 Epic 2 已经通过审查的行为。

### 1. 实例解析改走配置层

`auth::base_url()`（直读 `OUTLINE_URL`）与 `auth::paths::active_profile()`（直读
`OUTLINE_PROFILE`）删除，换成 `auth::resolve_instance(overrides) -> Instance`，内部是
`EnvLayer::from_process()` → `config::load_file` → `config::resolve_settings` →
`transport::require_secure` → `engine::base_url_origin`。

后果是 `otl auth login` / `set-key` / `info` 现在和 `otl api` 一样尊重 `--profile`、`--url`、
`--config` 和配置文件里的 `default_profile`。之前它们只看环境变量，也就是说
`otl --profile work auth login` 会把凭证写到 `default` 名下——两层各自解析同一件事，
迟早会不一致，这次把「哪个实例、哪个 profile」收敛到配置层唯一一处。

传输规则（HTTPS，回环 IP 字面量例外）留在 `resolve_instance` 里，而不是下沉到 engine：
engine 是通用的、按设计接受 `http`，「凭证不得明文上路」是这一层的策略。

`otl auth logout` 例外，仍然不解析实例：它只需要 profile 名字，用
`config::resolve_profile_name`（本次从 `resolve_settings` 里抽出的函数，因为
`resolve_settings` 在没有 URL 时会报 `MissingUrl`）。理由见 2-4。

### 2. 凭证释放闸门：`config/credentials.rs`

Epic 4a 引入了释放闸门 `release_token(source, settings)`：`TokenSource::fetch` 不接收
settings，只接收 `BindingChecked`，而后者的字段私有且只有 `config/release.rs` 能构造。
凭证文件必须走这个闸门，否则「profile 绑定」只覆盖环境变量。

做法是 `config/credentials.rs`——**扁平兄弟文件**，不是 `resolved`/`secret`/`release` 的子模块
（子模块能看见父模块的私有项，`config_isolation.rs` 正是为此而设）。里面三样东西：

- `StoredCredential<'a>`：持有借来的 secret 与文件路径，实现 `TokenSource`。
- `select(settings, file_has_credential) -> Source`：只决定「文件还是环境」。
- `Config::release(settings, env, stored)`（在 `config/mod.rs`）：按 `select` 选择，两条分支
  都过 `release_token`。

**文件不在这里读**。读它会让 `config` 依赖 `auth`，层次反了；而且
`config_isolation.rs` 的权限探针把 config 模块树单独编译，`--extern` 表是硬编码的四个 crate，
把 auth 拖进去就得把 auth 的每个依赖都列上。secret 由调用方递进来——`EnvApiKey` 同样在
`fetch` 之前就持有值，闸门从来管的是 RELEASE 而不是 READ。

同一个原因，本模块里 `auth` 只以散文提及，不写 `[`crate::auth`]` 内链：探针扫的是源文件里
任何位置的 `crate::`，包括文档注释，内链虽然不是编译期依赖，但在探针眼里是。

`select` 与凭证文件自身的优先级不冲突，因为分工不同：`select` 决定文件 vs 环境，
文件内部「可续期 session 还是固定 key」由 `auth` 决定——`Config` 只装一个固定 key，
每次刷新都会轮换的 session 不是它，所以 `auth` 在开口要 `Config` 之前就把 session 那条路走完了。

`auth = "oauth"` 只能取自凭证文件，永不回落到环境变量：环境变量装不了可续期 session，
配置了浏览器登录却因为残留的 `OUTLINE_API_KEY` 而以别的身份发请求，是不能接受的。

反过来，`otl auth login` 存下的 session 在 `auth` 保持默认（`api_key`）时也会被使用：
`auth` 命名的是登录方式，不是对已存凭证的过滤器，`otl auth info` 会报告它遮蔽了什么。

### 3. 精选命令共用同一个客户端

`session.rs` 的 `Session::open` 原本走 `Config::load` + `Client::new`，即只支持 API key。
改成走 `auth::open_client(overrides)`，于是 `otl docs ...` / `otl collections ...` 在 OAuth
下也能用，而且传输规则与实例绑定检查无法被「从这里进来」绕过。

`auth::open_client` 返回 `(Client, origin)`，而不是让调用方自己再算一次 origin：
返回的正是凭证被校验时用的那个 origin。

### 4. 「没有凭证」的诊断改说三条路

`ConfigError::MissingApiKey` / `MissingProfileApiKey` 原本只提 `OUTLINE_API_KEY`——Epic 2
之前确实只有这一条路。现在两条消息都列出 `otl auth login`、`otl auth set-key` 和环境变量，
把凭证放进 0600 的凭证文件是更好的选择，只被告知环境变量的用户会因为不知道替代方案
而把长期 key 导出到 shell 环境里。`config_diagnostics.rs` 的长度上界（600 字符）仍然满足。

### 5. 字符危险表下沉到 engine

Epic 3 把 `otl::text` 的 `Hazard`/`hazard()` 扩到完整 `Cf` 类别（比 develop 原版多 147 个码点）。
但 engine 的 `sanitize::normalize`——服务器文本进 stderr 前的清洗——是按**渲染宽度**分类的，
`unicode_width` 给 27 个 `Cf` 码点一个列宽，于是它们穿过 engine 的清洗，
而 CLI 侧的同名清洗会丢掉它们。同一个 crate 里两张表，其中一张更弱。

`Hazard`/`hazard()`/`has_hazard()` 连同逐码点枚举测试移到 `engine::text`（纯 `char` 逻辑，
无任何协议内容），`otl::text` 改为 re-export，调用方写法不变。`normalize` 现在两条规则并用：

- `hazard(c)` 命中 → 丢弃（补上 width 放过的 27 个 `Cf` 码点）；
- 宽度 0 或 None → 丢弃（补上 `hazard` 不管的组合附加符号，Zalgo 那一类）。

两条规则互不包含，所以是并用而不是替换。反向验证：把 `hazard` 一侧短路成 `None`，
`normalize_drops_every_format_character_not_just_the_zero_width_ones` 变红。

### 6. 凭证文件解析错误：与 `classify_parse_error` 无关的剥值规则

`config/file.rs` 的 `classify_parse_error` 会把解析器措辞归类成「未知键」「类型不对」等描述，
在配置文件那边是安全的（那个文件本来就拒收凭证）。凭证文件通篇都是凭证，而其中几种措辞
会引用出错的值（`unknown variant \`<token>\``、`invalid type: string "<token>"`），
所以这一侧只保留行列位置，**完全不查阅分类器**。

这条规则一直是这么实现的，本次补的是两个断言：`a_malformed_credential_file_is_reported_
without_any_of_its_content`（逐种措辞验证连 4 字符片段都不泄漏）和
`the_credential_files_parse_rule_does_not_borrow_the_config_files_wording`
（分类器的每条描述都不得出现）。两个都做了反向验证，各自会被不同的回退弄红。

### 7. 集成暴露出的三件既有问题（一并修掉）

这三件不是 Epic 2 引入的，但都是「精选命令改走 `auth::open_client`」之后才会咬人的，
所以在这一次修：

1. **测试没有隔离真实凭证文件**。`tests/common/mod.rs`（Epic 3）、`tests/profile_e2e.rs`（4a）
   和 `tests/contract_smoke.rs` 都只清了 `OUTLINE_*` 里的几个变量，没有设
   `OUTLINE_CONFIG_DIR`；`profile_e2e` 甚至把所有 `OUTLINE_*` 清空，于是回落到用户真实的
   配置目录。之前无害（那条路只读 `OUTLINE_API_KEY`），现在每个命令都会先找凭证文件——
   开发者机器上存了 session 就会得到与 CI 不同的结果。三处都改成指向进程内共享的空临时目录。
   `profile_e2e.rs:an_oauth_profile_...` 的失败信息里直接打出了
   `~/Library/Application Support/outline-cli/credentials.toml`，就是这个问题的实证。
2. **`auth = "oauth"` 的 e2e 断言过期了**。`profile_e2e.rs` 原本断言「只有 api-key 被接线」，
   现在 oauth 接线了。改成断言真正该成立的性质：`auth = "oauth"` 且凭证文件为空时，
   **即使 `OUTLINE_API_KEY_WORK` 已导出也不回落**，并且提示的是 `otl auth login`。
   （`config/secret.rs` 里 `EnvApiKey` 对 oauth 的 `UnsupportedAuthMethod` 拒绝保留：
   `release_token` 是公开的，那条防线仍然可达，且是纵深防御。）
3. **同一条规则有两个门禁**。`limits.rs`（4a/发布 track，带一份「只会变短」的豁免清单）和
   `source_hygiene.rs`（本 track，无豁免）都在查 800 行文件限制，于是
   `crates/engine/tests/validation.rs` 同时被豁免又被禁止。`source_hygiene.rs` 交出文件长度、
   只保留函数长度与嵌套；「接近上限」的提示搬进 `limits.rs`。顺带
   `crates/engine/tests/pagination.rs` 已被 develop 拆到 504 行，按清单自己的规则移出豁免。
   `crates/otl/src/commands/collections.rs::document_counts` 嵌套 5 层（我的门禁比 develop 的严），
   用 `Option`/`Result` 折叠两种失败 + 抽出 `Counts::record` 压到 3 层，行为不变。

### 8. 门禁数字

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo test --workspace` 全过。
- `scripts/check-binary-size.sh`：develop（含 spec-sync）3_215_728 B（76%），
  合并后 3_431_584 B（81%），低于 85% 告警带。分支单独测出的 +317_360 B 落到合并后
  只剩 +215_856 B，因为 reqwest/base64/sha2 已被先落地的 track 链接进去，本次又删掉了
  重复的 browser 模块、把危险字符表折进 engine、并把 sha2 从两个大版本收敛到一个
  （spec 缓存要 0.10、PKCE 要 0.11，同一个哈希库编译两遍毫无收益）。
  **常量未动**；脚本里的实测数字按它自己的要求更新了，musl 那一行明确标注为
  「按比例推算（+9%）→ ~3.57 MiB / 89%，非实测」。
- `config_isolation.rs` 的 `a_module_added_inside_config_cannot_forge_the_gates_state`
  在**干净的 target 目录**下通过（`config/credentials.rs` 不需要新增 `--extern`）。
  在本 worktree 累积了多轮不同 feature 组合的 target 目录里会失败，原因是探针用 mtime
  挑 `libserde-*.rlib` / `libtoml-*.rlib`，可能选到编译自不同 `serde_core` 的两个——
  即 specsync 报告的那个「mtime 抽奖」。已在 develop 与本分支的干净副本上分别验证：
  两边都通过，所以这不是本次集成引入的。探针本身未改动，留给 specsync 的修法。

### 9. 回退验证

沿用前几轮的做法，本轮每一条新断言都验证过「把修改回退后它会红」：

| 回退 | 变红的测试 |
| --- | --- |
| 凭证文件解析错误改用 `toml` 的 `Display` | `a_malformed_credential_file_is_reported_without_any_of_its_content` |
| 凭证文件解析错误改查阅 `classify_parse_error` | `the_credential_files_parse_rule_does_not_borrow_the_config_files_wording` |
| `normalize` 里 `hazard` 一侧短路成 `None` | `normalize_drops_every_format_character_not_just_the_zero_width_ones` |
| `StoredCredential` 加一个绕过闸门的取值方法 | `a_stored_credential_is_not_released_to_another_profiles_instance`、`..._when_the_profile_named_no_instance` |
| `select` 反转成永远返回 `Environment` | 6 个 `config_credentials.rs` 测试 |
| `session.rs` 回到 `Config::load` + `Client::new` | 3 个 `auth_curated_path.rs` 测试 |
| `check_binding` 从 `open_client` 与 `CredentialProvider::resolve` 一并删除 | `a_curated_command_refuses_a_credential_from_another_instance` |

最后一条特别说明：它第一版写成「指向一个不存在的主机」，回退后仍然是绿的——命令两种情况
都失败，只是失败原因从「拒绝」变成「DNS 错误」。这正是前几轮被点名三次的「断言对被测行为
不敏感」。改成**两个都活着的 mock server**（凭证由 A 签发、命令指向 B），断言 B 一个请求都
没收到，回退后立刻变红。

### 10. 第二次 develop 合并（spec-sync track）

第一次合并时 `MERGE_HEAD` 指向 Epic 3 的提交；合并期间 develop 又前进了 30 个提交
（epic4-specsync：`spec-compile` crate、`otl spec sync/reset`、磁盘 IR 缓存、
plain-document fetch 通道）。所以本 track 做了第二次合并，接缝如下：

- **sha2 版本冲突**：spec 缓存声明 0.10，Epic 2 的 PKCE 声明 0.11。统一到 0.10
  （已在 lockfile 里），`crates/otl/Cargo.toml` 里 `sha2` 与 `thiserror` 各自的重复条目
  合并成一条并说明两个调用方。
- **`no_phone_home.rs`（specsync 新增的门禁）把 OAuth 通道判为违规**。这条门禁把 HTTP
  限制在 engine 的两条通道内，而 OAuth 的 token/注册/撤销端点是**第三条**、也是文档化的
  例外（develop 自己有一个提交叫「register the third HTTP channel exception」，但注册的是
  fetch 通道）。改法不是加宽豁免了事：
  - 模块文档新增「The third channel」一节，写清为什么这三类请求不能走 engine 的认证通道
    （不带 bearer、form-encoded、不在 OpenAPI 里、其中一类的目的就是去取那个凭证）；
  - `.send()` 白名单只加 `auth/endpoint.rs` 一个文件，并把它计入
    `each_channel_has_exactly_one_send`（实测整个 otl crate 只有 1 处 `.send()`）——
    这条才是真正的约束，`reqwest` 白名单能放宽到 11 个 auth 模块正是因为它们只能「提到」
    而不能「发送」；
  - 顺手补上门禁漏掉的 `TcpListener` 规则（原表只有 `std::net`/`TcpStream`/`UdpSocket`）。
    回环回调监听器是**入站**的，不产生请求也不触达远端，所以它不是这条规则要拦的东西，
    但仍然只允许出现在实现那一条流程的两个模块里。
- **`startup_guard.rs`（specsync 新增）要求逐调用点登记文件打开**。凭证文件的 11 个打开点
  全部登记，每条都写明为什么不能用普通方式打开（`O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC` +
  对拿到的 fd 做 fstat、`create_new` + 0600 交给 `open(2)` 本身、只为 fsync 而打开目录）。
  登记后该文件涨到 830 行超限，按责任拆成 `tests/guard_registry/mod.rs`（登记数据）与
  `startup_guard.rs`（检查本身）。
  另外 `portability.rs` 会把数据里出现的 `std::os::unix::...` 字符串当成未加 `cfg` 的
  平台代码，所以那条登记改用 `OpenOptionsExt` 作为 context——它本来就该是「区分调用点的
  片段」而不是整行。
- **exit-code 表求并集**：spec-sync 给 1/2/5/6/7 增加了原因，Epic 2 给 2/3/4 增加了原因，
  逐行合并后用 `UPDATE_README_EXIT_CODES=1` 重新生成 README 的派生块。
- **api_* 测试的隔离助手合并**：develop 加了 `CACHE_DIR_ENV`/`no_cache_dir()`（防止本机
  跑过 `otl spec sync` 影响断言），本 track 加了 `OUTLINE_CONFIG_DIR`（防止读到真实凭证
  文件）。四个 suite 里各自复制一份的隔离块统一到 `common::isolate`。

## R6 修复记录：[N1] `otl auth info` 绕过 config 闸门

### 缺陷

`run_info` 解析实例走了新的 `auth::resolve_instance`（正确），但**凭证**仍走它自己的一条路：
`resolve_for_info → CredentialProvider::resolve`，而 `source.rs` 的 `Method::EnvApiKey` 分支
**直读全局 `OUTLINE_API_KEY`**。config 闸门在 profile 生效时只认
`OUTLINE_API_KEY_<PROFILE>`，**拒绝**回落到全局变量——理由正是「falling back would send one
workspace's key to another workspace's server」。

于是同一份配置下：`otl --profile work api ...` 退出 2、零请求；
`otl --profile work auth info` 把 `Bearer <全局 key>` 发给了 work 的实例。

已按审查者的复现写成测试（`auth_curated_path.rs`），断言的是**实例收到了什么**而不是退出码——
一个「拒绝了但还是发了请求」的实现能满足退出码断言。回退验证：把 `auth info` 的自有凭证路径
装回去，测试变红并打印
`auth info sent the global key to the profile's instance: ["Bearer global-key-for-another-instance"]`。

### 修法：按能力拆分，而不是按优先级

没有只在 `run_info` 里补一个判断。`CredentialProvider` 原本按优先级服务三种凭证，
**这个结构本身就是缺陷源**：任何拿到 provider 的调用方都自动获得了那条绕过闸门的环境分支。

现在按**能力**拆：

- `CredentialProvider` **只服务 OAuth session**。构造函数只剩
  `for_session`（无条件跑 `check_binding`），`State::Fixed` 与 `Method::EnvApiKey` 分支删除。
  会续期是它需要存在的唯一理由——固定 key 不需要状态也不需要锁。
- `selection::available()` **只报告凭证文件里有什么**。原来它读 `OUTLINE_API_KEY` 来决定
  「环境里有 key」，正是那个洞；`env_api_key()` 删除。
- 固定 key 只能来自 `config::Config::release`，别处产生不出来。

`auth::resolve_credential(instance, store, file) -> Resolved` 成为**唯一**凭证路径，
`otl api`、精选命令、`otl auth info` 全部经过它。`Resolved` 的 `credential` 字段私有、
不是 `Clone`，唯一出口是消耗式的 `into_client()`——类型上没有任何方法把 secret 取回来，
所以「将来某个命令直接读 key 再送到别处」不可表达。

顺带删掉两条死路径：`auth::open_session` / `auth::Session`（无调用方）与
`paths::active_profile()`。`login.rs::query_identity` 改用 `for_session`：
它要给刚写入的 session 打标签，用一个存储 key 或环境 key 回答会是**另一个身份**。

### 「还有哪条路径能拿到凭证而不经 `Config::release`？」

审查者要求把这个问题答全并写进 story。答案是：**有三类凭证，`Config::release`
结构上只能是其中一类的入口**，另两类各有自己唯一的入口和各自正确的锚点。

| 类别 | 认证到哪里 | 唯一入口 | 锚点 |
| --- | --- | --- | --- |
| 固定 API key | 请求通道的 `Authorization` | `config::Config::release` | 解析出的 profile/URL 绑定 |
| OAuth session | 请求通道的 `Authorization` | `CredentialProvider::for_session` | `check_binding` + session 自己记录的 origin |
| OAuth 端点密钥（refresh token / client secret / registration access token） | token / 注册 / 撤销端点 | `auth::endpoint` 里唯一的 `.send()` | 每个凭证**自己**记录的 origin |

**为什么不能三类共用一个入口**：第三类必须在完全没有配置时可用。
`otl auth logout` 要能在 `OUTLINE_URL` 缺失、错误或已经指向别处的机器上撤销 token、
删除动态注册——那正是用户会去用它的时刻——所以它的锚点是每个凭证自己记录的 origin，
不是解析出的实例。让它走 `Config::release` 等于「为了清理一个不可用的实例，先要求一个可用的实例」。
第二类也不可能是 `Config`：`Config` 装一个固定字符串，而 session 每次刷新都会轮换、还需要锁。

所以不变量不是「一个入口」，而是**「每类一个入口，且没有任何路径跳过它那一类的入口」**。

新增 `crates/otl/tests/credential_paths.rs` 把这件事变成测试而不是保证：

- `Config::release` / `StoredCredential::new` / `select_credential_source` 各只有一个调用点；
- `ENV_API_KEY` 与字面量 `"OUTLINE_API_KEY"` 的出现位置逐个登记——`auth` 下只允许
  `mod.rs`（消息文本）与 `report.rs`（**仅探测存在性**、绝不取值、绝不用于决策，调用点写明）；
- `for_session` 是 provider 的唯一构造函数，调用点只有三处；
- `Client::new(` / `with_credentials(` 的位置逐个登记（谁能造出已认证的通道）；
- `no_module_under_auth_reads_the_process_environment_for_a_credential`：
  `auth` 下每一处 `env::var` 都要连同它读的变量一起登记。这条是最窄也最抗重命名的形式。

自证不空转（每条 needle 必须仍然命中，且至少命中一个白名单文件）、白名单文件必须存在。
回退验证：把 `env_api_key()` 装回 `selection.rs`，
`no_module_under_auth_reads_the_process_environment_for_a_credential` 与
`every_way_to_obtain_a_credential_is_where_it_is_declared_to_be` 同时变红。

### 修 [N1] 时暴露的两处行为变化（都是改好）

1. **明文环境 key 的警告现在也覆盖 profile 作用域变量**。原来它由「全局变量是否被直读」决定，
   于是 `OUTLINE_API_KEY_WORK` 静默——而那正是 CI 最常用的那个变量，暴露面完全相同。
   现在由「闸门是否真的从环境释放了 key」决定，并且**指名实际用到的那个变量**
   （变量名向 config 索取，不自己拼，否则警告可能指向闸门根本不会读的变量）。
   `profile_e2e.rs::a_matching_env_url_is_not_a_conflict` 的断言从「stderr 为空」
   改成「没有冲突诊断 + 通知指名 `OUTLINE_API_KEY_WORK` + 不回显 key」。
2. **`auth info` 的 `available` 只列闸门会释放的凭证**。原来它把全局变量算进去（同一个直读）。
   被遮蔽的明文环境 key 改为独立观测字段 `plaintext_key_in_environment` 上报，
   人类输出里原本就有 `OUTLINE_API_KEY: set (plaintext in the environment)` 那一行。
   `auth_info_names_the_method_in_use_and_what_it_shadows` 两侧都断言，信息没有丢。

另外 `auth info` 现在**说清为什么**：`MissingApiKey`（哪儿都没有）仍报 `method: none`，
其余（profile 自己的变量未设、`auth = "oauth"` 但凭证文件为空、绑定被拒）都把原因和补救
完整打出来——包括闸门自己那句 "would give one workspace's key to another workspace's server"。
静默拒绝会是另一个 bug：用户导出了 key，命令说 "none"，原因不可见。

### [N2] / [N3]

- **[N2]** `stories/2-4` 里 R3/R4 时期的 dev notes 写的是 `exit 3`。已在文件顶部加了醒目的
  退出码更新说明，正文四处改成「非零退出（当时是 3，集成后为 9）」，并指明权威来源是
  `docs/exit-codes.md`。
- **[N3]** `no_phone_home.rs` 的注释说「send-site rule bounds what those streams can do」，
  对 `TcpStream` 不成立——它没有 `.send()`。注释改准，并按审查者的建议补上真正的兜底规则
  `every_outbound_connect_is_to_the_loopback_callback`：每一处 `TcpStream::connect`
  必须指名 `CALLBACK_HOST`，且该常量必须仍是回环 IP 字面量（不是 `localhost` 这个**名字**）。
  回退验证：把一处 connect 改成 `("example.com", port)`，规则变红并打印那一行。

### 结构变化引起的拆分

`commands/auth.rs` 加了 [N1] 的说明后到 814 行，按责任拆成
`commands/auth/mod.rs`（参数、`run_*`、读 key）与 `commands/auth/output.rs`（两种渲染）。
`info_output` 57 行超限，拆出 `info_value`——人类行与 JSON 各自短到藏不住一个字段。
`guard_registry` 里 `commands/auth.rs` 的登记路径同步改成 `commands/auth/mod.rs`。

### 门禁

`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo test --workspace`（1148 个测试）全过；
`scripts/check-binary-size.sh` 3_431_568 B（81%），未进 85% 告警带，常量未动。

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Completion Notes List

- 新增 `--no-browser`（只打印 URL）与 `--timeout <秒>`，前者是端到端测试能扮演浏览器的关键。
- 新增 `--client-id <ID>`：管理员预注册路径。首次使用后会连同 origin 记入凭证文件，
  之后 `otl auth login` 无需重复传参（记为 `dynamic = false`，`--purge` 永不删除它）。
- `auth info` 默认会真的调一次 `auth.info` 验证凭证；`--offline` 只报告本地状态。

### File List

- crates/otl/src/auth/{mod.rs, metadata.rs, pkce.rs, loopback.rs, oauth.rs, login.rs}
- crates/otl/src/browser.rs（集成：新增非阻塞 `spawn`，删除 `auth/browser.rs`）
- crates/otl/src/config/credentials.rs（集成：释放闸门的凭证文件适配器）
- crates/engine/src/text.rs（集成：危险字符分类下沉）
- crates/otl/tests/config_credentials.rs（集成：闸门接缝的行为测试）
- crates/otl/tests/credential_paths.rs（R6：三类凭证各自唯一入口的守卫）
- crates/otl/src/commands/auth/{mod.rs, output.rs}（R6：按责任拆分）
- crates/otl/src/commands/auth.rs
- crates/otl/tests/auth_oauth_e2e.rs
