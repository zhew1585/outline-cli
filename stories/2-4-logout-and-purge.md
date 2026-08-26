# Story 2.4: logout 与 --purge

Status: review (R1 fixes applied)

## Story

As a 注重凭证卫生的用户,
I want 登出时彻底清理,
so that 服务器与本地都不残留。

## Acceptance Criteria

1. **Given** 已 OAuth 登录
   **When** 执行 `otl auth logout`
   **Then** 调用 revocation_endpoint 撤销 tokens，从凭证文件移除该 profile 的凭证条目（文件无剩余凭证时删除文件本身）
2. **Given** 客户端来自 DCR 注册
   **When** 执行 `otl auth logout --purge`
   **Then** 凭 registration_access_token 走 RFC 7592 删除服务器上的注册，本地注册缓存一并清除

## Tasks / Subtasks

- [x] Task 1: 撤销 (AC: 1)
  - [x] `auth/logout.rs`：RFC 7009 form POST 到登录时记录的 `revocation_endpoint`
  - [x] **两个 token 都撤销**：refresh token 先（它才是会话的根），再 access token
  - [x] 无撤销端点时明确告知「只在本地删除了，服务端 token 直到过期前仍有效」，不假装成功
  - [x] 单个 token 撤销失败 → 警告，不阻断本地清理
- [x] Task 2: 本地清理 (AC: 1)
  - [x] 移除该 profile 的 `oauth` 与 `api_key`（= AC 里的「该 profile 的凭证条目」）
  - [x] `CredentialFile::prune` 删掉空的 profile 表，避免留下暗示凭证存在的空 `[profiles.x]`
  - [x] `CredentialStore::save` 在整体为空时**删除文件本身**而不是写一个空壳
  - [x] 其他 profile 的凭证不受影响
- [x] Task 3: `--purge` (AC: 2)
  - [x] `dcr::delete`：DELETE `registration_client_uri`，`Authorization: Bearer <registration_access_token>`
  - [x] 服务器答 404 视为成功（目标已达成）
  - [x] 缺管理凭证 → 返回 `Ok(false)`，报告「无法自动删除，请管理员在 Settings → Applications 处理」
  - [x] 删除失败 → **保留本地记录**，以便重试 `--purge`
  - [x] `dynamic = false`（管理员创建的）→ 跳过并说明
- [x] Task 4: 输出 (AC: 1, 2)
  - [x] 人类可读 / `--json` 双态；JSON 含 `revoked` / `registration_deleted` / `credential_file_removed` / `warnings`
  - [x] warnings 走 stderr（诊断），结果走 stdout（数据）
  - [x] 什么都没存时不报错，只说「这个 profile 没有存任何东西」
- [x] Task 5: 测试 (AC: 1-2)
  - [x] e2e：logout → 断言撤销端点被调用 **2 次**（refresh + access）
  - [x] e2e：`--purge` → 断言带正确 bearer 的 DELETE 恰好 1 次，凭证文件被删除
  - [x] 单测：普通 logout 保留可复用注册，`--purge` 才删
  - [x] 单测：`--purge` 拒绝删除管理员的 client，并说明原因
  - [x] 单测：登出一个 profile 不动另一个；无凭证时不报错
  - [x] 单测：无撤销端点时给出「只在本地删除」的警告

## Dev Notes

### R4 审查后的修正（[N2] / [N3]）

- **[N2] 并发刷新落在撤销窗口里 → 报 exit 0 + `revoked:true`，磁盘上却还有活会话。**
  撤销请求在取锁之前发出（它们慢，持锁跨越会阻塞所有其他 otl 进程），所以并发 refresh 能在
  撤销在途时把轮换后的会话写进文件。`clear_if_unchanged` **正确地不删**它——那不是本次操作的对象——
  但接下来告诉用户的是「Signed out」+ exit 0，而磁盘上躺着一条完全有效的 bearer 会话。
  这就是本轮要消灭的状态换了个入口：不是撤销失败，是并发替换。
  修法：移除事务**内部**回读，仍存在的凭证记为 `survived_concurrent_write` → 警告 + exit 3；
  `Report::signed_out()` 让 `revoked` 变成对 profile 的诚实声明而不只是「本次撤了什么」。
- **[N3] 「可重试 vs 不可能」二分把永久性拒绝讲成可重试。** `dcr::delete` 的 `Err` 里含
  `require_secure`（明文管理 URI）与 `ForeignEndpoint`（异源）——本地规则每次都拒，重试一万次也没用；
  撤销的 400/401/403 同理是服务器拒绝凭证本身。新增 `OAuthError::is_permanent()`，
  这些走 `unrevocable` 措辞，把「可以重试」换成「重试没用，请 `otl auth login` 重新发现端点，
  或用 `--force`」。方向本来就安全（保留分支从不销毁可恢复凭证），错的是措辞。

### R3 审查后的修正（§1 / [26] / [28]）

审查者同意「本地移除绝不能被阻塞」这个方向，但指出 R2 的实现把它变成了**单向损坏**：
破坏性的一半（本地删除）无条件执行，补救性的一半（撤销）却锚在 `OUTLINE_URL` 上。
锚点不一致，于是指错实例时净效果是纯损失，还 exit 0。三条都改了：

