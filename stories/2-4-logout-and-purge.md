# Story 2.4: logout 与 --purge

Status: review

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
