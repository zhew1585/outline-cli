# Story 2.6: 凭证文件卫生

Status: review (R1 fixes applied)

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

### R4 审查后的修正（[N1]）

- **并发化没有消除 DoS，只是换了机制。** R3 用 worker 线程消掉了串行读的头阻塞，但
  `MAX_LIVE_HANDLERS=64` 封顶处**超限即丢弃、不读不答**——而超限的那条连接可能**就是**真正的回调
  （浏览器对「被接受后立即关闭、无任何响应」的顶层导航不会自动重试）。加上 `READ_TIMEOUT` 是
  **每次 read** 的超时而非整条请求线的总时限，攻击者每 <2s 送 1 字节就能让每次 read 都成功、
  把一个槽占住约 2.3 小时（8 KiB 行上限），于是 64 个槽可被永久占满、「封顶持续释放」不成立。
  审查者用 out-of-tree harness 实测 64/70/400 条逐字节连接均让 login 失败。
  两处都修了：
  - `REQUEST_BUDGET` 现在是**整条请求线的总预算**（`callback_request::read_request_target` 自己
    逐字节推进并检查 deadline，不再依赖 socket 的 per-read 超时）；
  - 超限时**不再丢弃**，改为在 accept 循环上**内联处理**并给一个极小的
    `SATURATED_REQUEST_BUDGET`（50ms）。线程数仍有上限，accept 循环继续排空，
    而回调**永远会被读到**。现在的保证是：泛洪最多把回调**推迟** listen backlog × 50ms，
    不可能让它消失。
- **我的 R3 测试为什么没抓到（这条要记住）**：`stalled_connections_cannot_starve_the_real_redirect`
  只用了 **8** 条连接，远低于 64 的封顶，**按构造永远走不到「槽满」那条分支**；
  另两个测试的干扰连接立即关闭或立即发完整请求，成本约 0，也不占槽。
  这是同一 track 上第三次「断言对被测行为不敏感」（前两次：引用了一条不存在的提示、
  只找自由函数的扫描器）。所以本轮的新测试都做了**反向验证**：把修复逐半撤掉、确认测试真的变红。
  过程中还发现我第一版的 `one_peer_cannot_hold_a_handler_slot_beyond_the_request_budget`
  **根本没调 `wait_for_code`**，因此什么都没接受、`live_handlers()` 恒为 0、对着 bug 也是绿的——
  已改为在 `thread::scope` 里真的跑 accept 循环，并用轮询代替固定 sleep（既保住断言的内容，
  又不让它变成对机器速度的断言）。
- **[N4] 守卫的盲区已写进注释**：扫描器覆盖 `impl` 方法与 trait 默认方法，但**不覆盖闭包与
  `macro_rules!` 展开出的函数**（当前 `crates/*/src` 两者都不存在）。记录下来，因为守卫静默失效
  比没有守卫更糟——这正是它第一版只找自由函数的那种失效。

### R3 审查后的修正

- **回调改为并发处理**（[13] PARTIAL / [27] MAJOR）。R2 删掉了连接预算、用「暂停即是界」钳住
  迭代次数，那半边成立；但真正的危害换了机制仍然可达：**单线程串行 accept + 每连接 10s 读窗**，
  握手后一个字节都不发的连接就能占住唯一的 reader，浏览器回调排在后面。审查者用 out-of-tree
  harness 实测 `stalled=3, budget=25s -> LOGIN FAILED`；按同一算式 240s 只需 24 条静默连接。
  现在每条连接交给自己的 worker 线程（回调是单次事件，不需要串行），读窗降到 2s，
  并发上限 `MAX_LIVE_HANDLERS = 64` 且超限直接丢弃而非排队——排队才会吃掉 deadline。
  R2 那个 60 个 favicon 的测试**按构造抓不到这个**（快连接不占读窗），新测试用 8 条**保持静默不关闭**
  的连接 + 6s 预算（串行下需要 16s），算术上只有并发才可能通过。
