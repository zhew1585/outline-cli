# Story 3.2: docs view

Status: done

## Story

As a 阅读文档的用户,
I want `otl docs view <id>` 直接读内容,
so that 终端内完成阅读。

## Acceptance Criteria

1. **Given** stdout 为 TTY 且内容超一屏
   **When** 执行 view
   **Then** 原始 markdown 自动进入 `$PAGER`
2. **Given** `--raw` 或输出为管道
   **When** 执行 view
   **Then** 纯内容直出无分页
3. **Given** `--web`
   **When** 执行 view
   **Then** 默认浏览器打开该文档 URL

## Tasks / Subtasks

- [x] Task 1: pager 模块 (AC: 1, 2)
  - [x] `crates/otl/src/pager.rs`：`Pager::parse($PAGER)`（空白分词，无 shell），缺省 `less -R -F -X`
  - [x] `exceeds_screen(text, height, width)`：按**屏幕行**（含自动换行）而非逻辑行计数，
        宽度经 `render::display_columns`（grapheme + unicode-width），留一行给 shell 提示符；
        终端尺寸用 `terminal_size`，未知回落 80x24
  - [x] 内容经 stdin 喂给 pager；spawn 失败 → stderr 警告 + 直出 stdout，退出码仍 0
  - [x] pager 提前退出（broken pipe）视为正常完成
  - [x] R1 修复：TTY 显示路径把控制字符换成 U+FFFD（`\r` 丢弃、`\t` 展开），并去掉默认 `less -R`；
        管道/`--raw` 路径逐字节原样输出，连结尾换行都不补
