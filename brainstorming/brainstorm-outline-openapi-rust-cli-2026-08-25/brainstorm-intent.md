# Intent: Outline Rust CLI

来源: 2026-08-25 头脑风暴会话。
用途: 作为 bmad-prd / bmad-spec / bmad-architecture 的输入。

## 产品意图

一个 Rust 编写的 Outline 知识库命令行客户端。
面向在终端工作、需要日常检索/查看/创建/维护 Outline 文档的开发者，以及在脚本和 CI 中调用 Outline API 的自动化场景。
价值主张: Outline API 是纯 RPC 风格 (全部 POST /api/resource.method)，spec 即契约; CLI 以少量抛光命令覆盖 90% 日用场景，同时通过通用引擎覆盖全部 API 长尾。

## 已确认决策

- 语言: Rust (用户决策; 开发成本不作为权重，看重产物质量、类型安全的 IR 建模、库化前景)。
- spec 来源: 直接 vendor 官方仓库 https://github.com/outline/openapi ，不自行生成。
- 架构: build.rs 在编译期把 vendored spec 编译成精简 IR (静态数据表，非每端点生成函数)。
- 运行时: 单一通用 execute(op, args) 分发器; 运行时零 OpenAPI 解析，启动目标 <10ms。
- 所有请求均为 POST JSON，一个通用 request builder 覆盖 100% API。
- UX 形态: 混合式 = 约 6 个抛光的精选命令 + `outline api <op> k=v...` 裸调逃生舱兜住长尾端点。
- 稳定性契约: 精选命令走 semver; api 裸调模式明示不保证稳定。
- 认证: 双模式。
  - API key (Bearer token): 脚本/CI/无浏览器环境，OUTLINE_API_KEY 环境变量兜底。
  - OAuth 2.0: `outline auth login` 走授权码 + PKCE + 本地回环端口回调拉起浏览器; 应用需在 workspace 设置注册 (client id/secret); 支持 scopes (全局/命名空间/端点/通配符) 与 refresh token; Outline 无 device flow。
  - 两种模式的凭证均存 keyring 系统钥匙串; access token 过期由唯一请求通道自动用 refresh token 续期。
- 输出: 默认人类可读，--json 供脚本; 长文档进 $PAGER。
- 分层: 通用 OpenAPI RPC 引擎与 Outline UX 层分离 (两个 crate)，引擎未来可独立库化。

## 范围形态

### MVP 路径

先用 documents.* 子集跑通 build.rs IR 管线，验证编译期 IR + 通用分发器可行，再铺开全部端点。
依赖克制: 核心为 clap + HTTP client + serde + anyhow。

### 6 个高频精选命令 (90% 使用集中于此)

- search: 文档搜索
- view/info: 查看文档
- create: 创建文档 (支持 stdin 管道输入内容)
- update: 更新文档
- collections list: 集合列表
- export: 批量导出 (建立在分页引擎上)

### 明确延后 (deferred)

- docs pull/push 本地 markdown 双向同步: 延后，但已标记为未来最可能的杀手级功能 ("Outline 作为 docs-as-code 后端")。
- TUI (ratatui collection 树浏览器): 延后。
- watch (轮询 events API 桌面通知): 延后。
- MCP server 模式 (`outline mcp serve`，复用同一 IR): 延后。
- 离线写入队列: 明确不做 (过度设计)。

## 关键约束与风险 (含缓解)

- 社区维护的 spec 可能不正确/过时: CI 对真实测试 workspace 跑契约测试; vendored spec 允许打补丁 (overlay，不 fork 上游)。
- schema 中 oneOf/anyOf 难以映射为 CLI flag: 此类操作回退 `--body @file.json` 直通; spec 驱动的 flag 只处理标量。
- 两步协议 (如 attachments.create → S3 POST) 通用引擎覆盖不了: 预算约 5 个手写特例命令。
- 429 限流 (按用户计): 唯一请求通道内建 Retry-After 退避 + 全局令牌桶节流。
- 分页静默截断: 通用自动翻页 + --limit + 截断警告 (Outline 分页信封统一 offset/limit)。
- 跨平台差异 (Windows 路径/钥匙串): directories + keyring crate，三平台 CI 矩阵。
- spec 漂移: `spec sync` 重新拉取; `outline doctor` 对比线上 API 与本地 spec 差异; 弃用操作在 IR 标记并警告一个版本。
- 错误体验: Outline 错误信封映射为人类可读消息 + 稳定退出码表; 发送前本地 schema 校验 fail fast。

## 待定问题

- 二进制命名: 需查 crates.io / brew 上 outline 是否冲突; 备选短名 otl。
- spec 更新通道: 随版本 vendored / `spec sync` 手动拉取 / 每周自动检查提醒，三选一或组合，未定。
- 参数命名层叠细节: API 原名 (documents.info) 为规范名，人体工学别名 (docs info) 层叠，具体映射表未定。
- 配置层叠 (flag > env > 项目 .outline.toml > 用户配置) 与多 workspace profile 的落地版本未定 (v1 倾向只用环境变量)。
- HTTP client 选型: reqwest vs ureq (同步、更小二进制) 未最终敲定。
- OAuth client 注册的分发方式: 官方云版可预注册一个公共 client id 随 CLI 发布。
  自托管已确认 (源码级): OAuth provider 为核心功能，无需任何 env/开关; 注册应用需 workspace 管理员在 Settings → Applications 手动创建。
  备选自动化路径: workspace 开启 MCP 偏好时可走 RFC 7591 动态客户端注册 (DCR)，CLI 自注册免管理员配置; 是否实现 DCR 待定。
- OAuth 回环回调端口策略 (固定端口 vs 随机端口 + 注册多个 redirect URI) 未定。
- 默认申请的 scope 集合未定 (最小权限 vs 全量便利)。