- **hygiene 守卫扩到函数长度与嵌套层数**（[30]）。R2 只实现了三条铁律里的一条，
  而写那条守卫的同一个提交就违反了另一条。守卫现在同时检查文件行数、函数行数、嵌套深度，
  并且**能看到 impl 块里的方法**（第一版只认顶层函数，会漏掉这个代码库的大部分代码，
  给出一个令人安心的空结果）。修掉了 4 处生产代码违规：`login::run`、`errors::classify`、
  `client::extract_error_parts`、`paginate::fetch_all_pages`。
  **有意的收窄（这次写明）**：函数长度/嵌套只管 `src/`，测试函数豁免——那条规则约束的是
  「改动一个函数时要在脑子里装多少东西」，而测试是没有分支的线性脚本，拆成 helper 往往反而
  毁掉让它可读的那条叙事线。文件行数**不豁免测试**：1500 行的文件不论装什么都难导航。
- **超限测试文件已拆**：`auth_oauth_e2e.rs`（1667 行）拆成 `auth_login_e2e` / `auth_refresh_e2e` /
  `auth_logout_e2e` + 共享 `oauth_harness`；`engine/tests/pagination.rs`（1264 行）拆成
  `pagination`（核心翻页）+ `pagination_echo`（服务器谎报 offset/hint/descriptor）+ 共享 `paging_harness`。

### R2 审查后的修正

- **回调连接预算彻底取消**（[13] NOT FIXED → fixed）。R1 只钳制了总 deadline 与单连接 read window，
  循环仍有固定 `MAX_CONNECTIONS=4096`，而**合法的非回调 GET 不退避**——本机任何进程快发 4096 个
  `/favicon.ico` 就能在 deadline 之前耗尽循环、让登录失败。
  现在循环**只由 deadline 界定**，且任何没带来回调的连接（空连接、畸形请求、非回调路径）之后都强制
  退避 5ms——退避本身就是迭代次数的上界，不需要另设预算。给本机任意非特权用户一个必然弄坏登录的手段
  是不能接受的。
- **issuer 只折叠一个尾斜杠**（[24]）。`trim_end_matches('/')` 会折叠任意数量，
  而 `https://host/tenant///` 与 `https://host/tenant` 对不归一化重复分隔符的路径路由反代来说
  是不同的安全域。改用 `strip_suffix('/')`（恰好一个）。
- **健康报告说实话**（[23]）。0755 目录是被**允许**的（判据是「他人不可写」），但 R1 把它描述成
  "owner-only"——声称了比实际更强的权限状态。现在报告真实 mode：`0755, not writable by other users`。
- **文件行数超限**（[25]）。`secret_file.rs` 到了 938 行，违反 project-context 的 800 行铁律。
  拆成 `secret_file.rs`（读写原语）+ `file_guard.rs`（权限/类型/属主/目录判据）；
  修复过程中 `source.rs` 也涨到 834 行，拆成 `source.rs`（provider 与续期）+
  `selection.rs`（选哪个凭证、能否用于本实例）。
  并新增 `tests/source_hygiene.rs` 把这条铁律变成**常驻门禁**——功能测试永远发现不了架构约束违规，
  上次就是这么漏过去的；现在超限会直接让测试失败，并在接近 700 行时打提示。

### R1 审查后的修正

- **目录权限从「只报告」改为「按写权限拒绝」**。原来的取舍（文件硬失败、目录只报告）在写权限上
  站不住：能写目录的人可以替换凭证文件，也可以替换那个让 refresh 单飞的 `.lock`。
  现在的界线是 **write**：`ensure_dir` / `require_private_dir` 拒绝 group/other **可写**的目录
  (`0o022`) 和非本人所有的目录，但**接受** `0755`——目录可读不泄漏任何凭证（文件自身是 0600），
  而 `~/.config` 在绝大多数系统上就是 0755，对它硬失败会让工具不可用。
  测试同时钉住三种情形：0777 拒、0770 拒、0755 accept。
- **健康报告真的报告目录了**（源码注释原先声称却没做）。`CredentialHealth` 新增
  `directory` / `directory_problem`，并计入 `usable`。
- **锁文件与凭证文件按 fd 校验**：类型（必须是 regular file）、owner（必须是本人）、mode（owner-only）。
  预埋的 0666 锁文件、symlink 锁路径都会被拒。
