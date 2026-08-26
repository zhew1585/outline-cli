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
- `scripts/check-binary-size.sh`：develop 3_099_728 B（73%），合并后 3_315_648 B（79%），
  低于 85% 告警带。分支单独测出的 +317_360 B 落到合并后只剩 +215_920 B，因为
  reqwest/base64/sha2 已被先落地的 track 链接进去，且本次删掉了重复的 browser 模块、
  把危险字符表折进 engine。**常量未动**；`scripts/check-binary-size.sh` 里的实测数字按脚本
  自己的要求更新了，musl 那一行明确标注为「按比例推算，非实测」。
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
- crates/otl/src/commands/auth.rs
- crates/otl/tests/auth_oauth_e2e.rs
