# Story 2.5: API key 管理

Status: review (R1 fixes applied)

## Story

As a 在 CI 与本机混用的用户,
I want API key 也能安全存管,
so that 两种认证方式都有一等体验。

## Acceptance Criteria

1. **Given** 执行 `otl auth set-key`
   **When** 输入 API key
   **Then** 原子写入凭证文件（创建即 0600），`otl auth info` 显示 API key 认证身份与凭证文件路径
2. **Given** 仅设置了 `OUTLINE_API_KEY` env
   **When** 首次使用
   **Then** 正常工作并提示一次：env 明文会经进程环境与 shell 历史泄漏，建议改用 `otl auth set-key` 存入凭证文件
3. **Given** 同时存在 OAuth 登录、凭证文件 API key、env API key
   **When** 发起请求
   **Then** 按 OAuth > 凭证文件 API key > env 优先级选用

## Tasks / Subtasks

- [x] Task 1: `otl auth set-key` (AC: 1)
  - [x] 从 **stdin** 读取，绝不接受 flag 传值（argv 会进 shell 历史与 `ps` 输出）
  - [x] stdin 是 TTY 时提示，并说明「输入会回显」+ 建议 `otl auth set-key < key.txt`
  - [x] 读取上限 `MAX_API_KEY_BYTES = 4096`，防止误 `cat` 大文件
  - [x] 校验：非空；不含空白与控制字符（否则根本无法作为 HTTP 头发送）
  - [x] 校验失败**绝不回显 key 本身**，只说明原因；且不创建凭证文件
  - [x] 原子写、创建即 0600（复用 Story 2.6 的 `secret_file`）
- [x] Task 2: 优先级解析 (AC: 3)
  - [x] `source::available()` 返回该 profile 全部可用凭证，按优先级排序
  - [x] `CredentialProvider::resolve` 取第一个；OAuth 走可续期分支，两种 key 走固定值分支
  - [x] 固定 key 的 `renew()` 返回 `Ok(None)`：401 原样上报，不做无意义重试
- [x] Task 3: env 明文提示 (AC: 2)
  - [x] 在凭证**解析时**提示（每次命令一次），而不是在 `bearer()` 里（分页会调很多次）
  - [x] 只有真正**在用** env key 时才提示；被 OAuth 或文件 key 遮蔽时保持安静
  - [x] 提示内容：风险（进程环境 / shell 历史 / CI 日志 / crash report）+ 补救（`otl auth set-key`）
        + 关闭方式（`OUTLINE_NO_KEY_WARNING=1`）
  - [x] 走 stderr（诊断），永不打印 key 本身
- [x] Task 4: `otl auth info` (AC: 1, 3)
  - [x] profile / instance(origin) / method / also available / scope / access token 剩余时间
  - [x] account / workspace：默认真调一次 `auth.info` 校验，`--offline` 用 login 时缓存的值
  - [x] 凭证文件路径、存在性、权限状态、可用性（来自 `auth::report::credential_health`）
  - [x] 双态输出：TTY 人类可读，非 TTY / `--json` 输出 JSON
- [x] Task 5: 测试 (AC: 1-3)
  - [x] e2e：三者共存 → 断言服务器收到的 bearer 是 OAuth 的那个
  - [x] e2e：文件 key + env key → 断言收到的是文件里的那个
  - [x] e2e：只有 env key → 断言收到 env 的，且提示出现**恰好一次**
  - [x] e2e：被遮蔽时不提示；`OUTLINE_NO_KEY_WARNING=1` 时 stderr 全静默
  - [x] e2e：`set-key` 从 stdin 存入 → 0600 + 路径回显 + key 不回显 + 内容正确
  - [x] e2e：空输入 → 退出码 2 且不创建文件
  - [x] e2e：`auth info` 报告 method 与被遮蔽的两种，instance 只显示 origin
  - [x] 单测：含换行 / 含空格的 key 被拒且不回显

## Dev Notes

- **为什么不做 `--key <value>` flag**：argv 会出现在 shell 历史、`ps aux`、以及很多 CI 的命令回显里。
  唯一入口是 stdin，脚本用 `otl auth set-key < key.txt` 或管道即可，交互用户看到提示后粘贴。
- **为什么先校验 header 合法性**：一个含换行的 key 存进去后，之后**每条**命令都会撞上
  `EngineError::InvalidRequest`（退出码 2，消息讲的是 HTTP 头），用户很难联想到根因。
  在 set-key 处拒绝，把一次困惑换成一条清楚的错误。
- **优先级为什么这么排**：OAuth 与文件 key 都是用户显式、主动配置的；env 最不受保护
  （子进程继承、日志、`/proc/*/environ`），因此排最后，且用到它就提示一次。
- **提示时机**：放在 `CredentialProvider::resolve`（每次命令构造一次），不是 `bearer()`
  （分页时每页调一次）。这既满足「提示一次」，也避免刷屏。
- **可关闭是有意的**：CI 里 env key 往往就是正确选择，每次运行都刷一条警告是噪音。
  `OUTLINE_NO_KEY_WARNING=1` 让用户明确表达「我知道」。
- **`auth info` 默认联网**：这是「我的凭证还有效吗」的真答案。`--offline` 供无网/排障使用，
  此时 account/workspace 显示为「上次登录时」。

### R2 审查后的修正（BLOCKER [17]）

