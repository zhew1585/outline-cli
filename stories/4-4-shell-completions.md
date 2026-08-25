# Story 4.4: shell 补全

Status: review

## Story

As a 终端用户,
I want tab 补全子命令与 flag,
so that 不背命令。

## Acceptance Criteria

1. **Given** `otl completions zsh`（bash/fish 同理）
   **When** 装入 shell
   **Then** 子命令、api 操作名、flag 名均可补全，补全内容由 IR 生成
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
- **fish 描述转义**：单引号字符串里 `\` 与 `'` 需转义（操作摘要含撇号，如 “Change a users role”）。

### 故意留下的缺口

- powershell / elvish 不补全 api 操作名（上游生成器限制，见上）。AC 点名的 zsh/bash/fish 均已覆盖。
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
