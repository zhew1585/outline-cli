# Story 2.3: 自动续期与轮换安全

Status: review (R1 fixes applied)

## Story

As a 长期使用的用户,
I want token 过期自动续期,
so that 永远不需要手动重新登录。

## Acceptance Criteria

1. **Given** access token 已过期
   **When** 任意命令发起请求
   **Then** 请求通道自动用 refresh token 换新（单飞：锁文件建议锁保证并发进程只刷新一次），新 access/refresh token 原子写入凭证文件后重放原请求
2. **Given** refresh token 已失效或被撤销
   **When** 刷新失败
   **Then** 提示执行 `otl auth login` 重新登录，退出码符合退出码表

## Tasks / Subtasks

- [x] Task 1: 引擎侧续期钩子（唯一请求通道）(AC: 1)
  - [x] `engine::credential`：`CredentialSource { bearer(), renew(rejected) }` + `CredentialFault` + `StaticCredential`
  - [x] `Client::with_credentials` / `with_credentials_and_timeout`；`Client::new` 包一层 `StaticCredential`（旧调用方行为不变）
  - [x] `Client::send`：每请求取一次 bearer；收到 401 时**恰好问一次** renew 并原样重放
  - [x] engine 完全不含 OAuth/Outline 字样：「续期」在引擎眼里只是「换一个字符串」
  - [x] `EngineError::Credential(CredentialError)`，otl 侧 `errors.rs` 加一个 match arm 决定退出码
- [x] Task 2: 双触发续期 (AC: 1)
  - [x] 主动：`bearer()` 检查 `expires_at`，进入 `EXPIRY_SKEW_SECONDS = 60` 窗口即先刷新（避免「刚好过期」）
  - [x] 被动：401 后 `renew(rejected)` —— 服务器对自家 token 有最终解释权，记录里的过期时间不算
  - [x] 未记录过期时间时不每次刷新，交给 401 兜底
- [x] Task 3: 跨进程单飞 (AC: 1)
  - [x] `auth/lock.rs`：`credentials.lock` 同目录建议锁，`try_lock` 轮询 + `LOCK_TIMEOUT = 30s` 上限
  - [x] 锁内**重读凭证文件**：先拿到锁的进程已经轮换过，等待者直接用它的结果，不再刷一次
  - [x] `is_usable(session, rejected)`：被拒绝的那个 access token 即使「未过期」也不复用
  - [x] 锁只在刷新期间持有，不覆盖普通请求
- [x] Task 4: 轮换持久化 (AC: 1)
  - [x] `merge`：响应带新 refresh token 就换掉；不带则保留旧的（RFC 6749 允许省略）
  - [x] 响应未携带的字段（scope/account/workspace/client_id/端点）在 merge 后存活
  - [x] 原子写（Story 2.6）
  - [x] **写失败 = 硬错误** `OAuthError::RotationLost`：此刻旧 refresh token 已被服务器作废，
        沉默会留下一个永远无法续期的凭证文件
- [x] Task 5: 失效处理 (AC: 2)
  - [x] token 端点 4xx（除 429）→ `SessionExpired`，消息含 `otl auth login`，退出码 4
  - [x] 429 / 5xx 不算「grant 被拒」：分别映射为 8 / 6，凭证不动
  - [x] 服务器错误文本经 `clean_server_text` 脱敏（会回显 refresh token）
- [x] Task 6: 测试 (AC: 1-2)
  - [x] `engine/tests/credential_renewal.rs`：renew 恰好一次、无 renew 能力时原样上报 401、
        renew 失败不伪装成 401、bearer 失败时一个请求都不发
  - [x] e2e：过期 token → 主动刷新 → 重放 → 断言新旧 token 落盘正确
  - [x] e2e：未过期但被 401 → 被动刷新 → 重放
  - [x] e2e：**两个真实 otl 进程并发** → 断言 wiremock 只收到 1 次 refresh grant，两者都成功
  - [x] e2e：invalid_grant → 退出码 4 + `otl auth login`，且 refresh token 不被回显
  - [x] e2e：刷新成功但目录不可写 → 退出码 4 + 「rotated」+ `otl auth login`

## Dev Notes

- **为什么续期必须在引擎的唯一通道里**：分页会对同一个命令发多次请求，`docs export` 未来还会并发。
  在命令层刷新等于每个命令各写一份、各出一次 bug。挂在 `Client::send` 上则本地校验、429 退避、
  错误映射、续期共享同一条路径。为此 engine 新增了一个**服务无关**抽象：`CredentialSource`。
  「renew」在 engine 看来只是「拿回一个不同的字符串」，没有 OAuth 词汇。
