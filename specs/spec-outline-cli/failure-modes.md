# 失效模式与缓解

| # | 风险 | 缓解 |
|---|------|------|
| 1 | 社区维护的 spec 不正确或过时 | CI 对真实测试 workspace 跑契约测试；vendored spec 经 overlay 打补丁，不 fork 上游 |
| 2 | schema 中 oneOf/anyOf 无法映射为 CLI flag | 该类操作回退 `--body @file.json` 直通；spec 驱动的 flag 只处理标量 |
| 3 | 两步协议（attachments.create → S3 POST 等）引擎不覆盖 | 约 5 个手写特例命令，预算固定 |
| 4 | 429 限流（按用户计）导致脚本随机失败 | 唯一请求通道内建 Retry-After 退避 + 全局令牌桶节流 |
| 5 | 分页静默截断（默认仅 25 条） | 通用自动翻页 + `--limit` + 截断显式警告 |
| 6 | refresh_token 轮换 + 并发刷新导致凭证失效 | 凭证文件原子写（同目录 temp → fsync → rename）+ 锁文件建议锁做刷新单飞 |
| 7 | 跨平台差异（Windows 无 POSIX 权限位） | directories 解析路径；Unix 强制 0600 并校验，Windows 依赖 profile 目录 per-user ACL 且由 `auth info`/`doctor` 明示差异；三平台 CI 矩阵 |
| 10 | 凭证文件明文落盘被其他本地进程/备份/同步盘读走 | 创建即 0600（无 chmod 竞态）；读取时权限过宽即拒用并给修复命令；凭证与 config 分文件，避免误提交误分享；`doctor` 报告文件位置与权限状态但绝不打印凭证内容；文档明示依赖全盘加密 |
| 8 | 上游 spec 漂移/端点弃用破坏用户 | `spec sync` 重拉；`doctor` 差异报告；弃用操作 IR 标记并警告一个版本；精选命令 semver、api 模式明示不稳定 |
| 9 | 动态命令 UX 失控（60 端点全裸露不可用） | 精选 6 命令抛光 + 长尾仅经 `outline api` 显式路径暴露 |
