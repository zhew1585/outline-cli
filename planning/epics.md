---
stepsCompleted: [1, 2, 3]
inputDocuments:
  - specs/spec-outline-cli/SPEC.md
  - specs/spec-outline-cli/stack.md
  - specs/spec-outline-cli/failure-modes.md
---

# outline-cli (otl) - Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for outline-cli (otl), decomposing the requirements from SPEC.md and its companions (stack.md, failure-modes.md) into implementable stories.

## Requirements Inventory

### Functional Requirements

FR1: 用户可用 API key（Bearer token）认证，`OUTLINE_API_KEY` 环境变量兜底，`otl auth info` 返回正确身份。
FR2: `otl auth login` 完成 OAuth 2.0 授权码 + PKCE S256 + 本地回环回调流程，服务器元数据自 `/.well-known/oauth-authorization-server` 发现，默认申请 scope `read write`。
FR3: OAuth access token 过期后请求自动用 refresh token 续期，用户无感知；refresh token 轮换后新凭证原子持久化，并发刷新有防护。
FR4: 所有凭证（API key、OAuth tokens、registration_access_token）存于用户配置目录下的独立凭证文件（`credentials.toml`，明文 + Unix 0600），与 `config.toml` 分离；不使用系统钥匙串。
FR5: 未配置 client_id 时自动尝试 DCR（RFC 7591）注册公共客户端并缓存注册结果；DCR 不可用时回退提示管理员在 Settings → Applications 预注册。
FR6: `otl auth logout --purge` 凭 registration_access_token 走 RFC 7592 删除 DCR 注册，避免服务器孤儿客户端。
FR7: `otl docs search` 搜索文档，输出人类可读结果。
FR8: `otl docs view` 查看文档，长内容自动进 $PAGER，v1 直接输出原始 markdown。
FR9: `otl docs create` 创建文档，支持 stdin 管道输入内容（`cat notes.md | otl docs create --title X`）。
FR10: `otl docs update` 更新文档。
FR11: `otl collections list` 列出集合。
FR12: `otl docs export` 批量导出（建立在自动分页引擎上）。
FR13: vendored spec 中任意操作可经 `otl api <op> k=v...` 调用，标量参数按 schema 类型转换。
FR14: 复杂嵌套参数经 `--body @file.json` 直通。
FR15: `otl api list` 列出全部可用操作；spec 更新后新增端点无需改代码自动出现。
FR16: 发送前本地 schema 校验 fail fast，参数错误在发起网络请求前报出。
FR17: 429 限流按 Retry-After 退避重试，client 内全局令牌桶节流。
FR18: Outline 错误信封映射为人类可读消息与文档化的稳定退出码。
FR19: 默认输出人类可读（表格/markdown），`--json` 供脚本消费（可被 jq 处理）。
FR20: 输出目标为非 TTY 时自动关闭分页与装饰。
FR21: 列表类操作自动翻页，`--limit` 控制总量，截断时显式警告。
FR22: `otl spec sync` 拉取上游最新 spec 并立即生效（运行时解析一次落 bincode 缓存）。
FR23: `otl doctor` 对比线上 API 与本地 spec 差异并报告（缺失/已弃用操作）。
FR24: 配置体系：env 变量 + 单一用户配置文件（TOML，含命名 profile 支持多 workspace/自托管 base URL）；优先级 flag > env > 用户配置。
FR25: shell 补全由 IR 生成（clap_complete），覆盖子命令与 flag 名。

### NonFunctional Requirements

NFR1: 启动时间 <10ms；运行时零 OpenAPI/YAML 解析（仅 spec sync 路径解析一次）。
NFR2: 单静态二进制，目标体积约 5MB。
NFR3: 三平台（macOS/Linux/Windows）一等公民，CI 矩阵覆盖。
NFR4: 不 phone home：无自动更新检查、无自动 spec 检查、无遥测。
NFR5: 精选命令接口走 semver 稳定契约；`otl api` 裸调模式明示不保证稳定。
NFR6: 凭证安全：独立凭证文件、创建即 0600、读取前权限校验（过宽拒用）、原子写入、env 明文使用时提示风险。
NFR7: 测试体系：wiremock + spec 生成 fixture 的单测、输出渲染 golden file 测试、CI 对真实 workspace 的契约测试。

