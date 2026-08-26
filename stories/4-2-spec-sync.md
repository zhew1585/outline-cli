# Story 4.2: spec sync

Status: review

## Story

As a 想用最新端点的用户,
I want `otl spec sync` 拉取上游 spec,
so that 新端点无需等 CLI 发版。

## Acceptance Criteria

1. **Given** 执行 `otl spec sync`
   **When** 上游有更新
   **Then** 运行时解析一次并以 bincode 落缓存（key 为 spec hash + CLI 版本 + IR schema 版本，原子 rename 写入），`otl api list` 立即含新端点
2. **Given** 缓存文件损坏
   **When** 任意命令启动
   **Then** 自动废弃缓存回退内置 IR，不崩溃
3. **Given** 未执行 `spec sync`
   **When** 运行任意命令
   **Then** 绝不联网检查 spec、绝无自动更新检查（NFR4），且启动路径零 OpenAPI 解析、`otl --help` 冷启动 <10ms（NFR1）

## Tasks / Subtasks

- [x] Task 1: 共用 spec 编译器 (AC: 1, 3)
  - [x] 新 crate `crates/speccompile`（package `spec-compile`）：OpenAPI JSON → 中性 IR 数据结构
  - [x] build.rs 与运行时同一套解析代码；build.rs 只保留静态表渲染
  - [x] panic 全部改为 typed error（`CompileError`），递归深度受限（引用环不再是挂死或 panic）
  - [x] 操作名/请求路径安全校验（`is_safe_op_name` / `is_safe_path`）
  - [x] `tests/spec_parity.rs`：vendored spec 编译期产物与运行时产物逐字段相等
- [x] Task 2: 文档下载通道 (AC: 1)
  - [x] `engine::fetch::DocumentFetch`：无认证 GET、16 MiB 上限、UTF-8 校验、错误响应体不回显
  - [x] 复用 `RetryPolicy` + `Throttle`：429 按 Retry-After/退避重试、每次尝试过节流、耗尽为独立错误（退 8）
  - [x] URL 校验（http/https、有 host、无 userinfo），错误信息不回显 URL
  - [x] 独立错误域 `FetchError` + `errors::map_fetch_error`（退出码语义不变，文案不提 Outline 凭证/实例）
- [x] Task 3: bincode IR 缓存 (AC: 1, 2)
  - [x] `crates/otl/src/spec/cache.rs`：magic + 布局版本 + SHA-256 + bincode body
  - [x] 缓存 key 三要素：spec hash（provenance）+ CLI 版本 + IR schema 版本，任一不符整体废弃
  - [x] 位置经 `directories` 解析（Linux `~/.cache/outline-cli`、macOS `~/Library/Caches/outline-cli`、Windows `%LOCALAPPDATA%\outline-cli\cache`）；`OTL_CACHE_DIR` 覆盖
  - [x] 写入：同目录随机名 temp（`tempfile`，O_EXCL + Unix 0600）→ fsync → persist（Windows 亦替换）；失败即清理
  - [x] 写入前校验：load 侧全部规则（含版本）+ 编码长度上限（bincode limit 只约束解码）
  - [x] 读取：文件类型（不跟随 symlink）→ 尺寸 → fstat 复查 → 限长读取 → magic → 布局版本 → 校验和 →
        有界解码（元素数 ≤ 8192 + 解码后 footprint ≤ 8 MiB）且无尾随字节 → 版本 → provenance → 逐 op 校验
- [x] Task 4: 运行时表解析 (AC: 1, 2, 3)
  - [x] `ops::table()`：`OnceLock` 惰性解析，缓存优先、内置兜底
  - [x] 缓存不可用 → stderr 一行警告（区分 damaged / outdated）+ 修复命令，退出码不受影响
  - [x] `--help`/`--version` 不触发任何缓存 I/O
- [x] Task 5: 命令 (AC: 1)
  - [x] `otl spec sync [--url URL] [--spec PATH] [--force]`、`otl spec reset`
  - [x] 双态输出：`--json` 结构化报告，TTY 人类可读；进度到 stderr
  - [x] 同 hash 且缓存可用 → 不重写（`--force` 强制）；损坏缓存必重建
  - [x] provenance 只记 origin / `local file`，不落完整 URL 或文件路径
- [x] Task 6: 测试与文档 (AC: 1-3)
  - [x] wiremock 端到端：sync 后 `api list` 含新端点且可 dispatch
  - [x] 损坏/截断/空/异物/目录占位缓存 → 全部退 0 回退内置
  - [x] 版本不匹配（布局版本 / IR schema / CLI 版本）→ 废弃且判定为 outdated
  - [x] `tests/no_phone_home.rs`：网络入口收敛（约束 reqwest crate 与裸 socket）+ 本地命令在出口全断时仍成功
  - [x] 旧 CLI 测试统一隔离 `OTL_CACHE_DIR`（`tests/common/mod.rs`）
  - [x] `docs/exit-codes.md` 登记新错误类与两个错误域的文案约束；README 更新；`project-context.md` 登记第 3 条 HTTP 例外

