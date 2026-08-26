# Story 4.4: shell 补全

Status: review

## Story

As a 终端用户,
I want tab 补全子命令与 flag,
so that 不背命令。

## Acceptance Criteria

1. **Given** `otl completions <bash|zsh|fish|powershell|elvish>`
   **When** 装入 shell
   **Then** 五个 shell 均可补全子命令与 flag 名；**bash / zsh / fish** 另可补全 api 操作名，
   补全内容全部由 IR 生成。powershell / elvish 不含操作名（上游生成器对 positional 值不产候选，
   见 Dev Notes），且每份生成脚本在头部注释里自陈其覆盖范围，不做超出实际的声明
1b. **Given** 生成脚本会被 shell 执行
   **When** IR 里出现含引号 / `$()` / 反引号 / 换行 / 控制字符的操作名
   **Then** 该名字不写入脚本（build 期直接拒绝该 spec，运行期再过一次白名单）
2. **Given** 输出被管道消费或重定向
   **When** 生成补全脚本
   **Then** 脚本走 stdout、诊断走 stderr；提前关闭管道不 panic
3. **Given** NFR1 启动预算
   **When** 生成补全
   **Then** 不引入运行时 OpenAPI/YAML 解析，`otl --help` 冷启动仍 <10ms

## Tasks / Subtasks

- [x] Task 1: `completions` 子命令 (AC: 1)
  - [x] `otl completions <bash|zsh|fish|powershell|elvish>`，shell 参数用 `clap_complete::Shell`（ValueEnum）
  - [x] `main.rs` 追加 `Command::Completions`，把 `Cli::command()` 传进去（`Cli` 留在 binary 里，不为补全重构 lib）
- [x] Task 2: 补全内容由 IR 生成 (AC: 1)
  - [x] 生成前把 `ops::OPS` 的操作名（外加保留名 `list`）挂到 `api` 的 operation positional 上
  - [x] 只改用于生成的命令树副本：真实 parser 仍接受任意操作名，未知操作继续走 otl 自己的错误消息
  - [x] 用 `mut_args` 而不是 `mut_arg`（后者会重排 positional 索引，触发 clap 自身的 debug assert）
- [x] Task 2b: 候选文本受约束而非受信任 (AC: 1b)
  - [x] `is_safe_operation_name`：白名单 `[A-Za-z0-9._-]`，长度上限
  - [x] `build.rs` 对 vendored spec 的操作名做同样检查，不合规直接 panic（构建失败）
  - [x] fish 描述转义再加「丢弃控制字符 + 截断」
  - [x] 每份脚本头部注释自陈覆盖范围（`#` 在五个 shell 里都是行注释）
- [x] Task 3: 管道安全 (AC: 2)
  - [x] 脚本先生成到内存再经 `stdio::write_data` 写出（broken pipe = 正常结束，退出码 0）
  - [x] 生成路径不需要任何配置：无 URL、无 API key、无配置文件也必须成功
- [x] Task 4: 测试 (AC: 1-3)
  - [x] `tests/completions.rs`：10 个用例（五个 shell 各自产出脚本、操作名全覆盖、管道关闭、未知 shell 退出 2 等）
  - [x] 断言 `otl api --help` 不因此变成操作名转储
  - [x] 断言 fish 追加规则所依赖的 clap_complete 条件函数名仍然存在（升级时会明确失败）

## Dev Notes

- **为什么改副本而不是真实命令树**：给 operation positional 挂 `PossibleValuesParser` 会让 clap 自己拒绝
  未知操作，从而丢掉 `unknown API operation ...（run `otl api list`）` 这条更有用的错误消息，也会把 200
  个操作名倒进 `--help`。所以增强只作用于生成用的克隆，两个测试分别钉住这两点。
