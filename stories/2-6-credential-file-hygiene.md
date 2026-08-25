# Story 2.6: 凭证文件卫生

Status: review

## Story

As a 把凭证明文放在磁盘上的用户,
I want CLI 严格管好这个文件的权限与写入,
so that 明文存储的风险被压到只剩「磁盘被物理读取」这一层。

## Acceptance Criteria

1. **Given** 全新环境首次写入凭证（login 或 set-key）
   **When** 凭证文件被创建
   **Then** Unix 上文件权限为 0600 且是创建时即设定（禁止先创建再 chmod 的竞态窗口），父目录不存在时一并创建且权限为 0700
2. **Given** 凭证文件权限被改宽（如 0644 或组可读）
   **When** 执行任意需要凭证的命令
   **Then** 拒绝使用该文件并报可读错误，含具体修复命令（如 `chmod 600 <path>`），退出码符合退出码表；不静默降级也不自动改权限
3. **Given** 写入过程中进程被杀或磁盘写失败
   **When** 检查凭证文件
   **Then** 文件内容或为旧值或为新值，绝不为截断/半写状态（同目录 temp → fsync → rename，temp 同为 0600）
4. **Given** 多个 otl 进程并发触发 token 刷新
   **When** 刷新完成
   **Then** 锁文件建议锁保证只有一个进程执行刷新，其余进程读到刷新后的有效凭证，refresh_token 不因竞争而失效
5. **Given** Windows 平台（无 POSIX 权限位）
   **When** 执行 `otl auth info` 或 `otl doctor`
   **Then** 明示该平台的凭证保护依赖用户 profile 目录 ACL，不谎报已设权限
6. **Given** 执行 `otl doctor`
   **When** 报告凭证健康
   **Then** 输出凭证文件路径、存在性、权限是否合规、各 profile 有哪些凭证类型，但绝不打印任何凭证值或其片段

## Tasks / Subtasks

- [x] Task 1: 创建即 0600（无 chmod 竞态）(AC: 1)
  - [x] `auth/secret_file.rs::open_owner_only`：`OpenOptions::create_new(true)` + `OpenOptionsExt::mode(0o600)`
  - [x] 权限位是 `open(2)` 调用的一部分——代码里不存在任何 `set_permissions` 写路径
  - [x] `ensure_dir`：只把**最后一级**目录建为 0700，父级（`~/.config`）用默认权限
  - [x] 已存在的目录不动（悄悄给用户 home 改权限是副作用，不是修复）
- [x] Task 2: 读取前校验权限 (AC: 2)
  - [x] `read_checked`：先 `File::open`，再对**已打开的 fd** 做 `metadata()`（fstat，非 stat），
        避免 check-then-open 之间被换掉
  - [x] `mode & 0o077 != 0` 即拒；错误消息含实际 mode 与 `chmod 600 <绝对路径>`
  - [x] 比 0600 更严（如 0400）视为合法
  - [x] 退出码 2，且**不自动修权限**（测试断言 mode 未变）
  - [x] 读取上限 `MAX_CREDENTIAL_FILE_BYTES = 1 MiB`
- [x] Task 3: 原子写 (AC: 3)
  - [x] 同目录 temp（`.credentials.toml.tmp.<pid>.<随机>`）→ write → `sync_all` → `rename` → 目录 fsync
  - [x] temp 用 `create_new`：预埋的符号链接或残留 temp 无法被写穿
  - [x] temp 同为 0600；`rename` 携带 temp 自己的权限位，因此原子写**永远不会放宽**目标权限
  - [x] 任一步失败即清理 temp
  - [x] 名字冲突重试上限 `TEMP_NAME_ATTEMPTS = 8`
- [x] Task 4: 并发刷新锁 (AC: 4)
  - [x] `auth/lock.rs`：同目录 `credentials.lock` 建议锁（`fs4`，纯 Rust，无 unsafe）
  - [x] `try_lock` 轮询 + `LOCK_TIMEOUT` 上限，不用无超时的阻塞 flock
  - [x] 锁文件同样创建即 0600
  - [x] 锁内重读凭证文件（详见 Story 2.3）
- [x] Task 5: 平台差异不说谎 (AC: 5)
  - [x] `Permissions` 枚举含 `NotApplicable`（Windows），`describe()` 明说「本平台没有 POSIX 权限位，
        otl 不设置任何权限位，保护完全依赖 profile 目录的 per-user ACL」
  - [x] Windows 分支显式处理，不假装成功；权限断言测试 `#[cfg(unix)]`，Windows 有对应分支测试
- [x] Task 6: 健康报告不泄漏 (AC: 6)
  - [x] `auth/report.rs::credential_health`：只由路径、bool、权限位、作者写死的标签构成
  - [x] 模块内**没有任何能触达 token 的代码路径**
  - [x] 权限过宽 / 解析失败时仍产出报告（这时候最需要它），profiles 列表为空 + `usable: false`
  - [x] `otl auth info` 消费它；`otl doctor`（属 Epic 4）复用同一个函数（见 Dev Notes 归属说明）
- [x] Task 7: 凭证值绝不出现在别处
  - [x] 凭证文件之外零落盘；`CredentialFile` / `ProfileCredentials` / `OAuthSession` /
        `ClientRegistration` / `Tokens` / `Pkce` / `ClientAuth` / `StaticCredential` 全部手写 Debug 打 `***`
  - [x] TOML 解析失败只报 line/column：`toml` crate 自己的 Display 会把出错的**源码行**打出来，
        在凭证文件里那一行就是 token
  - [x] OAuth 端点错误文本经 `engine::sanitize::clean_server_text`，以本次请求携带的每个 secret 为脱敏键