### Additional Requirements

- 两 crate 分层：通用 OpenAPI RPC 引擎 crate + Outline UX 层 crate（引擎未来可独立库化）。
- build.rs 编译期将 vendored spec 编译为精简 IR 静态表（每操作仅 path + 参数 schema + auth），非每端点生成函数。
- 运行时单一通用 `execute(op, args)` 分发器；clap 命令树运行时由 IR 构建。
- vendor 上游 github.com/outline/openapi，`include_str!` 打包；`--spec` 开发时覆盖；定制走 overlay（x-cli 扩展），不 fork。
- bincode 缓存于 `~/.cache/outline-cli`，key 为 spec hash + CLI 版本；原子 rename 写入，损坏自愈重解析。
- 依赖基线：clap + serde + anyhow + reqwest（rustls）+ directories + toml + clap_complete（不含 keyring）。
- 两步协议（附件上传等）手写特例命令，预算约 5 个（v1 至少覆盖附件上传路径的设计口子，不强制实现）。
- 分发：cargo-dist（brew tap + shell 安装器 + MSI）；命名 crate `outline-cli`、二进制 `otl`。
- MVP 顺序约束：① documents.* 子集跑通 build.rs IR 管线 + 分发器 → ② 全端点 + api 逃生舱 → ③ 六精选命令抛光 + OAuth。
- 风险缓解已固化（failure-modes.md 9 条），其中直接影响故事验收的：oneOf/anyOf 回退 --body、分页无静默截断、DCR 客户端不可界面删除。
- 非目标（不得混入故事）：docs pull/push 同步、TUI、watch、MCP server、device flow、离线写队列、全量 codegen。

### UX Design Requirements

（无独立 UX 文档。CLI 的交互契约已内嵌于 FR：双态输出 FR19/FR20、pager FR8、退出码 FR18、补全 FR25。）

### FR Coverage Map

FR1: Epic 1 - API key 认证与 auth info
FR2: Epic 2 - OAuth 授权码 + PKCE 登录
FR3: Epic 2 - token 自动续期与轮换持久化
FR4: Epic 2 - 凭证入本地凭证文件（0600）
FR5: Epic 2 - DCR 自注册优先与回退
FR6: Epic 2 - logout --purge 自删注册
FR7: Epic 3 - docs search
FR8: Epic 3 - docs view + pager
FR9: Epic 3 - docs create + stdin
FR10: Epic 3 - docs update
FR11: Epic 3 - collections list
FR12: Epic 3 - docs export 批量导出
FR13: Epic 1 - api 裸调 + schema 类型转换
FR14: Epic 1 - --body 复杂参数直通
FR15: Epic 1 - api list 操作枚举
FR16: Epic 1 - 本地 schema 预校验
FR17: Epic 1 - 429 退避与节流
FR18: Epic 1 - 错误映射与稳定退出码
FR19: Epic 1 - 双态输出（人类可读/--json）
FR20: Epic 1 - 非 TTY 自动降级
FR21: Epic 1 - 自动分页与截断警告
FR22: Epic 4 - spec sync
FR23: Epic 4 - doctor 差异报告
FR24: Epic 4 - 配置体系与命名 profile
FR25: Epic 4 - shell 补全生成

## Epic List

### Epic 1: 用 API key 调通任意 Outline API（引擎立身之本）
用户拿着 API key 就能用 `otl api <op> k=v` 调用 spec 里的任意端点，输出可读、脚本可用、限流不炸。
build.rs IR 管线、通用分发器、可靠请求通道、双态输出在此就位；结束时产品已"能用"。
**FRs covered:** FR1, FR13, FR14, FR15, FR16, FR17, FR18, FR19, FR20, FR21
（内含 NFR1 启动预算、NFR3 CI 矩阵、NFR7 测试骨架的落地）

### Epic 2: OAuth 登录与凭证安全
用户 `otl auth login` 浏览器授权即用，token 自动续期无感知，凭证进本地凭证文件（0600）；DCR 自注册优先、可回退、可自删。
**FRs covered:** FR2, FR3, FR4, FR5, FR6