- **各 shell 覆盖度不同，这是上游生成器的限制**（clap_complete 4.6.9 源码实查）：
  - bash / zsh：原生为 positional 输出候选值。
  - fish：生成器自带注释 “currently only supports named options (-o/--option), not positional
    arguments”，因此操作名以普通 `complete` 规则追加，条件复用生成器自己定义的
    `__fish_otl_using_subcommand api`。若上游改名，追加逻辑什么也不输出（条件永不命中的规则比没有更糟），
    并由测试立即失败。
  - powershell / elvish：生成器只输出 flag 与子命令，对任何 positional 值都不输出候选（连
    `otl completions <shell>` 自己的取值也不补全）。这两个 shell 因此只有子命令与 flag 补全。
- **没有用 `unstable-dynamic`**：clap_complete 的动态补全能统一解决所有 shell 的 positional 问题，但它是
  显式 unstable 的 feature，且会改变安装方式（`COMPLETE=<shell> otl` 回调进程）。对一个把退出码和 flag
  当公共 API 的 CLI 来说，押注 unstable API 不划算。
- **启动预算**：`ops::OPS` 只在 completions 路径被遍历，`--help` 路径完全不碰。实测 3.38 ms。
  release 体积 2 783 488 字节（新增 clap_complete + toml + directories + IR 响应字段共 +216 KiB，预算 ~5 MB）。
- **fish 描述转义**：单引号字符串里 `\` 与 `'` 需转义（操作摘要含撇号，如 “Change a users role”），
  并丢弃控制字符——摘要来自 spec，ESC 若存活到补全描述里会在候选显示时被终端解释。
- **候选文本是可执行代码（R1 finding 4 的处置）**：操作名会进入 bash 的 `opts="..."`、zsh 的
  `_arguments` 单引号值表、fish 的 `-a "..."`。原实现只依赖「当前 spec 恰好安全」。现在两道防线：
  `build.rs` 对 vendored spec 的操作名做白名单校验（不合规 → 构建失败，这是唯一能**排除**而非过滤问题的
  地方），运行期 `is_safe_operation_name` 再过一遍（覆盖将来 `spec sync` 缓存这条不经过 build.rs 的路径）。
  测试含 16 个敌对名字的白名单单测、对已编译 IR 全表的断言、从 fish 脚本里反向抽取候选再逐个验白名单、
  以及 `bash -n` / `zsh -n` 语法检查。
- **为什么 powershell / elvish 不补操作名（R1 finding 5 的处置）**：审查者认可「上游生成器不支持
  positional values」是事实，但指出「宣称支持五种 shell 却只交付三种」是口径问题。处置是**收敛口径**而非
  硬塞实现：AC 已按 shell 明确写清；`completions` 子命令的 long help 写清；README 写清；每份生成脚本
  头部注释写清（powershell/elvish 的脚本会明说 “operation names are NOT completed here”，并指向
  `otl api list`）。测试 `each_script_states_its_own_coverage` 与
  `a_shell_that_claims_operation_names_actually_carries_them` 用同一个
  `completes_operation_names(shell)` 判定表双向核对——脚本里的声明与实际内容不一致即失败。
  不为这两个 shell 做 splice 注入的理由：它们的脚本是嵌套结构（elvish 的 `&'otl;api'= {...}` 映射、
  powershell 的 `switch` 块），注入要手写两种新方言的 shell 代码，正是 finding 4 警告的那类风险；
  fish 之所以例外，是因为它的格式是行式 `complete` 语句，可以纯追加。

### R1 对抗审查处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| 4 | MAJOR | 已修：build 期白名单（构建失败）+ 运行期白名单 + 描述去控制字符；含敌对 fixture 与 `bash -n`/`zsh -n` 检查 |
| 5 | MAJOR | 已修（收敛口径）：AC / long help / README / 每份脚本头部注释均按 shell 写明覆盖范围，双向测试核对 |

### R2 复核处置（2026-08-26）

| # | 级别 | 处置 |
|---|------|------|
| R2-5 | MINOR | 部分修复（R3-7 补完）：R1 的口径收敛漏了公开 rustdoc（模块开头仍写 “operation names all complete”）。现在模块文档按 shell 写明，并新增 `the_public_module_documentation_matches_the_delivered_coverage`——直接读源文件的模块注释，与 `completes_operation_names` 判定表核对。文档也是一种声明，一并纳入测试。 |