## Dev Notes

- **先读 `project-context.md`**。本 story 触碰的红线：engine 禁止 Outline 内容、运行时禁止解析 OpenAPI（唯一例外
  `spec sync`）、库层禁 unwrap、IR 版本化、缓存写入原子、不 phone home。
- **共用编译器为什么是新 crate**：build script 只能用 build-dependencies。把编译器放进 `engine` 会把
  reqwest/rustls 拖进 host 构建（三平台 CI 上很贵）；`spec-compile` 只依赖 serde_json + thiserror。代价是它的
  `BodyKind`/`ScalarKind` 是 `engine::ir` 的镜像枚举，由 `otl::spec::to_ir` 单点穷举映射 + parity 测试兜住。
- **为什么 `--spec` 挂在 `spec sync` 而不是全局 flag**：stack.md 要求「`--spec` 允许开发时覆盖」，但全局 flag 意味着
  运行时解析 OpenAPI 的第二条路径，与 NFR1 与 startup guard 的约束冲突。`spec sync --spec <path>` 同样满足开发覆盖
  语义（编译一次落缓存，之后所有命令生效），且解析仍只发生在 sync 这一条路径。`otl spec reset` 退出该状态。
- **缓存 key 里的 spec hash**：加载时无法用 hash 做查找（运行时手里没有 spec 可以算 hash），所以 hash 是 provenance +
  「是否需要重写」的判据，CLI 版本与 IR schema 版本才是准入校验。三者都在 header 里。
- **缓存是独立信任边界**：文件可能被截断、位翻转、换成管道或 symlink、被别的进程写、被上一版 CLI 留下。加载顺序为
  文件类型（不跟随 symlink，必须是普通文件）→ 文件尺寸 → 打开后 fstat 复查 → 限长读取 → magic → 布局版本 →
  校验和 → 有界解码（元素数 + 解码后 footprint，见下）且无尾随字节 → IR schema 版本 → CLI 版本 → provenance →
  每个 op 的 name/path/绑定/文本。任一失败即整体废弃，**不做迁移**。
- **字节上限不是内存上限**（R2 发现，R3 修正）：bincode 的 limit 计的是消耗字节，而最小 OpSpec 编码 6 字节、
  解码后上百字节。R2 我加了「元素数 + 逐元素 footprint」，并声称峰值约 26 MB——**这个推导 R3 被算术核对证伪**：
  `footprint_of` 按 `len × 24` 计费，漏掉了 Vec 按翻倍扩容的容量浪费。刚越过 serde cautious 上限（43,690 个
  `Cow<str>`，约 43 KiB 输入）的容器会一次涨到 87,380 槽 = 2 MiB，一个 1 MiB body 能塞进约 24 个这样的容器 →
  **约 47 MiB**，全部发生在预算被查之前。
  R3 的修法不是调数字而是**换表结构**：表改为逐记录分帧（`meta_len | meta | op_count | [op_len | op]*`），
  每条记录 ≤ 32 KiB 且单独解码。内层容器再怎么声明，也只有一条记录的字节可用；容量计费加了 2× slack。
  实测最大 vendored 记录 391 字节（documents.search），整表约 16.5 KB，余量 60 倍以上。
  现在的界：文件 1 MiB + 已接受 4 MiB + 正在解的记录 <2 MiB ≈ **8 MiB 以内**，且随即废弃。
- **路径校验是安全需求不是洁癖**：engine 以 `format!("{base}{path}")` 拼 URL。若 IR 里出现 `@evil.example/x`，
  `https://host` + 该 path = `https://host@evil.example/x`，host 变成 userinfo，Bearer token 直接送给攻击者。
  因此下载的 spec 与缓存文件两侧都强制「纯绝对路径」白名单（禁 `@ : ? # % //` 与 `..`）。
- **两条 HTTP 通道的取舍**（R1 审查后修正）：spec 下载不能带 token（第三方主机），但**「不带 token」不等于「可以没有退避」**——
  初版把 429 当普通 HTTP 错误一次性返回，绕过了唯一通道的 retry/throttle，这是审查判定的 BLOCKER。现在
  `engine::fetch::DocumentFetch` 复用 `RetryPolicy` 与 `Throttle`：429 按 Retry-After/带抖动退避重试、每次尝试都过节流、
  重试耗尽是独立错误（CLI 退出码 8）。同时错误域独立（`FetchError`），因为文档主机不是 Outline API：它的 401 不该提示
  用户检查 `OUTLINE_API_KEY`（fetch 从不发送它），它的连接失败不该提示检查 `OUTLINE_URL`（与失败地址无关）。
  该例外连同附加义务已登记进 `project-context.md`（第 3 条 HTTP 例外）。