- [x] Task 8: 测试 (AC: 1-6)
  - [x] `tests/credential_hygiene.rs`：原子写无残留 temp、反复替换不出现半写、temp 0600、
        0644 被拒且给出修复命令且未被自动修复、`auth info` 报告不可用但不崩、
        `auth info` 输出不含 secret 任何片段（含 `TOKEN-` / `9c7a` 这类子串）、
        并发 4 线程争锁被串行化、`set-key` 落盘 0600
  - [x] 单测：多种过宽 mode 全被拒（0604/0640/0660/0644/0666/0701）、更严的 0400 被接受、
        超大文件被拒、缺失文件不算错误、Windows 描述不含 "0600"
  - [x] e2e：`login` 首次写入时嵌套目录被创建，文件 0600 且目录 0700

## Dev Notes

- **为什么 fstat 而不是 stat**：`fs::metadata(path)` 然后 `File::open(path)` 之间有窗口。
  先 open 再对 fd 取 metadata，检查的和读的必然是同一个 inode。
- **为什么 rename 天然收紧权限**：`rename(2)` 保留的是 temp 文件自己的 inode 与权限位。
  即使目标原本是 0666，一次原子写之后也是 0600。测试
  `rewriting_a_wide_open_file_narrows_it_back_to_0600` 钉住这条。
- **为什么只把最后一级目录设 0700**：`DirBuilder::recursive(true).mode(0o700)` 会把**所有**新建层级
  都设成 0700，包括可能被一并创建的 `~/.config`——那是别的程序也在用的目录，把它变成 0700 是
  意料之外的副作用。所以父级用 `create_dir_all` 默认权限，只有最后一级用 0700 建。
- **为什么已存在的目录不改权限**：`~/.config` 在绝大多数系统上是 0755，这是正常的。
  对目录硬失败会把工具变得不可用；悄悄改权限则是越权。因此：文件权限**硬失败**（它直接装着 token），
  目录权限**只报告**。这是有意识的分界，写在 `ensure_dir` 的文档注释里。
- **为什么 TOML 解析错误只报坐标**：`toml::de::Error` 的 Display 会渲染出错的源码片段。
  凭证文件里那一行长得像 `access_token = "..."`。因此 `credentials::parse_position` 从 span 自己
  算出 line/column，一个字节的文件内容都不进错误消息。测试
  `a_malformed_file_reports_a_position_and_never_its_content` 钉住这条。
- **为什么 `registration_access_token` 也在凭证文件里**：它是 bearer 凭证。project-context 的规则是
  「凭证只存唯一的凭证文件」，所以 DCR 注册记录整体住在凭证文件里，没有单独的注册缓存文件。
- **`fs4` 而不是 std 的 `File::lock`**：std 的文件锁在 Rust 1.89 才稳定，workspace MSRV 是 1.85。
  `fs4` 是纯 Rust（rustix/windows-sys），我们这边不需要 unsafe。注意 `File` 在新工具链上已有
  同名 inherent 方法，会遮蔽 trait 方法，因此调用点写成 `FileExt::try_lock(&file)`，并在注释里
  说明原因。
- **`doctor` 的归属**：`otl doctor` 属于 Epic 4（Story 4.3），由另一条 track 拥有，本 story
  不创建 `commands/doctor.rs`。AC 6/AC 5 里 doctor 需要的能力以
  `auth::report::credential_health()` 的形式提供并测试（`the_credential_health_report_is_reusable_by_doctor`），
  doctor 只需调用它。这是有意留下的接缝，不是遗漏。
- **测试隔离**：所有凭证相关测试通过 `OUTLINE_CONFIG_DIR` 指向 tempfile 目录，绝不触碰开发者真实凭证。
  既有的 Epic 1 测试也补上了同样的隔离（指向一个不存在的目录），否则本机存在凭证文件时它们的
  「缺 OUTLINE_API_KEY」断言会变得依赖环境。

### 已知缺口（有意保留）

- 目录权限过宽只报告不拒绝（见上文理由）。
- Windows 上不主动设置 ACL（stack.md 明确的决策），只在报告里说清依赖 profile 目录 ACL。
- 「进程被 kill 导致半写」用「反复替换 + 每次读回」间接验证，而不是真的在写中途发 SIGKILL：
  原子性由 `rename` 提供，注入式测试只能验证同一件事却引入不确定性。

### References

- [Source: planning/epics.md#Story 2.6]
- [Source: specs/spec-outline-cli/stack.md#认证实现]（凭证存储小节全部条目）
- [Source: specs/spec-outline-cli/failure-modes.md #6 #7 #10]
- [Source: project-context.md]「安全与凭证规则」全部条目

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Debug Log References

- `startup_guard.rs` 的源码扫描禁止 `crates/*/src` 出现 `read_dir`。原本放在
  `secret_file.rs` 里的「原子写不留 temp」测试因此移到 `tests/credential_hygiene.rs`，
  并在原处留了注释说明去向。守卫本身未放宽。

### File List

- crates/otl/src/auth/{secret_file.rs, credentials.rs, lock.rs, paths.rs, report.rs, error.rs}
- crates/otl/tests/credential_hygiene.rs
- crates/otl/tests/{api_e2e.rs, api_list.rs, api_params.rs, paging_e2e.rs, startup_guard.rs}（补测试隔离）
- docs/exit-codes.md