| R3-7 | MINOR | 已修：R2 那个 rustdoc 测试只校验「被支持的 shell 被提到」，追加一句 “powershell and elvish operation names complete” 仍会通过（循环里对谓词为 false 的分支没有 else 断言）。现在按**句子**双向校验：任何正面声称补全操作名的句子都不得点到未覆盖的 shell；另加一个「守卫的守卫」测试，用审查者给的那句原文验证检查本身会命中 |

| R4-4 | MINOR | 已修：R3 的检查只挡住了「给未支持 shell 加正面声明」，挡不住反向漂移——把文档改成 “bash, zsh, fish do not complete operation names” 时，正向断言仍因子串匹配通过，该句又被 `not complete` 当成否认句跳过。现在按句子分类做**对称**两条规则：正面声明不得点到未覆盖 shell，否认句不得点到已覆盖 shell，且必须存在一句正面声明点齐全部已覆盖 shell。`is_denial` 特意不以 “only” 为据（“complete in bash, zsh, fish only” 是正面声明）。guard-the-guard 现在两个方向各一个样本，并已做变异验证：两种漂移分别触发 `rustdoc claims powershell...` 与 `rustdoc denies that bash...` |

| R5-2 | MINOR | 已修：R4 的检查按**整句**二元分类，混合句会漏——`powershell does not complete operation names, but elvish does` 整体被判为否定句，后半句对 elvish 的错误正面声明就没被检查。现在两个粒度：句级用于「必须存在一句点齐全部已覆盖 shell 的正面声明」（逗号列表要完整保留），**子句级**用于漂移检测（在 `.` `;` `,` 与 but/while/whereas/however 处切分）。`affirms_completion` 还识别省略动词的子句（“but elvish does”“elvish too”），否则混合句照样能溜过去。guard-the-guard 现在三个方向各一个样本，并已对真实文档做变异验证：三种漂移全部被检出 |

R2/R3/R4/R5 已 VERIFIED：R1-4（build 期与生成期两层白名单都实际生效）。审查者备注「当前仓库尚无运行时缓存接缝可
进一步验证」——生成期过滤不依赖 build.rs 的编译期拒绝，是独立的第二道，specsync 合并后可直接复验该接缝。

### 故意留下的缺口

- powershell / elvish 不补全 api 操作名（上游生成器限制，见上）。已在 AC、help、README、模块 rustdoc
  与脚本头部注释内自陈，五处均有测试或核对。
- fish 在 `otl api <op> ` 之后仍会继续提供操作名候选（fish 的 `complete` 条件无法表达「第 N 个位置」）。
  噪声可接受，替代方案是为 200 个名字生成 `not __fish_seen_subcommand_from` 条件，代价远大于收益。
- 不提供 `otl completions --install`（自动写入 shell 配置）：那属于分发范围（Story 4.5），且会往用户的
  rc 文件里写东西，超出本 story。

### References

- [Source: planning/epics.md#Story 4.4、FR25]
- [Source: specs/spec-outline-cli/stack.md#依赖基线（clap_complete，补全由 IR 生成）]
- [Source: specs/spec-outline-cli/SPEC.md#Constraints（启动 <10ms）]
- [Source: project-context.md CLI 行为契约（双态输出）]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### Completion Notes List

- 偏差：`hide_possible_values(true)` 会连生成脚本里的候选一起去掉（实测），因此没有使用；`--help` 的整洁
  靠「只增强副本」保证，而不是靠这个 flag。
- 质量门：fmt / clippy `-D warnings` / `cargo test --workspace`（334 通过）全绿；bench 3.38 ms。

### File List

- crates/otl/src/commands/completions.rs（新增）
- crates/otl/src/commands/mod.rs（追加 `pub mod completions;`）
- crates/otl/src/main.rs（追加 `Completions` 子命令）
- crates/otl/tests/completions.rs（新增）
- Cargo.toml, crates/otl/Cargo.toml（clap_complete 依赖）
- README.md