- **撤销锚到 session 自己记录的 origin**（`session_origin()`，即 token_endpoint 的 origin），
  不再锚到环境变量。`dcr::delete` 早就是这么做的——同一条命令里 DCR 删除跨实例成功、
  token 撤销被拒，正好证明正确的锚点已经存在、只是没用在撤销上。
  把 A 的 token 撤销到 A 不向 B 泄露任何东西；同源校验要防的是被篡改的文件，
  而那个基准是凭证自证的 issuer，不是用户可以随便指的环境变量。
- **撤销失败进入退出码**。新增 `Report::unrevocable`（无法重试：没有撤销端点／端点不可用）
  与 `Report::retryable`（可以重试：端点存在但这次失败），两者都置 `remote_cleanup_failed` → exit 3。
  R2 里三条撤销失败分支**一条都没有**置这个标志，于是模块自述的
  「anything that fails is reported... and the exit code says so」对撤销路径是假的。
- **logout 不再走 `instance_origin` 传输门**。它根本不需要 base_url——要联系的 URL 全在凭证文件里。
  新增 `open_store_without_instance()`。绑定在明文 http 上的 profile（早于本规则、或从别的机器拷来）
  以前 `logout` 直接 exit 2，用户只能手删文件、连 RAT 一起丢、把 DCR 注册变永久孤儿——
  正是 project-context DON'T-MISS 条款要防的终局。
- **默认不做不可逆的事**（审查者 §1.4 第 4 点）。可重试的失败**保留**本地凭证并 exit 3；
  `--force` 是用户明说「我知道这些撤不掉了，还是删」。`--force` 同样覆盖注册记录，
  并在警告里点名那个孤儿 client id——否则用户还是只能 `rm`，那是更糟的静默版本。

### R2 审查后的修正（MAJOR [20]）

- **清理决定只应用到本次真正操作过的那个对象**。R1 的 logout 用**网络之前**的快照决定删什么，
  却把决定无条件作用到**锁内重读**的新状态：P1 purge 删掉 C1 期间 P2 完成 login 写入 C2/RAT2，
  P1 随后直接清空 client → C2 还在服务器上而 RAT2 永久丢失。普通 logout 同理会删掉并发写入的新 session。
  现在每个字段都经 `clear_if_unchanged` + 显式比较函数（session 比 access token，registration 比
  client id 与管理 URI）。比较不上就保留——留下一个多余凭证可以再跑一次 logout 清掉，
  误删一个管理 token 则不可恢复，方向选安全的一侧。

### R1 审查后的修正（BLOCKER）

- **purge 失败仍清本地管理凭证**。原实现的清除条件是 `options.purge || registration_deleted`，
  于是 `--purge` 无条件清掉本地 `registration_access_token` / `registration_client_uri`——
  而服务器上的注册还在，且这两个值是**唯一**能删除它的东西。这正是 project-context 里
  「丢了服务器上就删不掉了」要防的事。
  现在由 `drop_registration` 决定：**只有服务端确认删除成功**（`registration_deleted`）才清；
  管理员创建的 client（`dynamic = false`）没有管理凭证、服务器上也没有属于我们的东西，清掉不产生孤儿。
- **退出码要反映部分失败**。新增 `Report::remote_cleanup_failed`；`run_logout` 在本地清理与报告
  之后返回退出码 3，stderr 说明「本地已登出，但服务器上的应用仍在，可重试 --purge」。
  已登记进 docs/exit-codes.md。
- 测试：`a_failed_purge_keeps_the_credential_that_can_retry_it`（DELETE 返回 503 → 断言
  rat/uri 仍在盘上、session 已删、退出码非 0）与 `a_retried_purge_succeeds_once_the_server_recovers`
  （用上一步留下的凭证重试成功、文件被删）。


- **为什么 logout 保留注册、只有 --purge 删**：DCR 注册是**可复用**的——下次 `otl auth login`
  会复用它而不是再建一个应用。每次登出都删，等于每次登录都在服务器上多一个应用。
  `--purge` 面向「彻底离开这台机器」的场景，也是删掉 DCR 客户端的**唯一**手段
  （Outline 管理界面删不掉它，源码确认）。
- **为什么 logout 也删 api_key**：AC 的措辞是「移除该 profile 的凭证条目」。把「logout」理解为
  「忘掉这个 profile 的凭证」是一致且可预期的，命令帮助文本里写明了这一点，避免惊讶。
  （备选设计是加 `--all`，被否决：多一个 flag 换一点点保守性，不值。）
- **撤销顺序有意为之**：refresh token 是会话的根，先撤它，即使随后 access token 撤销失败，
  会话也无法续期。反过来则不然。
- **本地清理无条件执行**：实例不可达时也要清理。用户要的是「本地不要再留着」，
  服务端做不到的部分逐条报告，不静默吞掉。
- **删除失败保留本地记录**：这条是为了 `--purge` 可重试。若失败还把 `registration_client_uri`
  和 `registration_access_token` 删了，服务器上就永久留下一个谁也删不掉的客户端。
- 撤销与注册删除都可能回显我们发过去的值，因此两个调用都声明了 secret 列表交给
  `endpoint::sanitize` 脱敏。

### References

- [Source: planning/epics.md#Story 2.4]
- [Source: specs/spec-outline-cli/stack.md#认证实现]（DCR 清理约束）
- RFC 7009（撤销）、RFC 7592（注册删除）

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### File List

- crates/otl/src/auth/{logout.rs, dcr.rs, oauth.rs}
- crates/otl/src/commands/auth.rs
- crates/otl/tests/auth_oauth_e2e.rs