#### Story 2.6: 凭证文件卫生

As a 把凭证明文放在磁盘上的用户,
I want CLI 严格管好这个文件的权限与写入,
So that 明文存储的风险被压到只剩"磁盘被物理读取"这一层。

**Acceptance Criteria:**

**Given** 全新环境首次写入凭证（login 或 set-key）
**When** 凭证文件被创建
**Then** Unix 上文件权限为 0600 且是创建时即设定（禁止先创建再 chmod 的竞态窗口），父目录不存在时一并创建且权限为 0700

**Given** 凭证文件权限被改宽（如 0644 或组可读）
**When** 执行任意需要凭证的命令
**Then** 拒绝使用该文件并报可读错误，含具体修复命令（如 `chmod 600 <path>`），退出码符合退出码表；不静默降级也不自动改权限

**Given** 写入过程中进程被杀或磁盘写失败
**When** 检查凭证文件
**Then** 文件内容或为旧值或为新值，绝不为截断/半写状态（同目录 temp → fsync → rename，temp 同为 0600）

**Given** 多个 otl 进程并发触发 token 刷新
**When** 刷新完成
**Then** 锁文件建议锁保证只有一个进程执行刷新，其余进程读到刷新后的有效凭证，refresh_token 不因竞争而失效

**Given** Windows 平台（无 POSIX 权限位）
**When** 执行 `otl auth info` 或 `otl doctor`
**Then** 明示该平台的凭证保护依赖用户 profile 目录 ACL，不谎报已设权限

**Given** 执行 `otl doctor`
**When** 报告凭证健康
**Then** 输出凭证文件路径、存在性、权限是否合规、各 profile 有哪些凭证类型，但绝不打印任何凭证值或其片段

## Epic 3: 六个日用精选命令
终端日常工作流成立：搜索、查看（pager）、创建（stdin）、更新、集合列表、批量导出。
**FRs covered:** FR7, FR8, FR9, FR10, FR11, FR12

### Epic 4: 多工作区、spec 生命周期与分发
多 workspace profile 配置、spec sync / doctor、shell 补全、cargo-dist 发布渠道（brew/installer/MSI）。
**FRs covered:** FR22, FR23, FR24, FR25
（内含 NFR2 体积、NFR4 不 phone home 验证、NFR5 semver 契约声明）

**依赖关系：** Epic 1 完全独立（env API key 即可跑）；Epic 2/3 建立在 1 之上但各自独立交付；Epic 4 独立。无前向依赖。

## Epic 1: 用 API key 调通任意 Outline API（引擎立身之本）

用户拿着 API key 就能用 `otl api <op> k=v` 调用 spec 里的任意端点，输出可读、脚本可用、限流不炸。
build.rs IR 管线、通用分发器、可靠请求通道、双态输出在此就位。

### Story 1.1: 首次端到端调用

As a 使用 API key 的开发者,
I want 在全新环境用 `otl api documents.info id=<id>` 调通我的 Outline 实例,
So that 引擎最小闭环（IR 编译 + 分发器 + 认证 + 请求）被真实验证。

**Acceptance Criteria:**

**Given** Cargo workspace 含 engine 与 otl 两 crate，vendored spec 已入库
**When** 执行 `cargo build`
**Then** build.rs 将 spec 中 documents.* 子集编译为 IR 静态表并嵌入二进制，构建成功

**Given** 已设置 `OUTLINE_API_KEY` 与 base URL（env）
**When** 执行 `otl api documents.info id=<真实文档id>`
**Then** 以 POST JSON 携带 Bearer 头发起请求，stdout 输出响应 data 部分

**Given** 未设置 `OUTLINE_API_KEY`
**When** 执行任意 api 命令
**Then** 发起网络请求前报出可读错误并以文档化非零退出码退出

### Story 1.2: 全端点 IR 与操作枚举

As a 需要长尾端点的用户,
I want `otl api list` 列出 spec 内全部可调用操作,
So that 不查文档也知道 CLI 能做什么。

**Acceptance Criteria:**

**Given** vendored spec 全量编入 IR
**When** 执行 `otl api list`
**Then** 输出全部操作名与摘要，数量与 spec 中 operation 数一致

