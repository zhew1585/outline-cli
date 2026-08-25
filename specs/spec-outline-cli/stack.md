# Stack & 实现决策

实现层契约（HOW）。
kernel 的 intent 不含实现处方，全部落在此处。

## 架构

- 语言 Rust。
- 两 crate 分层：通用 OpenAPI RPC 引擎 crate + Outline UX 层 crate；引擎未来可独立库化。
- build.rs 在编译期把 vendored spec 编译成精简 IR：静态数据表（每操作仅 path + 参数 schema + auth 要求），非每端点生成函数。
- 运行时单一通用 `execute(op, args)` 分发器解释 IR；clap 命令树运行时由 IR 构建。
- 所有请求均为 POST JSON（Outline 全 API 统一），一个通用 request builder 覆盖 100% 端点。
- `spec sync` 路径：运行时解析新 spec 一次，产物以 bincode 落 `~/.cache/outline-cli`，key 为 spec hash + CLI 版本；缓存写入原子 rename，损坏自愈重解析。

## spec 供给

- vendor 上游 https://github.com/outline/openapi ，`include_str!` 打包进二进制；`--spec` 参数允许开发时覆盖。
- 定制经 overlay 文件（x-cli 扩展：别名、输出模板、示例），不改上游文件。
- 更新通道（已定）：随版本 vendored 为主；`spec sync` 手动可选；无自动检查。

## 认证实现

- API key：Bearer token，`OUTLINE_API_KEY` env 兜底。
- OAuth：授权码 + PKCE S256 + 本地回环回调；服务器元数据自 `/.well-known/oauth-authorization-server` 发现；默认申请 scope `read write`。
- 客户端获取（已定）：双路径，DCR 优先。未配置 client_id 时自动 DCR 注册（公共客户端），注册结果连同 registration_access_token 本地缓存；DCR 不可用（workspace 未开 MCP）时回退提示管理员在 Settings → Applications 预注册。
- DCR 清理约束（源码确认）：DCR 客户端管理员无法在界面删除，仅能凭 registration_access_token 走 RFC 7592 删除。CLI 必须持久化该 token，并提供 `otl auth logout --purge` 自删注册，避免服务器堆积孤儿客户端。
- 回调端口（已定）：DCR 场景先绑定随机端口，再以实际端口的精确 redirect_uri 注册；预注册场景使用文档化固定端口清单（4 个备选 URI 依次尝试绑定）。
- 实测确认（docs.91aql.com，脚本 scripts/test_oauth.py）：DCR（RFC 7591）自注册可用（依赖 workspace MCP 偏好开启）；公共客户端（无 secret）+ PKCE 可用；access token 3600s；refresh_token 每次轮换；revoke 正常。
- 自托管无需服务端开关；管理员预注册路径为 Settings → Applications（需 admin 权限）。
- 凭证存 keyring crate（系统钥匙串）；刷新后原子持久化，防并发刷新。

## 依赖基线

- 核心：clap + serde + anyhow + reqwest（rustls 后端，已定；OAuth 回调并存、批量导出并发、生态成熟）。
- 辅助：directories（跨平台路径）、keyring、clap_complete（补全由 IR 生成）。

## 输出实现

- v1 文档内容直接打印原始 markdown；富渲染（termimad/syntect）v2。
- `--template`（minijinja 用户模板）为后续项。

## 测试

- wiremock 模拟 + spec 生成 fixture；输出渲染 golden file 测试。
- CI 对真实测试 workspace 跑契约测试（spec 正确性兜底）。
- 三平台 CI 矩阵。

## 分发

- cargo-dist：一份配置产出 brew tap、shell 安装器、MSI。
- 单静态二进制，目标体积 ~5MB。
- 命名（已定）：crate `outline-cli`，二进制 `otl`；备选名 outl/oln/outctl。

## 命令与配置（v1 范围，已定）

- 6 个精选命令用人体工学名（`otl docs search`）；长尾用 API 原名（`otl api documents.info k=v`）。
- 不做用户自定义 alias 表（推迟）。
- 配置：env 变量 + 单一用户配置文件（TOML，含命名 profile 支持多 workspace）；项目级 `.outline.toml` 推迟。
- 优先级：flag > env > 用户配置。

## MVP 顺序

1. documents.* 子集跑通 build.rs IR 管线 + 通用分发器。
2. 铺开全部端点 + `outline api` 逃生舱。
3. 六个精选命令抛光 + OAuth login。
