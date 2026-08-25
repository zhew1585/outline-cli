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
- **浏览器打开是 best-effort**：`auth/browser.rs` 用平台命令（`open` / `cmd /C start ""` / `xdg-open`），
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

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Completion Notes List

- 新增 `--no-browser`（只打印 URL）与 `--timeout <秒>`，前者是端到端测试能扮演浏览器的关键。
- 新增 `--client-id <ID>`：管理员预注册路径。首次使用后会连同 origin 记入凭证文件，
  之后 `otl auth login` 无需重复传参（记为 `dynamic = false`，`--purge` 永不删除它）。
- `auth info` 默认会真的调一次 `auth.info` 验证凭证；`--offline` 只报告本地状态。

### File List

- crates/otl/src/auth/{mod.rs, metadata.rs, pkce.rs, loopback.rs, oauth.rs, browser.rs, login.rs}
- crates/otl/src/commands/auth.rs
- crates/otl/tests/auth_oauth_e2e.rs