**Given** spec 中任意操作名
**When** 执行 `otl api <op>` 且参数合法
**Then** 请求发出并返回响应，无任何端点需要手写代码

### Story 1.3: 参数校验与类型转换

As a 脚本作者,
I want k=v 参数按 schema 自动转型并在本地校验,
So that 参数错误即时暴露而不是变成服务端 400。

**Acceptance Criteria:**

**Given** schema 声明某参数为 integer/boolean
**When** 传入 `limit=5 template=true`
**Then** JSON body 中为对应原生类型而非字符串

**Given** 缺失必填参数
**When** 执行该操作
**Then** 不发网络请求，报错指明缺失参数名与类型

**Given** 操作含 oneOf/anyOf 复杂参数
**When** 以 k=v 传入该参数
**Then** 报错并提示改用 `--body @file.json`

**Given** `--body @file.json`
**When** 执行操作
**Then** 文件内容直通为请求体，跳过 flag 组装

### Story 1.4: 错误映射与稳定退出码

As a CI 脚本维护者,
I want 每类失败有固定退出码与人类可读消息,
So that 脚本能可靠分支处理。

**Acceptance Criteria:**

**Given** 服务端返回 Outline 错误信封（400/401/403/404）
**When** 命令失败
**Then** stderr 输出映射后的可读消息（含服务端 message），退出码符合文档化的退出码表

**Given** 网络不可达或超时
**When** 命令失败
**Then** 退出码区别于 API 错误，消息含重试建议

### Story 1.5: 双态输出

As a 同时交互使用与写脚本的用户,
I want 默认可读输出与 `--json` 机器输出,
So that 一套命令两种场景都好用。

**Acceptance Criteria:**

**Given** stdout 为 TTY
**When** 执行列表类操作
**Then** 输出 schema 驱动的表格（关键列自动挑选），无需每端点手写渲染代码

**Given** `--json` 或 stdout 非 TTY
**When** 执行同一操作
**Then** 输出原始 JSON 可被 jq 消费，无颜色与装饰

### Story 1.6: 自动分页

As a 批量处理数据的用户,
I want 列表操作自动翻页,
So that 不会默默只拿到前 25 条。

**Acceptance Criteria:**

**Given** 结果超过单页（offset/limit 信封）
**When** 执行列表操作
**Then** 自动翻页拿全结果

**Given** `--limit N`
**When** 结果被截断
**Then** stderr 显式警告已截断及获取更多的方法

### Story 1.7: 限流退避

As a 批量脚本作者,
I want 429 自动退避重试,
So that 脚本在限流下不随机失败。

**Acceptance Criteria:**

**Given** wiremock 返回 429 带 Retry-After
**When** 请求触发限流
**Then** 按 Retry-After 等待后重试成功，全局令牌桶限制并发请求速率

**Given** 重试次数耗尽
**When** 仍被限流
**Then** 报可读错误与专属退出码

### Story 1.8: 性能与 CI 基线

As a 日常高频使用者,
I want 启动即时且质量由 CI 守护,
So that 工具值得信赖。

**Acceptance Criteria:**

**Given** release 构建
**When** hyperfine 测量 `otl --help`
**Then** 冷启动 <10ms，运行时无 OpenAPI/YAML 解析

**Given** push 到主分支
**When** CI 运行
**Then** macOS/Linux/Windows 三平台矩阵全绿，含 wiremock 单测与输出 golden file 测试

## Epic 2: OAuth 登录与凭证安全

用户 `otl auth login` 浏览器授权即用，token 自动续期无感知，凭证进本地凭证文件（0600）；DCR 自注册优先、可回退、可自删。

### Story 2.1: OAuth 浏览器登录（预注册路径）

As a 不想手工管理 API key 的用户,
I want `otl auth login` 拉起浏览器完成授权,
So that 用工作区身份安全登录。

**Acceptance Criteria:**

**Given** 配置了 client_id（管理员预注册）
**When** 执行 `otl auth login`
**Then** 自 `/.well-known/oauth-authorization-server` 发现端点，从固定端口清单依次绑定回环端口，浏览器打开授权页（PKCE S256，scope `read write`，随机 state）