- **锁路径替换可被检测**：`still_same_file` 在取到锁后比对 (dev, ino)。锁在 inode 上，
  但别的进程按 pathname 找它——路径被 unlink/rename 后，下一个进程会在**新 inode** 上「拿到锁」，
  两边都以为独占。这条把竞态变成明确报错。
- **锁超时提示不再建议删除锁文件**。holder 还在跑时删掉 pathname 正是上面那种分裂。
  新文案让用户等待或找卡住的 otl 进程，并明确写出「不要删」。测试
  `a_contended_lock_never_advises_deleting_the_lock_file` 反向钉住。
- **凭证路径用 `O_NOFOLLOW | O_NONBLOCK` 打开**（新增 `rustix` 依赖，仅 unix）。
  `O_NOFOLLOW` 让 symlink 变成错误而不是静默重定向（`symlink_metadata` 预检会留窗口，
  在 open 里拒绝则没有窗口）；`O_NONBLOCK` 是因为**对无写端的 FIFO 做 open 会永久阻塞**——
  在任何权限或类型检查之前，一个 0600 的 FIFO 就能让所有需要凭证的命令永久挂起。
  测试里真的创建 FIFO 来验证（若测试回归，测试本身会挂住）。
- **目录 fsync 错误向上传播**（见 Story 2.3）。
- **所有凭证文件写入纳入同一把事务锁**：`CredentialStore::update` 取锁 → **锁内 load** → 修改 → save。
  原来只有 refresh 路径持锁，`set-key` / `login` / `logout` 各自「读快照 → 改 → 写回」，
  与并发 refresh 相互覆盖。锁绝不跨越等人输入或等浏览器的阶段。

### 已知缺口（有意保留）
- Windows 上不主动设置 ACL（stack.md 明确的决策），只在报告里说清依赖 profile 目录 ACL。
- 「进程被 kill 导致半写」用「反复替换 + 每次读回」间接验证，而不是真的在写中途发 SIGKILL：
  原子性由 `rename` 提供，注入式测试只能验证同一件事却引入不确定性。

### References

