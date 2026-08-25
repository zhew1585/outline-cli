# Story 2.2: DCR 自注册优先

Status: review (R1 fixes applied)

## Story

As a 自托管 workspace 的用户,
I want 未配置 client_id 时 CLI 自动注册自己,
so that 无需找管理员即可 OAuth 登录。

## Acceptance Criteria

1. **Given** 未配置 client_id 且服务器广播 registration_endpoint
   **When** 执行 `otl auth login`
   **Then** 先绑定随机回环端口，再以实际端口的精确 redirect_uri 走 RFC 7591 注册公共客户端，注册结果连同 registration_access_token 持久化
2. **Given** 已有缓存的注册
   **When** 再次 login
   **Then** 复用缓存 client_id，不重复注册
3. **Given** DCR 不可用（workspace 未开 MCP，端点 404）
   **When** 执行 login
   **Then** 回退输出清晰指引：请管理员在 Settings → Applications 注册并提供 client_id

## Tasks / Subtasks

- [x] Task 1: RFC 7591 注册 (AC: 1)
  - [x] `auth/dcr.rs`：POST JSON，`token_endpoint_auth_method = "none"`（公共客户端）
  - [x] `grant_types = [authorization_code, refresh_token]`、`response_types = [code]`
  - [x] `client_name` / `client_uri` 常量化，管理员在应用列表里能认出来
  - [x] **绑定在前，注册在后**：`login::register_new` 先 `bind_ephemeral()`，再用实际端口注册
- [x] Task 2: 持久化管理凭证 (AC: 1)
  - [x] `ClientRegistration` 存 client_id / client_secret? / registration_access_token /
        registration_client_uri / redirect_uri / dynamic / origin
  - [x] 存在**凭证文件内**（registration_access_token 是 bearer 凭证，不能另开缓存文件）
  - [x] `registration_client_uri` 同源校验（带 bearer 使用，off-origin 会泄漏管理 token）
  - [x] **注册成功即落盘，早于浏览器步骤**（见 Dev Notes 顺序决策）
- [x] Task 3: 缓存复用 (AC: 2)
  - [x] `login::cached_for`：origin 匹配才复用；不匹配则提示并重新注册
  - [x] dynamic 客户端复用时**必须绑回原端口**（注册的是精确 URI）；端口被占则重新注册
  - [x] 重新注册前 best-effort RFC 7592 删除旧注册，避免服务端堆积孤儿
- [x] Task 4: 回退指引 (AC: 3)
  - [x] 无 registration_endpoint，或注册端点 404 → `OAuthError::RegistrationUnavailable`
  - [x] 消息含 Settings → Applications、四个固定 redirect URI 全列、`otl auth login --client-id <id>`
  - [x] 退出码 2（本地可修：拿到一个 client_id 就能继续）
- [x] Task 5: 测试 (AC: 1-3)
  - [x] e2e：DCR 注册 → 断言 client_id、registration_access_token 落盘、redirect_uri 与授权 URL 一致
  - [x] e2e：连续两次 login → 断言 wiremock 收到 `/oauth/register` **恰好 1 次**
  - [x] e2e：元数据无 registration_endpoint → 退出码 2 + 指引含全部四个 URI，且不创建凭证文件
  - [x] 单测：注册响应缺 client_id 被拒；off-origin `registration_client_uri` 被拒；公共客户端无 secret

## Dev Notes

- **顺序决策（关键）**：新注册在**浏览器步骤之前**写盘。理由：`registration_access_token` 是唯一能删掉
  这个客户端的凭证（Outline 管理界面删不掉 DCR 客户端，源码确认）。若先跑同意流程、失败了才落盘，
  用户在同意页放弃 = 服务器上永久多一个删不掉的孤儿客户端。代价是一次多余的写，换不可逆泄漏的消除。
- **端口顺序决策**：DCR 走 ephemeral 端口而不是固定清单。固定清单的意义是「管理员能提前把 4 个 URI 注册好」；
  自注册没这个约束，用 ephemeral 端口可避免和别的程序抢固定端口。反过来，注册必须在**已绑定**之后，
  否则可能注册一个随后被别人占掉的端口，产出一个永远无法完成登录的客户端。
- **复用时的端口困境**：dynamic 客户端绑死了一个 ephemeral 端口。复用时若该端口被占，注册就废了——
  此时 `login::retire` best-effort 删掉旧注册再重新注册，并在 stderr 说明发生了什么（包括删不掉时的原因）。
  实践中概率很低，但不处理就会静默堆积孤儿。
- **origin 不匹配不自动删除**：profile 指向了另一个实例时，旧注册属于**那个**实例。这里只提示
  「去那个实例上跑 `otl auth logout --purge`」，不擅自跨实例发删除请求。已知缺口，见下。
- **`--purge` 不碰管理员的 client_id**：`dynamic = false` 的注册是别人创建的资产，删除它超出 CLI 的权限范畴。
  `logout --purge` 会明确说明它被跳过了。
- 硬编码值全部提常量：`CLIENT_NAME`、`CLIENT_URI`、`AUTH_METHOD_NONE`。

### R1 审查后的修正

- **注册成功但首次保存失败 → 补偿删除**（原为缺口）。`login::persist_registration` 在
  `store.update` 失败时用内存里仍在的管理凭证发 RFC 7592 DELETE。若补偿删除也失败，
  抛 `OrphanedRegistration`，消息里带 **client_id**（公共客户端的 id 不是秘密——它本来就出现在
  授权 URL 里；没有它管理员根本找不到那个应用）但**不带** registration_access_token。
- **旧注册退役失败 → 不再继续注册**（原为「best-effort，失败也继续」）。理由：注册新的会覆盖
  唯一能删掉旧的那个凭证，等于把「一次登录失败」换成「服务器上永久多一个谁也删不掉的应用」。
  现在报 `RetireFailed`（退出码 2）并提供显式逃生阀 `otl auth login --force-new-client`，
  后者会把孤儿的存在明确告知用户，是用户的知情选择而非静默后果。

### 已知缺口（有意保留）

- origin 变更时旧实例上的注册不会被自动删除，只提示用户手动 purge。跨实例自动删除需要同时持有
  两个实例的元数据与管理凭证，收益不足以抵消「CLI 悄悄给另一个服务器发删除请求」的意外性。
- 注册响应不含 `registration_access_token` 时（RFC 未强制），客户端事后无法删除。这不被当作失败，
  但 `auth info` 会明确警告「这个注册无法被 --purge 删除」。

### References

- [Source: planning/epics.md#Story 2.2]
- [Source: specs/spec-outline-cli/stack.md#认证实现]（DCR 清理约束，源码确认）
- [Source: project-context.md]「DCR 注册后必须持久化 registration_access_token。丢了服务器上就删不掉了。」
- RFC 7591（动态注册）、RFC 7592（注册管理）

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Completion Notes List

- 注册记录同时用于「管理员给的 client_id」路径（`dynamic = false`），所以第二次 login 不必再传
  `--client-id`。这不在 AC 里，但复用同一份数据结构后是自然结果，且明显是用户想要的。

### File List

- crates/otl/src/auth/{dcr.rs, login.rs, credentials.rs}
- crates/otl/tests/auth_oauth_e2e.rs