**Given** 用户在浏览器完成授权
**When** 回调命中本地服务器
**Then** state 严格校验，授权码换取 tokens，access/refresh token 原子写入凭证文件（创建即 0600），终端提示登录成功身份

**Given** 已登录
**When** 执行 `otl auth info`
**Then** 显示当前用户、workspace、认证方式与 scope

### Story 2.2: DCR 自注册优先

As a 自托管 workspace 的用户,
I want 未配置 client_id 时 CLI 自动注册自己,
So that 无需找管理员即可 OAuth 登录。

**Acceptance Criteria:**

**Given** 未配置 client_id 且服务器广播 registration_endpoint
**When** 执行 `otl auth login`
**Then** 先绑定随机回环端口，再以实际端口的精确 redirect_uri 走 RFC 7591 注册公共客户端，注册结果连同 registration_access_token 持久化

**Given** 已有缓存的注册
**When** 再次 login
**Then** 复用缓存 client_id，不重复注册

**Given** DCR 不可用（workspace 未开 MCP，端点 404）
**When** 执行 login
**Then** 回退输出清晰指引：请管理员在 Settings → Applications 注册并提供 client_id

### Story 2.3: 自动续期与轮换安全

As a 长期使用的用户,
I want token 过期自动续期,
So that 永远不需要手动重新登录。

**Acceptance Criteria:**

**Given** access token 已过期
**When** 任意命令发起请求
**Then** 请求通道自动用 refresh token 换新（单飞：锁文件建议锁保证并发进程只刷新一次），新 access/refresh token 原子写入凭证文件后重放原请求

**Given** refresh token 已失效或被撤销
**When** 刷新失败
**Then** 提示执行 `otl auth login` 重新登录，退出码符合退出码表

### Story 2.4: logout 与 --purge

As a 注重凭证卫生的用户,
I want 登出时彻底清理,
So that 服务器与本地都不残留。

**Acceptance Criteria:**

**Given** 已 OAuth 登录
**When** 执行 `otl auth logout`
**Then** 调用 revocation_endpoint 撤销 tokens，从凭证文件移除该 profile 的凭证条目（文件无剩余凭证时删除文件本身）

**Given** 客户端来自 DCR 注册
**When** 执行 `otl auth logout --purge`
**Then** 凭 registration_access_token 走 RFC 7592 删除服务器上的注册，本地注册缓存一并清除

### Story 2.5: API key 管理

As a 在 CI 与本机混用的用户,
I want API key 也能安全存管,
So that 两种认证方式都有一等体验。

**Acceptance Criteria:**

**Given** 执行 `otl auth set-key`
**When** 输入 API key
**Then** 原子写入凭证文件（创建即 0600），`otl auth info` 显示 API key 认证身份与凭证文件路径

**Given** 仅设置了 `OUTLINE_API_KEY` env
**When** 首次使用
**Then** 正常工作并提示一次：env 明文会经进程环境与 shell 历史泄漏，建议改用 `otl auth set-key` 存入凭证文件

**Given** 同时存在 OAuth 登录、凭证文件 API key、env API key
**When** 发起请求
**Then** 按 OAuth > 凭证文件 API key > env 优先级选用

## Epic 3: 六个日用精选命令

终端日常工作流成立：搜索、查看、创建、更新、集合列表、批量导出。

### Story 3.1: docs search

As a 终端工作者,
I want `otl docs search <query>` 快速搜到文档,
So that 不用切浏览器。

**Acceptance Criteria:**

**Given** 已认证
**When** 执行 `otl docs search 部署`
**Then** 可读输出结果列表：标题、所属 collection、更新时间、匹配上下文片段

**Given** `--json`
**When** 同一搜索
**Then** 原始 JSON 输出含文档 id 供脚本消费

### Story 3.2: docs view

As a 阅读文档的用户,
I want `otl docs view <id>` 直接读内容,
So that 终端内完成阅读。

**Acceptance Criteria:**

**Given** stdout 为 TTY 且内容超一屏
**When** 执行 view
**Then** 原始 markdown 自动进入 $PAGER

**Given** `--raw` 或输出为管道
**When** 执行 view
**Then** 纯内容直出无分页