- **不受信输入的三层校验**（R1 后加强）：
  1. **编译期**：路径/操作名白名单 + 递归深度 + 「有语义的文本」（content type、参数名、format、enum 值）控制字符与长度校验；
     纯展示文本（summary）改为**清洗**（丢控制字符 + 截断）而不是拒绝——`api list` 是行协议，一个 `\t` 就能伪造一列。
  2. **缓存加载**：同一套规则再跑一遍，外加 `path == "/api/" + name` 这个**语义绑定**断言。只校验字符安全是不够的：
     name=`documents.search` / path=`/api/documents.delete` 两个字段各自都合法，却能让无害命令带着 Bearer token 打到删除端点。
  3. **provenance**：`source` 与 `spec_hash` 也会被打印，同样校验（hex 摘要 + 可打印且限长）。
- **复杂度是攻击面**：宽 schema 的属性去重/required 标记原为线性扫描（O(n²)，实测 64k 属性约 4.4s CPU，输入只有几 MB），
  sync 变更报告的 diff 原为嵌套 contains（O(n×m)）。两处都改为 hash set。
- **启动预算**：实测 `otl --help` 3.64 ms（阈值 10 ms，release，hyperfine -N，794 runs）；缓存解析只在真正需要
  操作表时发生，实测 113 op / 16 KB 缓存约 +0.3 ms（`api list` 6.0 → 6.3 ms）。release 二进制 2.65 MB。
- **stale 也会警告**：CLI 升级后缓存必然失效。静默回退会让「昨天还能用的端点今天报 unknown operation」变成谜题，
  所以 stale 与 damaged 都出一行 stderr 警告并给出 `otl spec sync` / `otl spec reset`。
- **不在范围内**：doctor（4.3）、profile 体系（4.1）、OAuth（Epic 2）、overlay 文件（x-cli 扩展）落地。
  overlay 只在编译器留了 `CompileOptions` 这个扩展点，本 story 不实现。

### Project Structure Notes

```
crates/
  speccompile/            # 新：共用 OpenAPI -> IR 编译器（无 vendor 特定内容）
    src/{lib.rs, schema.rs, text.rs, document.rs}   # document.rs：有界解析
  engine/
    src/fetch.rs          # 新：明文文档通道（独立错误域 + 复用 retry/throttle）
  otl/
    build.rs              # 瘦身：只渲染静态表
    src/spec/{mod.rs, cache.rs, bounded.rs, openfile.rs}  # 生命周期 / 缓存 / 分帧 / 不挂死的 open
    src/ops.rs            # 表解析（缓存优先，内置兜底）
    src/commands/spec.rs  # 新：sync / reset
    tests/{spec_cache.rs, spec_sync_e2e.rs, spec_parity.rs, no_phone_home.rs, common/mod.rs}
```

### References