- [Source: planning/epics.md#Story 2.6]
- [Source: specs/spec-outline-cli/stack.md#认证实现]（凭证存储小节全部条目）
- [Source: specs/spec-outline-cli/failure-modes.md #6 #7 #10]
- [Source: project-context.md]「安全与凭证规则」全部条目

## develop 集成记录（Phase 6）

- **解析错误的剥值规则与配置文件那一侧无关**，并且现在有断言钉住这件事。
  `config/file.rs` 的 `classify_parse_error` 把解析器措辞归类成人话描述；凭证文件这一侧
  只保留行列位置，一个字都不从解析器那里拿。新增
  `a_malformed_credential_file_is_reported_without_any_of_its_content`（逐种措辞验证，
  连 4 字符片段都不泄漏——4 是 sanitizer 自己的片段阈值）与
  `the_credential_files_parse_rule_does_not_borrow_the_config_files_wording`
  （分类器的每条描述都不得出现）。两者分别对不同回退变红。
  未知**键**不在测试清单里，是有意的：凭证文件容忍未知键（`version` 才是拒绝
  「本 build 读不懂的格式」的机制，降级运行不该被新版加的键噎住）。
- **服务器文本清洗补上 27 个 `Cf` 码点**，见 2-1 集成记录第 5 节。凭证文件路径与诊断
  经由 `sanitize_path` / `text::quote`，现在两层读同一张表。

## CI 修复：Windows clippy 在 `file_guard.rs` 上失败

`develop` 上 windows-latest 的 `cargo clippy --workspace --all-targets -- -D warnings` 报了两条：

```
error: unused import: `File`            crates/otl/src/auth/file_guard.rs:10
error: variable does not need to be mutable   crates/otl/src/auth/file_guard.rs:136
```

两处都只在 `#[cfg(not(unix))]` 下失效：`File` 只被 unix 版 `require_private_dir` 用，
`mut` 只为 unix 分支里的 `builder.mode(DIR_MODE)` 而存在。macOS/Linux 全绿。

**修法不是加 `#[allow]`**：
- `use std::fs::File` 移进 unix 版 `require_private_dir` 函数体（它本来就有一个局部
  `use std::os::unix::fs::MetadataExt`）；
- `DirBuilder` 的构造拆成 `dir_builder()` 的两个 `#[cfg]` 版本——这个文件里**其他每一处**
  平台差异本来就是这个形状（`classify`、`require_private_dir`、`directory_mode`），
  唯独这一处用了「一个绑定 + 内嵌 cfg 块」，而这正是它能在 Windows 上出问题的原因。
  Windows 版的文档注释说明保护来自用户 profile 目录的 ACL，与凭证文件本身同一条依据。

### 为什么没被本地门禁抓到

`file_guard.rs` 是最后一轮把 `secret_file.rs` 拆分时**新建**的文件，拆完没有再跑
`scripts/win-check.sh`——那个脚本正是为这类问题写的（此前当场抓到过 `Durability::Flushed`
未构造和一个多余 import）。

已验证脚本本身没有缺口：
- 把两处错误装回去，`win-check.sh` 复现 CI 的**同样两条**（同文件、同行号 10 与 136）；
- 在 `#[cfg(test)] mod` 里植入一个未加 guard 的 `use std::os::unix::fs::PermissionsExt`，
  脚本同样报错——即 `--all-targets` 确实覆盖了 `lib test` 目标（CI 那两条错误分别来自
  `lib` 与 `lib test`，只查 `lib` 会漏掉测试代码里的同类问题）。

### 让它不容易被忘

根因是**它不在清单上**：README 的 Development 段列了 `cargo test` / `clippy` / `fmt` /
`bench-startup` / `check-binary-size`，唯独没有 `win-check.sh`。所以：

1. README Development 段加上 `bash scripts/win-check.sh`，并写明为什么它是五条里最容易漏、
   漏掉代价最大的一条：另外四条都只为当前机器跑，而 `#[cfg(unix)]` 会在 Windows 上留下
   未使用的 import / `mut` / 整个函数，CI 用同一条 clippy 加 `-D warnings` 把每一条变成
   构建失败——本地完全看不见，只能主动要求。
2. `portability.rs` 的模块文档原本声称「把同样的检查搬到了本地门禁」，这句**只对一半成立**：
   它抓「未加 guard 地引用平台模块」，抓不到「cfg 留下的死代码」——后者是 lint、
   对 cfg 敏感，判定它就等于为另一个 target 跑一遍编译器。文档改准，并新增
   `the_windows_cross_check_is_offered_to_developers`：断言脚本还在、还可读、仍然传
   `--all-targets` 与 `-D warnings`、且仍被 README 的 Development 段点名。
   它**不**声称「跑过了」（本地无从得知），它守的是**提醒本身不被删掉或改弱**。
   三条断言各自做了回退验证：删掉 README 那一行、去掉 `--all-targets`、去掉 `-D warnings`，
   分别变红。
3. **完成清单**（本 track 之后每一轮都按这个跑）：`cargo fmt --all -- --check`、
   `cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、
   **`./scripts/win-check.sh`**、`./scripts/check-binary-size.sh`。
   规则：**新建或拆分任何带 `#[cfg]` 的文件之后，必须跑 `win-check.sh`。**

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Debug Log References

- `startup_guard.rs` 的源码扫描禁止 `crates/*/src` 出现 `read_dir`。原本放在
  `secret_file.rs` 里的「原子写不留 temp」测试因此移到 `tests/credential_hygiene.rs`，
  并在原处留了注释说明去向。守卫本身未放宽。
- R1 修复期间新增两个依赖：`rpassword`（关闭终端回显）与 `rustix`（`O_NOFOLLOW`/`O_NONBLOCK` 与
  fd 上的 fstat/geteuid，避免自己写 unsafe）。两者都已在依赖图里（tempfile / fs4 传递引入 rustix）。

### File List

- crates/otl/src/auth/{secret_file.rs, credentials.rs, lock.rs, paths.rs, report.rs, error.rs}
- crates/otl/tests/credential_hygiene.rs
- crates/otl/tests/{api_e2e.rs, api_list.rs, api_params.rs, paging_e2e.rs, startup_guard.rs}（补测试隔离）
- docs/exit-codes.md