- **为什么至多重放一次**：`send` 用 `renewed: bool` 而非计数。一个总是返回被拒 token 的 source
  不能让通道空转；第二次 401 直接把服务器自己的 401 交出去。
- **为什么是文件锁而不是进程内 Mutex**：refresh token 每次使用都轮换（实测确认）。两个 **otl 进程**
  同时刷新，会各花掉同一个 refresh token，其中一个拿到 `invalid_grant`，而凭证文件里留下的可能是
  服务器已经作废的那个。进程内互斥对此无能为力。
- **锁内重读是单飞的另一半**：只有锁不够——等到锁的进程如果继续刷新，还是会花掉刚被轮换掉的 token。
  必须在锁内重读文件，看到已经新鲜就直接用。这条是 `concurrent_processes_refresh_exactly_once`
  真正在验的东西（wiremock `.expect(1)`）。
- **`RotationLost` 为什么是退出码 4**：根因是磁盘写失败（像是配置问题），但可行的下一步只有重新登录，
  因为服务器端的旧 refresh token 已经死了。已登记进 `docs/exit-codes.md` 的注释里。
- **60 秒 skew** 覆盖时钟偏差 + 请求在途时间。宁可早刷一次（成本：一次多余的 token 请求），
  也不要晚一步（成本：一次失败请求 + 一次重放）。
- **过期时间存绝对值**（unix 秒）而非服务器给的 `expires_in`：读盘时不需要「相对于何时」的记忆。

### R1 审查后的修正

- **续期后旧 token 仍在脱敏上下文里**。`Client::send` 现在维护 `used: Vec<String>`（本次请求用过的
  每一个凭证），全部传给脱敏管线。服务器见过 T1，被攻陷的服务器完全可以在第二次响应里故意回显 T1；
  只知道 T2 的管线会把 T1 原样打出来。
- **单飞在短寿命 token 上退化**。`is_usable` 拆成两个语义明确的谓词：`fresh_enough`（主动路径，
  **应用** 60 秒安全裕量，为的是提前刷新）与 `superseded_by`（锁内复用判定，**不应用**裕量，
  只问「是不是和已知作废的那个不同」+「是否真的还没过期」）。原来对等待者也套裕量，
  会导致服务器给出短寿命 token 时每个排队进程各刷一次、各花掉一个一次性 refresh token，
  第二个开始必然 invalid_grant——恰好破坏「只刷新一次」。
  测试 `concurrent_processes_refresh_once_even_with_a_short_lived_token` 用 `expires_in=5` +
  wiremock `.expect(1)` 钉住。
- **目录 fsync 错误被吞**。`sync_dir` 现在返回 `Result` 并由 `write_atomic` 向上传播；
  Windows 分支显式 no-op（该平台无法把目录当文件打开，`MoveFileEx` 本身已提供原子替换）。
  轮换后写盘只要不能确认落盘，就必须报 `RotationLost`。
- **锁只包住 refresh 路径**（见 Story 2.6 的事务锁修正）。

### 已知缺口（有意保留）

- 锁超时 30 秒后报错并建议删除锁文件，而不是抢锁。抢锁需要 PID/心跳存活检测，跨平台正确实现的代价
  远超收益，而 CLI 的刷新过程只有一次 HTTP 往返，正常不可能占住 30 秒。
- 主动刷新只看本地记录的过期时间，不做时钟同步检测。系统时钟严重偏移时会退化为「靠 401 兜底」，
  功能仍然正确，只是多一次往返。

### References

- [Source: planning/epics.md#Story 2.3]
- [Source: specs/spec-outline-cli/SPEC.md#Constraints]（refresh_token 每次刷新都轮换）
- [Source: specs/spec-outline-cli/failure-modes.md #6]
- [Source: project-context.md]「所有 HTTP 请求必须经唯一请求通道」「刷新单飞」「持久化失败必须显式报错」

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Completion Notes List

- `engine/src/client.rs` 的 `send` 被拆成 `send`（凭证 + 续期重放）/ `send_once`（一次往返 + 429 吸收）
  / `absorb_rate_limit`，为的是每个函数留在 50 行以内。行为对既有调用方完全不变（25 个 execute
  测试、11 个 rate_limit 测试未改一行即通过）。
- `display_origin` / `send_error` / `body_error` 现在接受当前 token 作参数（原先读 `self.token`），
  因为凭证会变。脱敏语义不变。

### File List

- crates/engine/src/{credential.rs, client.rs, error.rs, lib.rs}
- crates/engine/tests/credential_renewal.rs
- crates/otl/src/auth/{source.rs, lock.rs, oauth.rs}
- crates/otl/src/errors.rs（新增一个 match arm）
- crates/otl/tests/auth_oauth_e2e.rs