- [Source: specs/spec-outline-cli/SPEC.md#CAP-7、#Constraints（启动 <10ms、不 phone home、overlay 不 fork）]
- [Source: specs/spec-outline-cli/stack.md#spec 供给、#架构（spec sync 路径）]
- [Source: specs/spec-outline-cli/failure-modes.md #1 #8]
- [Source: planning/epics.md#Story 4.2、FR22、NFR1、NFR4]
- [Source: docs/exit-codes.md]
- [Source: project-context.md 全文]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Debug Log References

- bincode 3.0.0 在 crates.io 上是**空占位包**（无 feature、无依赖）；实际可用的 serde 集成是 2.0.1
  （`bincode::serde::{encode_to_vec, decode_from_slice}` + `config::standard().with_limit::<N>()`）。
  Story 1.1 的 Dev Notes 说「bincode 已是 3.x」，据实改用 2.0。
- `engine::base_url_origin` 拒绝带 query 的 URL（它是 base URL 校验器），不能用来取 spec URL 的 origin；
  为此在 `engine::fetch` 暴露 `document_origin`。
- `directories` 在 macOS 上不认 `XDG_CACHE_HOME`，所以测试覆盖走专用的 `OTL_CACHE_DIR`。

### Review R1 处置（codex gpt-5.6-sol，1 BLOCKER + 10 MAJOR + 5 MINOR，全部修复）

| # | 处置 |
|---|------|
| 1 BLOCKER 第二通道绕过 429/节流/错误映射 | `DocumentFetch` 复用 `RetryPolicy`+`Throttle`，429 退避重试、耗尽退 8；例外与附加义务写入 `project-context.md`。测试：retry / Retry-After 计时 / 精确请求数的耗尽 / pacing / CLI 退 8 |
| 2 fetch 错误进 Outline 错误映射 | 独立 `FetchError` + `errors::map_fetch_error`；撤回上一版加进 `EngineError` 的两个 variant（engine/src/error.rs 回到 develop 状态）。测试：401/403/404/500/418 与不可达地址均断言不出现 `OUTLINE_API_KEY`/`OUTLINE_URL` |
| 3 缓存可同源重映射 | `validate_ops` 断言 `path == "/api/" + name`，并加重名检查。测试：单测 + 缓存文件级 |
| 4 不受信文本进终端 | summary 清洗、有语义文本拒绝且不回显、长度上限；缓存加载与 provenance 同样校验。测试：编译器 5 例 + 缓存 2 例 + CLI e2e |
| 5 temp 名可预测 / 权限继承 | 改用 `tempfile`（随机名 + `create_new` + Unix 0600 + drop 清理）。测试：symlink 不被穿透、0666 旧文件不被继承、预放 temp 无影响 |
| 6 Windows rename 不替换 | `NamedTempFile::persist`（内部 `MOVEFILE_REPLACE_EXISTING`）。`a_store_replaces_an_existing_cache` 现在是 Windows 回归测试（CI 三平台矩阵会跑） |
| 7 尺寸上限不对称 | 一个上限派生出 body 上限；encode 后显式校验（bincode limit 只管解码），store 侧同时跑 load 侧规则。测试：超限拒绝且不落文件、近上限往返 |
| 8 属性 O(n²) | `HashSet` 去重与 required。测试：20k 属性（整套编译器测试 0.1s） |
| 9 diff O(n×m) | `HashSet` |
| 10 `$ref` 二次转义 | 改为按 RFC 6901 **反转义**后直接索引 schema map。测试：`A/B`、`A~B`、`A~1B`、`a/b/c` |
| 11 旧测试未隔离 cache | 新增 `tests/common/mod.rs`，api_list / api_params / api_e2e / paging_e2e / startup_guard / contract_smoke 全部指向一个**故意不存在**的 cache 目录 |
| 12 尾随字节被接受 | 校验 `consumed == body.len()` |
| 13 `.send()` 守卫太弱 | 守卫改为约束 **reqwest crate 本身**与裸 socket（`std::net`/`TcpStream`/`UdpSocket`），并加「manifest 不得出现第二个 HTTP/TLS 栈」与「allowlist 文件必须存在」两条断言 |
| 14 allowlist 粒度太粗 | allowlist 改为「文件 + 模式 + 该行必须含的上下文」，另加「runtime 不得出现 `read_to_string`（除用户指定路径的两处）」。已用报告里的原样 bypass 实验验证会被抓住 |
| 15 `--spec` 对 FIFO 阻塞 | 打开前先 `fs::metadata` 判类型，非普通文件即 usage error。测试：FIFO（带看门狗，回归时失败而非挂死）+ 目录 |
| 16 函数 >50 行 | `fetch_document` 拆成 `DocumentFetch` 的方法、`collect_facets` 抽出 `merge_facets`；脚本核对本 story 新增/改动的函数全部 <50 行（仓库里仅剩 Epic 1 的 `client::send`、`extract_error_parts`、`paginate::fetch_all_pages` 超限，不在本 story 范围） |

### Review R2 处置（8 VERIFIED / 8 PARTIAL + 2 MAJOR + 9 MINOR，全部修复）

R2 判定 VERIFIED 的 8 项（FetchError 独立错误域并核对 blob hash、文本净化/拒绝两侧、tempfile 原子写与 Windows
persist、HashSet 化无语义回归、consumed 检查、函数长度）未再改动。PARTIAL 与新发现的处置：

| # | 处置 |
|---|------|
| [2] MAJOR 缓存读取不拒特殊文件、读取无上限 | `symlink_metadata`（不跟随 symlink）→ 必须是普通文件 → 打开后再 fstat 复查 → `take` 限长读取。FIFO/`/dev/zero` symlink/指向合法缓存的 symlink/目录/读中增长全部拒绝并回退内置表。FIFO 测试带看门狗线程，回归时失败而非挂死 |
| [3] MAJOR 解码内存放大 | 新增 `BoundedOps`：自定义 seq visitor，先按 `size_hint` 拒绝不可能的元素数（不预分配），逐元素计数至 `MAX_CACHED_OPS`=8192，并逐元素累加**解码后 footprint** 与 8 MiB 预算比对。文件上限从 8 MiB 降到 1 MiB（约 7000 op，是 vendored 的 60 倍），因为它是所有放大界的乘数。同一 op 上限也进了 `validate_ops`，使「能编译」与「能缓存」不会打架。bincode 的 byte limit 改为 footprint 预算而非文件上限——它计的是「消耗字节 + 容器 claim」，绑到文件上限会拒绝本 build 刚写出的文件 |
| [1] MINOR 超 u64 的 Retry-After | 全数字但溢出的值视为「非常久」返回 `Duration::MAX`，由 `max_wait` 钳制；非 delta-seconds 仍走退避 |
| [4] MINOR 编译期缺 name/path 绑定 | 绑定断言移到编译器（对任意 prefix 成立）：无前导 `/` 的文档路径会被前缀吞掉成 `/apidocuments.delete`，现在直接拒绝文档 |
| [5] MINOR store 不查版本 | `store_at` 增加 load 侧版本检查，不再写出下一条命令就会废弃的缓存 |
| [6] MINOR RFC 6901 非法转义被接受 | 改为严格逐字符解码：非法 `~x`、尾随 `~`、空 token、裸 `/` 一律拒绝（`UnsupportedRef`） |
| [7] MINOR 守卫按整文件放行 | 改为**计数**每条通道恰好一个 `.send()`；断言两条通道互不调用（fetch.rs 不得用带凭证的 client）；固定 fetch.rs 的公开符号清单——在放行文件里新增入口现在必须显式过审 |
| [8] MINOR 第二 HTTP 栈只查根 manifest | 遍历全部 member manifest + `Cargo.lock`（覆盖传递依赖） |
| [9] MINOR 拆分构造路径绕过 | 禁止 runtime 出现任何打开文件的 API（`File::open`/`OpenOptions`/`fs::read`/`read_to_string`/`read_dir`/`include_*!`），只放行读用户指定路径的两处与缓存一处。路径怎么拼都得经过其中之一 |
| [10] MINOR 共享的「保证不存在」目录 | 目录名含 pid + 计数器 + 纳秒，返回前断言不存在 |
| [11] MINOR `--spec` 打开竞态 | 打开动作放到看门狗线程 + `recv_timeout`，「打开阻塞」变成明确错误而非永久挂起（`O_NONBLOCK` 需要 libc 且平台常量不同，为一次调用不值得）。看门狗本身有单测 |

### Review R3 处置（4 VERIFIED / 5 PARTIAL + 3 MAJOR + 6 MINOR，全部修复）

**最重要的一条：R2 我给的「峰值约 26 MB」被算术核对证伪，是我算错了。** `footprint_of` 按 `len × 24` 计费，
漏掉 Vec 翻倍扩容：刚越过 serde cautious 上限（43,690 个 `Cow<str>`，约 43 KiB 输入）的容器一次涨到 87,380 槽
= 2 MiB，一个 1 MiB body 能塞约 24 个 → **约 47 MiB**，全在预算被查之前。

| # | 处置 |
|---|------|
| [1] MAJOR 内层容器放大 ~47 MiB | 不是调数字而是**换结构**：表改为逐记录分帧（`meta_len \| meta \| op_count \| [op_len \| op]*`），每条记录 ≤ 32 KiB 且从自己的切片单独解码。内层容器再怎么声明也只有一条记录的字节可用，构造本身不存在了。容量计费加 2× slack。新界（写进代码注释）：文件 1 MiB + 已接受 4 MiB + 正在解的记录 <2 MiB ≈ **8 MiB 以内**。实测最大真实记录 391 字节（documents.search），余量 83 倍 |
| [2] MAJOR store/load 仍不对称 | store 现在跑 load 侧全部规则（含 decoded-footprint）；新增 store→load 组合测试（8 万短参数名那类形状） |
| [3] MAJOR 缓存加载 TOCTOU | 打开走看门狗（换成 FIFO 只会超时报错并回退，不会永久阻塞）；句柄按 **(dev, ino)** 与预检查结果比对，换成另一个普通文件也会被认出。两处 open 合并到 `spec/openfile.rs`——运行时唯一的 open 站点 |
| [4] MINOR 守卫可绕（send/公开面） | 通道文件内禁用 `.execute(`/`blocking::get(` 等其他发送 API（否则 `.send()` 计数不变）；公开面扫描加 `pub use`/`pub async fn`，并**双向**校验（改名不会让清单静默失效） |
| [5] MINOR hyper/rustls 无法区分直接依赖 | 锁文件允许（reqwest 带入），但**任何 member manifest 里直接声明**即失败；同时区分「依赖键」与「feature 字符串」（`"rustls"` 作为 reqwest 的 feature 是正确用法） |
| [6] MINOR 文件读取按整文件放行 | 改为**按调用站点**（文件 + 该行必须含的上下文），并把 `.open(` 也纳入模式（覆盖 `File::options()`/别名）。已用报告原样的绕过（拆分路径 + `fs::read` 塞进已登记文件）验证会被抓住 |
| [7] MINOR watchdog 线程泄漏 | 单测结尾打开 FIFO 写端放行工作线程，不再留阻塞线程；限制（超时后线程会阻塞到进程退出、真正的修法需要 `O_NONBLOCK` 与 libc）写进模块注释 |
| [8] MINOR 容量上界是死上界 | 分帧后按**实际剩余字节**判定：`count > remaining / MIN_FRAMED_OP_BYTES` 直接判为撒谎，短 body 声明 8192 个 operation 不再预留 |
| [9] MINOR 错误分类与措辞 | 三类资源错误各自独立（operation 数 / 单条记录大小 / 解码后内存），各带实际值 + 上限 + 对症的 remedy，单条超限还会**点名是哪个 operation**；`--spec` 的文档类错误按「谁选的文档」判退出码（本地 2 / 远程 1） |

守卫的注释也按要求改写：它们是**源码文本的回归守卫，不是证明**。字符串扫描不解析 Rust，绕过总是存在；它们保证的是
「本仓库实际使用的形态不能被悄悄新增」——新的 send 点、fetch 模块的新公开项、任何 manifest/lock 里的第二个 HTTP 栈、
新的文件打开站点，都会在这里失败，而修改它们本身就是这些测试代替的那次评审。

### Review R4 处置（R3 九条：VERIFIED 5 / PARTIAL 4 / 0 REGRESSED；新增 3 MAJOR + 6 MINOR，全部修复）

R4 换成能真跑门禁的审查者，结论有两条对我不利且都成立：**上界数字第二次被证伪**，以及**真正的内存问题在缓存解码的上一步**。

| # | 处置 |
|---|------|
| [2] MAJOR 16 MiB 文档解析放大到 367 MB | 根因是 `serde_json::from_str::<Value>` 无界。新增 `speccompile/src/document.rs`：**只物化编译器真正读的部分**（`paths` / `components`，其余键走 `IgnoredAny`，零分配），并给物化的部分**边解析边计费**（`MAX_PARSED_BYTES` = 24 MiB，超了当场停）。用审查者原样的复现实测：**367 MB → 29 MB**（vendored 对照 7.2 MB）。取消了 `compile(&Value)` 这个公开入口——它会绕过唯一在其它限额之前生效的限制 |
| [1] MAJOR 上界数字又错（8 MiB vs 实测 10 MiB） | **不再写数字**。`bounded.rs` 的散文上界换成「哪一条检查、对应哪个常量」的清单，真实峰值改为**可执行断言**：新增 `crates/otl/tests/memory_bounds.rs`，用 dhat（`unsafe` 在 dhat 内部，本仓库仍零 unsafe）实测六个场景并断言峰值。当前实测：忽略键文档 **0.00 MiB**、膨胀文档 8.00、vendored 2.25、8192 op 缓存 1.52、两层撒谎记录 1.46、正常缓存 0.04 |
| 同上，两处低估的根因 | 文件缓冲改为按 stat 大小预留（不再倍增到 3×）；表自身的 `Vec<OpSpec>` **在预留前先计费**（原先完全不计），`footprint_of` 相应只算内容 |
| [3] MAJOR provenance 撒谎 | `get_text` 改返回 `FetchedDocument { text, origin }`，origin 取**应答方**（`response.url()` 经同一套 URL 规则复检）而非被问方；`CacheMeta.source` 与 sync 报告都用它。双 server 测试（engine 与 e2e 各一）断言记录的是应答主机。错误消息仍用**被问的** origin——那是用户敲的、能据以行动的那个 |
| [5] MINOR `OperationTooLarge` 解码侧是死变体 | 解码侧新增 `take_op_record`：超长记录报「operation #N，超出每条上限，参数/枚举太多」而不是泛化 framing 错误；测试断言序号、上限与对症 remedy |
| [6] MINOR `--spec` 没跟上 (dev,ino) | `open_regular` 复检改为 `is_same_file`（与缓存路径同一函数），文案同步。**测试局限如实说明**：竞态本身无法稳定构造，`is_same_file` 有单测，`--spec` 侧靠共用同一函数 |
| [7] MINOR `[dependencies.X]` table 写法绕过 | 检测抽成 `is_dependency_declaration`，同时认 inline 与 table（含 `[target."cfg(unix)".dependencies.hyper]`），并**给检测器本身加了自测**（6 个应命中 + 6 个不应命中）。已实测：植入 `[dependencies.hyper]` 后守卫失败 |
| [8] MINOR `record_config` 死参数 | 拆成 `op_record_config` / `meta_record_config`，各自的 bincode 限额为记录上限的 4 倍——**这个倍数有实证理由**：一条 32,008 字节的合法记录在 32 KiB 限额下会被拒（限额计的是「消耗字节 + 容器 claim」） |
| [9] MINOR File List 漏登记 | 补 `engine/src/retry.rs`、`engine/tests/rate_limit.rs`，并补齐本轮新增文件 |
| 守卫摘要过度声称 | 摘要改为继承被摘要测试的限制：只保证**具名**的 HTTP/TLS crate 会被抓，而不是「任何 HTTP 依赖」 |

### Review R5 处置（R3 四条 PARTIAL 全部 VERIFIED；新增 2 MAJOR + 4 MINOR，全部修复）

R5 确认了 R4 的核心修法（367 MB → 实测 29.2 MB、IgnoredAny 零分配经源码+实测双重确认、provenance 多跳正确、
无绕过入口），并判定可合并。但它指出**「可执行断言」这块招牌自己没兑现**——这条批评成立且尖锐：

| # | 处置 |
|---|------|
| [1] MAJOR fixture 与注释不一致、最坏形状未测 | 注释说「两层容器都撒谎」，代码却让外层诚实声明 4000 → 只测到 1.46 MiB。现在**外层也撒谎**，实测 **2.03 MiB**（与审查者独立探针一致）；并新增**组合场景**「8192-op 表 + 首条最坏记录」实测 **3.19 MiB**（同样吻合）——这是加载器真正的最坏情形，此前没有任何测试构造过它。阈值按实测重设（4 MiB / 6 MiB） |
| 同上：`bounded.rs` 那句错的散文 | 「一条记录不会放大超过约 1 MB」删掉，换成**机制**：bincode 的 serde 桥只按消耗字节计限额、从不 `claim_container_read`，而 serde 的 `Vec` 独立按自己的 1 MiB cap 预留——**每层嵌套各一次**，`OpSpec` 有两层。分帧能把它压成小常数，但不能消掉（那需要接管每个嵌套类型的解码）。数字一律指向 memory_bounds 测试 |
| [2] MAJOR 测量窗口不含输入缓冲 | 文档改为**在 profiler 窗口内从文件读**。实测因此从 0.00/8.00/2.25 变为 **16.00/24.00/2.60 MiB**——之前少报的正是最大的一项。头注改为明确说明窗口含什么（输入缓冲 + 解析/加载）、不含什么（进程基线约 13 MB），并附命令级 RSS 实测：vendored **6.9 MB**、ignored-key **29.2 MB**、预算接受的最坏形状 **46.1 MB**。另新增「预算会接受的最坏文档」（16 MiB 长字符串）场景，实测 32.08 MiB |
| [3] MINOR 重校验失败时记错主机 | 先剥 userinfo 再校验（带凭证的 Location 现在也能正确点名应答主机，凭证不入记录也从不发送）；真正无法确定时记**空**（调用方渲染为 unknown），**绝不回退到没有应答过的主机**。新增双 server + 带凭证 Location 的测试 |
| [4] MINOR 依赖检测器仍漏两种写法 | 补上 quoted key（`"hyper" = "1"`、`"hyper".workspace = true`）与**改名依赖**（`h = { package = "hyper" }`——键名什么都不说，`package` 才是真的）。自测扩到 13 命中 + 8 不误报，并逐个在真实 manifest 里植入验证 |
| [5] MINOR 过大文档被报成「JSON 不合法」 | 新增独立错误变体：给出输入大小、上限、停下前已计费的量，全部「可读 + 精确」双写（`24.0 MiB (25165824 bytes)`），与缓存侧三条限额的措辞标准一致 |
| [6] MINOR serde `default-features` 被忽略 | 删掉该无效项（每次构建都告警），注释改为与 workspace 实际 feature 集一致 |

审查者「已查无发现」的结论保持不变（路径跨源防线、下载体积、bincode 分配、OnceLock 不影响 `--help`、mirror/parity 守法、thiserror 用法、运行时无第二条 OpenAPI 解析路径），相关代码未做无谓改动。

### 合并集成注意（develop 已前移，orchestrator 请看这一段）

复核门禁时发现 `develop` 已合入 Epic 4a 的 schema 驱动列（`IR_SCHEMA_VERSION` 4 → 5，新增
`engine::ir::FieldSpec` 与 `OpSpec::response_fields`）。本分支相对**合并基**仍未碰过 `ir.rs`/`error.rs`
（`git diff develop...HEAD -- crates/engine/src/{ir,error}.rs` 为 0 行），所以之前的声明依然成立；
但合并时需要动的地方是确定的，列在这里免得漏：

1. `otl::spec::to_ir` 用的是 `OpSpec { .. }` 全字段字面量，**会编译失败直到补上 `response_fields`**。
   这是当初特意选结构体字面量而非 `..Default::default()` 的目的：新字段必须被显式处理，不能静默取默认值。
   最省事且正确的落法是让 `spec-compile` 也编译 response 字段（4a 的编译器侧已有实现，可复用），
   实在赶时间可以先填空 `Cow::Borrowed(&[])`——但那会让「synced 缓存的表格列」退化，属功能缺口而非安全问题。
2. **`bounded.rs::footprint_of` 必须给 `response_fields` 计费**（连同其内层字符串），否则新增的容器就是一条
   没被预算看见的放大路径。
3. `bounded.rs` 模块注释里「`OpSpec` 嵌套两层」要改成三层：serde 的 per-container 预留是**每层一次**，
   最坏单记录从 ~2 MiB 变成 ~3 MiB，组合场景从 3.19 MiB 变成约 4.2 MiB。
4. 因此 `tests/memory_bounds.rs` 的两条阈值（4 MiB / 6 MiB）要按合并后实测重设，
   并把「三层都撒谎」加进那条 fixture——**这一步别省**，R5 的教训正是 fixture 落后于代码。
5. `IR_SCHEMA_VERSION` 变 5 会让所有旧缓存判为 stale 并回退内置表（设计如此，有测试覆盖）。
   `build.rs` 里的 `IR_SCHEMA_VERSION` 常量副本也要同步（否则生成代码里的 `const _: () = assert!(...)` 会失败——
   这是它存在的意义）。
6. 双方都改过的共享文件（`main.rs`/`lib.rs`/`commands/mod.rs`/`build.rs`/`api.rs`/`startup_guard.rs`/
   `ir_table.rs`/`docs/exit-codes.md`/两个 `Cargo.toml`）都是追加式改动，冲突应为机械冲突。
   注意 `crates/otl/tests/{api_list,api_params,api_e2e,paging_e2e,contract_smoke,startup_guard}.rs`
   两侧都碰了：我加的是 `OTL_CACHE_DIR` 隔离（`mod common;`），合并后**务必保留**，否则 4a 的断言会重新
   依赖开发机上的真实缓存。

### Completion Notes List

- 偏差 1：`--spec` 实现为 `spec sync --spec <path>` 而非全局 flag（理由见 Dev Notes）。
- 偏差 2：额外加了 `otl spec reset`。没有它，用户 sync 到一份坏 spec 后无法自助恢复（需要知道缓存路径去手删）。
- 偏差 3：为缓存错误在 otl 引入 thiserror（anyhow 仍是边界类型）。"stale vs damaged" 需要可判定，字符串不够。
  （R1 审查确认该读法与现有文字规则不冲突。）
- 偏差 4（R1 后）：`tempfile` 从 dev-dependency 提为 otl 的正式依赖。原子写要的是「随机名 + O_EXCL + Unix 0600 +
  Windows 替换语义 + 失败自清理」，手写这四件事只会写出更差的版本。
- `crates/engine/src/ir.rs` **未改动一行**（IR 已 derive Serialize/Deserialize 且已有 `IR_SCHEMA_VERSION`，
  `Cow<'static, _>` 反序列化为 Owned 即可）。这是为了不与 Epic 4a 的 schema 驱动列改动冲突。
- `build.rs` 大幅瘦身（432 → 165 行）：解析逻辑整体搬到 `spec-compile`，只留渲染与两个变体名映射函数。
- 共享文件只做追加式改动：`main.rs`（+1 变体 +1 dispatch 行）、`lib.rs`（+1 mod）、`commands/mod.rs`（+1 mod）、
  `errors.rs`（+2 classify 分支）、`engine/src/lib.rs`（+1 mod +文档）。`exit.rs` 未改（无新退出码）。
- `tests/ir_table.rs` 改为显式查内置表（`builtin()` 而非 `ops::find`），否则开发机上一份真实缓存会让它对
  vendored spec 的断言失真。
- `tests/startup_guard.rs`：二进制内容检查从「文件名」改为「文件路径」（上游 URL 恰好以同名文件结尾），
  并加了两条 allowlist（上游 URL 常量、`--spec` flag 名）。守卫语义未削弱：运行时仍不得定位 vendored spec。
- 质量门（R1 修复后重跑）：`cargo fmt --all -- --check` / `cargo clippy --all-targets --all-features -D warnings` /
  `cargo test --workspace` / `scripts/bench-startup.sh` 全绿，数字见汇报。
- 架构红线复查：`grep -ri outline crates/engine crates/speccompile` 零命中。

### File List

- Cargo.toml, Cargo.lock
- crates/speccompile/Cargo.toml, crates/speccompile/src/{lib.rs, schema.rs, text.rs, document.rs}
- crates/engine/Cargo.toml（无改动）, crates/engine/src/{lib.rs, fetch.rs, retry.rs}, 
  crates/engine/tests/{fetch.rs, rate_limit.rs}
  （crates/engine/src/error.rs 与 ir.rs 相对 develop **零 diff**，已由 R2/R4 审查者独立核对 blob hash）
- crates/otl/Cargo.toml, crates/otl/build.rs
- crates/otl/src/{lib.rs, ops.rs, errors.rs, main.rs}
- crates/otl/src/spec/{mod.rs, cache.rs, bounded.rs, openfile.rs}
- crates/otl/src/commands/{mod.rs, api.rs, spec.rs}
- crates/otl/tests/{spec_cache.rs, spec_sync_e2e.rs, spec_parity.rs, no_phone_home.rs, ir_table.rs, startup_guard.rs,
  common/mod.rs, memory_bounds.rs}；cache 隔离补丁另涉 {api_list.rs, api_params.rs, api_e2e.rs,
  paging_e2e.rs, contract_smoke.rs}
- docs/exit-codes.md, README.md, project-context.md（第 3 条 HTTP 例外 + 两条 Windows/终端注入的反模式）
- stories/4-2-spec-sync.md