- [x] Task 2: browser 模块 (AC: 3)
  - [x] `crates/otl/src/browser.rs`：`$BROWSER` 优先，否则 macOS `open` / Windows `rundll32 url.dll,FileProtocolHandler` / 其余 `xdg-open`
  - [x] URL 始终作为独立 argv 末位参数传入，不经 shell
  - [x] `Session::absolute_url`：只接受"纯 root-relative 路径"（无 scheme/authority/`..`/空白/控制符/`\`/`:`），否则拒绝
  - [x] R1 修复：额外拒绝百分号编码的分隔符/点段（`%2e` `%2f` `%5c`）与不可见/双向格式字符
  - [x] R3 修复：不可见字符表改为完整 Unicode `Cf` 类别（`crate::text`），
        手写清单曾漏掉 U+061C、U+06DD、U+070F、U+0600–0605、U+13430 整块等 26+ 个字符
- [x] Task 3: 命令实现 (AC: 1, 2, 3)
  - [x] `commands/docs/view.rs`：`documents.info id=<id>`
  - [x] 默认输出 markdown（管道也是 markdown，不是 JSON）；`--json` 才是对象
  - [x] `--raw` 强制不分页；`--raw --json` 是用法错误；`--raw --web` 由 clap 互斥拒绝
  - [x] `--web`：先把 URL 打到 stdout，再启动 opener（启动失败仍留下可手工打开的 URL）
- [x] Task 4: 测试 (AC: 1, 2, 3)
  - [x] pager 单测：`$PAGER` 分词/元字符不解释/空值禁用分页/一屏边界/0 高度不下溢/真实 spawn（unix）
  - [x] browser 单测：`$BROWSER` 覆盖、Windows 默认不是 `cmd`、URL 单参数传递（unix）
  - [x] `tests/docs_view.rs`：管道得 markdown、`--raw`、`--json`、无 body 警告、404→5、`--web` 打印绝对 URL、跨源 URL 被拒、opener 失败仍留 URL

## Dev Notes

- **本命令是双态输出的一个例外，且是故意的**：`docs view` 的"数据"就是 markdown，
  所以管道拿到的是文档正文而不是 JSON——AC 明确要求"输出为管道 → 纯内容直出"。
  JSON 必须显式 `--json`。为此 `main.rs` 把 `--json` 这个 flag 本身（而不只是解析后的 `OutputMode`）
  传给 `docs::run`，因为 `OutputMode::Json` 无法区分"用户要 JSON"与"stdout 不是终端"。
  其余五个命令仍是标准双态。
- **分页判定**：`mode == Table` 已经等价于「stdout 是 TTY 且没有 --json」，再叠加 `!--raw` 与「超一屏」。
- **stdout 与 stderr 的清洗规则不同，而且都在出口**（R6 finding 1）：
  stderr 侧由 `stdio::write_diagnostic_line` 统一清掉除 `\n` 以外的控制字符与 Cf 格式字符，
  所以所有诊断路径（包括以后新增的）默认安全；stdout 侧的文档正文走 `pager`，
  规则是下面这条（交互路径清洗、管道逐字节）。两侧刻意分开：正文是用户要读的数据，诊断不是。
- **交互路径是"显示"，管道路径是"数据"**（R1 findings 6/11）：文档正文由任何有编辑权的人写入，
  而终端会把其中一部分字节当**命令**执行（OSC 52 改剪贴板、OSC 8 伪造超链接、光标移动重绘）。
  这些都不是 markdown，所以 TTY 路径一律替换为 U+FFFD 并在 stderr 说明可用 `--raw` 拿原始字节；
  管道与 `--raw` 则一个字节都不动——脚本做 hash/diff 必须拿到 API 的原文。
  默认 pager 参数因此去掉了 `-R`：既然显示路径已经过滤，`-R` 只会在"漏掉一个转义序列"时起作用，
  而那正是应该按字面显示的场景。
- **"超一屏"要按屏幕行算**（R1 finding 5）：终端会自动换行，一行 10,000 列在 80 列窗口里占 125 行。
  只数 `lines()` 会把它判成 1 行直接灌进终端。宽度计算复用 render 的 grapheme + unicode-width，
  CJK 宽 2、组合符宽 0 都算对。
- **tab stop 也必须按列算**（R2 finding 7）：tab 停位是**终端列**，所以它前面的文本也得按列量。
  按 UTF-8 字节算会在任何非 ASCII 文本之后把停位放错（`中` 是 3 字节但 2 列），
  进而让整行的列数算少、该分页的文档不分页。
- **pager 起来了但非零退出不能当成功**（R2 finding 9）：`PAGER=false` 会正常 spawn 后立刻退出 1，
  什么都没显示。判据只能是**退出状态**，不能是「它读了多少字节」——
  短文档整篇落进管道缓冲区，每次 write 都"成功"，与对端是否看过无关。
  常见 pager（less/more/bat/most）用户按 q 都退 0，所以非零即视为没显示，回退到 stdout 并警告。
  误判的代价是不对称的：多打一遍只是难看，不回退则是文档连同 exit 0 一起消失。
- **绝不把内容交给 shell**：pager 通过 stdin 拿正文，opener 通过独立 argv 拿 URL。
  Windows 特意不用 `cmd /c start`——`cmd` 会重新解析参数，URL 里的 `&` 会变成命令分隔符。
- **`--web` 的 URL 从 origin 拼**：`Session` 只保存 `scheme://host[:port]`（base URL 的 path 可能带凭证，
  见 engine 的凭证卫生规则）。Outline 的 `url` 字段是站点根相对路径，origin + path 即可。
- **服务端返回的 path 必须校验**：否则一个被攻破/被伪造的实例可以让 `--web` 打开任意 URL。
  校验失败时既不打印也不启动 opener，且错误消息不回显该 URL。

### References

- [Source: planning/epics.md#Story 3.2]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-5、stack.md#输出实现（v1 直接打印原始 markdown）]
- [Source: docs/exit-codes.md（pager/browser 失败的退出码语义）]

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-26

### Completion Notes List

- 新增依赖 `terminal_size 0.4`：std 无终端尺寸 API，自己做需要 `unsafe` ioctl（两 crate 都 forbid unsafe）。
- pager 是否真的被拉起无法在 assert_cmd 下测（进程无 TTY），所以拆成纯函数 `exceeds_screen` + `Pager::parse`
  单测，加上 unix 下用 `sh -c 'cat > file'` 当 pager 验证「内容走 stdin」。
- `$PAGER=`（空）解释为"用户要求不分页"，与 `git` 等工具一致。

### File List

- Cargo.toml（workspace dep `terminal_size`）、crates/otl/Cargo.toml
- crates/otl/src/{pager.rs, browser.rs}
- crates/otl/src/session.rs（`absolute_url` + 路径形状校验）
- crates/otl/src/commands/docs/view.rs
- crates/otl/src/main.rs、crates/otl/src/commands/docs.rs（传递 `--json` flag）
- crates/otl/tests/docs_view.rs
- docs/exit-codes.md（pager/browser 失败语义）