**Given** `--web`
**When** 执行 view
**Then** 默认浏览器打开该文档 URL

### Story 3.3: docs create

As a 记录者,
I want 管道或文件直接建文档,
So that 笔记一条命令入库。

**Acceptance Criteria:**

**Given** `cat notes.md | otl docs create --title "Notes" --collection <id>`
**When** 执行
**Then** stdin 作为内容创建文档，输出新文档 id 与 URL

**Given** `--file notes.md`
**When** 执行
**Then** 文件内容作为文档内容，效果等同 stdin

### Story 3.4: docs update

As a 维护者,
I want 命令行更新文档,
So that 修订不进网页编辑器。

**Acceptance Criteria:**

**Given** 已有文档 id
**When** `otl docs update <id> --title 新标题` 或经 stdin 提供新内容
**Then** 更新成功并输出更新后的元信息

### Story 3.5: collections list

As a 用户,
I want 列出全部 collection,
So that 拿到 id 供其他命令使用。

**Acceptance Criteria:**

**Given** 已认证
**When** 执行 `otl collections list`
**Then** 表格输出名称/id/文档数，自动分页拿全

### Story 3.6: docs export

As a 备份与迁移者,
I want 整个 collection 批量导出为本地 markdown,
So that 内容可进 git 或离线阅读。

**Acceptance Criteria:**

**Given** `otl docs export --collection <id> --out ./docs-backup`
**When** 执行
**Then** 该 collection 全部文档经自动分页完整导出为 .md 文件，文件名安全化处理，目录结构反映文档层级

**Given** 导出中途某文档失败
**When** 继续执行
**Then** 失败文档汇总在结尾报告，退出码反映部分失败

## Epic 4: 多工作区、spec 生命周期与分发

多 workspace profile、spec sync / doctor、shell 补全、cargo-dist 发布，从"能用"到"可分发的产品"。

### Story 4.1: 配置与 profile

As a 拥有多个 workspace 的用户,
I want 命名 profile 切换实例,
So that 工作/个人/自托管随手切。

**Acceptance Criteria:**

**Given** 用户配置文件（TOML）定义多个 profile（base URL、认证方式）
**When** `--profile work` 或 `OUTLINE_PROFILE=work`
**Then** 请求指向该 profile 的实例与凭证

**Given** flag、env、配置文件同时设置同一项
**When** 解析配置
**Then** 优先级 flag > env > 配置文件生效

### Story 4.2: spec sync

As a 想用最新端点的用户,
I want `otl spec sync` 拉取上游 spec,
So that 新端点无需等 CLI 发版。

**Acceptance Criteria:**

**Given** 执行 `otl spec sync`
**When** 上游有更新
**Then** 运行时解析一次并以 bincode 落缓存（key 为 spec hash + CLI 版本，原子 rename 写入），`otl api list` 立即含新端点

**Given** 缓存文件损坏
**When** 任意命令启动
**Then** 自动废弃缓存回退内置 IR，不崩溃

### Story 4.3: doctor

As a 排障的用户,
I want 一条命令看清环境健康,
So that 问题自查不开 issue。

**Acceptance Criteria:**

**Given** 执行 `otl doctor`
**When** 诊断运行
**Then** 输出：认证状态、实例连通性、线上 API 与本地 spec 差异（缺失/已弃用操作）、spec 缓存健康

### Story 4.4: shell 补全

As a 终端用户,
I want tab 补全子命令与 flag,
So that 不背命令。

**Acceptance Criteria:**

**Given** `otl completions zsh`（bash/fish 同理）
**When** 装入 shell
**Then** 子命令、api 操作名、flag 名均可补全，补全内容由 IR 生成

### Story 4.5: 发布管道

As a 新用户,
I want brew 一条命令安装,
So that 上手零门槛。

**Acceptance Criteria:**

**Given** cargo-dist 配置
**When** 打 tag 发版
**Then** 自动产出 brew tap、shell 安装器、Windows MSI

**Given** release 构建
**When** CI 检查
**Then** 二进制体积在门槛内（目标 ~5MB），README 含退出码表、semver 契约与 `otl api` 不稳定声明