- **跨实例写入被拒绝，而不是静默混存**。R1 只在**读取**侧检查绑定；`set-key` 依然可以在
  `OUTLINE_URL=B` 时把 profile 的 origin 改成 B、写入 B 的 key，却**保留 A 的 OAuth session**。
  profile 看起来绑定到 B 了，但 OAuth 优先级高于 API key，于是下一条命令把 **A 的 access token 发给 B**；
  过期后还会去 A 刷新再把新 token 发给 B——R1 [1] 从写入侧完整复活。
  现在 `auth::ensure_bindable` 在 `set-key` 与 `login` 的**网络动作之前**和**事务之内**各检查一次
  （两次都必要：另一进程可能在提示打开期间绑定该 profile）。
- **第二道防线：session 自证来源**。`ProfileCredentials::session_origin()` 从登录时记录的
  `token_endpoint` 推导 session 自己的 origin（discovery 当时已校验过同源），`check_binding` 额外比对它。
  **边界（R3 审查者划清，记在这里以免被误用）**：
  它只覆盖 **OAuth session**——stored API key 没有任何等价机制，因为一个 key 不携带来源信息。
  而且 `token_endpoint` 同样来自磁盘，所以它**不是**对「能任意编辑凭证文件的攻击者」的防线
  （那种攻击者本来就能直接读走 token）。它真正的价值是对**代码 bug** 的第二层：
  任何未来只改写 `profile.origin` 而忘记清理其余凭证的写路径，都会得到「不可用」而非「危险」。
  **不能当作 `ensure_bindable` 的替代品**——写入侧的守卫才是主防线。

### R1 审查后的修正

- **交互输入现在关闭回显**（原列为 deliberate gap，审查者不接受，已改）。加入 `rpassword` 依赖：
  stdin 是 TTY 时用 `read_password()`，非 TTY（管道/文件/测试）走原路径。
  原先的理由（「让用户改用 pipe」）站不住：pipe 本身可能把 key 留在 shell history 或上游进程的
  参数里，而且屏幕回显对录屏、肩窥、终端 scrollback 都是实打实的暴露。为一条安全属性加一个
  小依赖是正确的取舍。
- **凭证绑定实例**（见 Story 2.1/2.3 的 InstanceMismatch）。`set-key` 会把当前实例 origin 一并写入，
  之后把 `OUTLINE_URL` 指向别处时该 key 会被**拒用**而不是发出去。env key 不受此限——
  它由调用者每次随 `OUTLINE_URL` 一起提供，没有可矛盾的存储绑定。
- **set-key 不再覆盖并发轮换**。key 先从 stdin 读完（不持锁等人输入），再取锁、锁内重读文件、
  只改 `api_key` 字段。原实现先读快照、停在输入等待、然后把整个快照写回，会把期间别的进程
  轮换出来的 R2 覆盖成已失效的 R1。

### 已知缺口（有意保留）

- 「提示一次」是**每进程一次**，不是「这台机器上永远一次」。持久化「已提示过」需要另一个状态文件，
  与「凭证目录只放凭证」的洁癖冲突，改用可关闭的环境变量解决。

### References

- [Source: planning/epics.md#Story 2.5]
- [Source: specs/spec-outline-cli/stack.md#认证实现]（API key：Bearer + env 兜底）
- [Source: specs/spec-outline-cli/SPEC.md#CAP-1]

## develop 集成记录（Phase 6）

- **凭证文件 vs 环境变量的优先级现在是配置层的决定**，实现在 `config/credentials.rs::select`：
  `auth = "oauth"` 只取凭证文件；否则文件有东西就用文件，环境变量是回落。
  理由不变（`otl auth set-key` 是有意的动作且文件是 0600；导出的变量常是别的 shell 的残留，
  且对每个子进程可读），但决定点从散在各处的 `if` 变成一个具名类型。
- **stored key 现在同样过释放闸门**。`Config::release` 的两条分支都走 `release_token`，
  所以「profile 声明的实例 ≠ 解析出的实例就拒绝发凭证」这条规则不再只覆盖环境变量。
  `config_credentials.rs` 正反两面都测（拒绝两种、放行一种），并做了反向验证：
  给 `StoredCredential` 加一个绕过闸门的取值方法，两条拒绝测试变红。
- **`otl auth set-key` 现在尊重 `--profile` / `--url` / `--config`**，见 2-1 集成记录第 1 节。
- **「没有凭证」的诊断改说三条路**，见 2-1 集成记录第 4 节。
- **R6 [N1]**：`otl auth info` 曾用自己的一条凭证路径（直读全局 `OUTLINE_API_KEY`）绕过闸门，
  在 profile 生效时把全局 key 发给了 profile 的实例。修法是把 `CredentialProvider` 收窄成
  「只服务 OAuth session」、让 `auth::resolve_credential` 成为唯一凭证路径，并新增
  `credential_paths.rs` 守卫三类凭证各自的唯一入口。完整记录（含「还有哪条路径不经
  `Config::release`」的全表）在 2-1。
- **明文环境 key 的警告现在也覆盖 `OUTLINE_API_KEY_<PROFILE>`**，并指名实际用到的变量。
  原来它只在全局变量被直读时触发，而 CI 用的恰恰是 profile 作用域那个。

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### File List

- crates/otl/src/auth/{source.rs, report.rs}
- crates/otl/src/commands/auth.rs
- crates/otl/tests/auth_api_key.rs
- crates/otl/tests/credential_hygiene.rs
