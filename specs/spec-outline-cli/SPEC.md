---
project: outline-cli
date: 2026-08-25
companions:
  - stack.md
  - failure-modes.md
sources:
  - ../../brainstorming/brainstorm-outline-openapi-rust-cli-2026-08-25/brainstorm-intent.md
---

# SPEC: Outline Rust CLI

## Why

在终端工作的开发者需要不离开命令行就能检索、查看、创建和维护 Outline 知识库文档，并在脚本与 CI 中调用 Outline API。
Outline API 是纯 RPC 风格（全部 POST /api/resource.method），spec 即契约：少量抛光命令覆盖 90% 日用场景，通用引擎覆盖全部 API 长尾。

## Capabilities

### CAP-1 双模式认证
intent: 用户可用 API key（脚本/CI/无浏览器环境，env 兜底）或浏览器 OAuth 2.0（`otl auth login`，授权码 + PKCE + 本地回环回调）向任意 Outline 实例（云或自托管）认证。
success: 两种模式下 `auth info` 返回正确身份；OAuth access token 过期后请求自动用 refresh token 续期，用户无感知；凭证存于用户配置目录下的独立凭证文件（明文 + 仅属主可读写）。

### CAP-2 精选高频命令
intent: 六个抛光命令覆盖日用场景：文档搜索、查看、创建（支持 stdin 管道输入内容）、更新、集合列表、批量导出。
success: 六个命令在真实 workspace 上全部可用，输出人类可读；`cat notes.md | otl docs create --title X` 成功创建文档。

### CAP-3 通用 API 逃生舱
intent: vendored spec 中的任意操作都可通过 `otl api <op> k=v...` 调用；标量参数按 schema 类型转换，复杂嵌套参数经 `--body @file.json` 直通。
success: spec 内每个操作均可调用；spec 更新后新增端点无需改代码即出现在 `otl api list`。

### CAP-4 可靠请求通道
intent: 所有请求经唯一通道：发送前本地 schema 校验 fail fast，429 按 Retry-After 退避，Outline 错误信封映射为人类可读消息与稳定退出码。
success: 参数错误在发网络请求前报出；脚本在触发限流时不失败而是退避重试；每类错误有文档化的固定退出码。

### CAP-5 双态输出
intent: 默认输出为人类可读（表格/markdown），`--json` 供脚本消费，长文档自动进 $PAGER。
success: 同一命令交互使用可读、管道中可被 jq 消费；输出为非 TTY 时自动关闭分页与装饰。

### CAP-6 自动分页
intent: 列表类操作自动翻页，`--limit` 控制总量，截断时显式警告。
success: 导出超过单页大小的 collection 得到完整结果集，无静默截断。

### CAP-7 spec 生命周期
intent: `spec sync` 拉取上游最新 spec 并使其立即生效，`doctor` 对比线上 API 与本地 spec 的差异并报告。
success: sync 后新端点可用；doctor 能发现本地 spec 缺失或已弃用的操作。

## Constraints

- 启动时间 <10ms：运行时零 OpenAPI/YAML 解析；仅 `spec sync` 路径运行时解析一次并落 bincode 缓存。
- refresh_token 每次刷新都轮换（服务器实测确认）：凭证持久化必须原子写，并发刷新必须防护（文件锁或单飞）。
- 精选命令接口走 semver 稳定契约；`otl api` 裸调模式明示不保证稳定。
- 两步协议（附件上传 attachments.create → S3 POST 等）通用引擎不覆盖：手写特例命令预算约 5 个。
- 上游 spec 为社区维护：仅经 overlay 打补丁，不 fork；CI 对真实 workspace 跑契约测试兜底正确性。
- 三平台（macOS/Linux/Windows）均为一等公民，CI 矩阵覆盖。
- 自托管实例支持：base URL 可配置；实测环境为一个自托管实例，地址从 env 注入（不写入仓库）。
- 命名已定：二进制 `otl`，crate `outline-cli`（crates.io 上 `outline` 与 `otl` 已被占用，二者均为无关项目）。
- CLI 不主动联网检查更新或 spec（不 phone home）；spec 更新仅随版本发布或用户显式 `spec sync`。

## Non-goals

- docs pull/push 本地 markdown 双向同步（v2 候选杀手级功能，v1 不做，但分页引擎与 revision 冲突检测能力为其留口）。
- TUI（ratatui collection 浏览器）。
- `watch` 事件轮询与桌面通知。
- MCP server 模式。
- OAuth device flow（上游不支持）。
- 离线写入队列（明确判定为过度设计）。
- 全量类型化 codegen（每端点生成函数/类型的路线已被否决）。

## Success signal

对真实 workspace（自托管实例或云版，地址从 env 注入）：`otl auth login` 完成 OAuth 全流程且后续请求自动续期；六个精选命令全部成功；spec 中任选 3 个未精选端点经 `otl api` 调用成功；`time otl --help` 启动 <10ms；`--json` 输出被 jq 消费且退出码符合文档。

## assumptions[]

- 社区 spec 基本正确，契约测试可兜住偏差。
- Outline 限流响应含 Retry-After 头（未逐端点验证）。
- OAuth scopes 粒度 read/write 满足 v1（实测服务器仅广播这两个全局 scope）。

## open_questions[]

（无。2026-08-25 全部拍板，决策落于 Constraints 与 stack.md，记录见 .memlog.md。）
