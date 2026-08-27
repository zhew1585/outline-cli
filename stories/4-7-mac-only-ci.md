# Story 4.7: 收窄到 macOS 单平台

Status: done

## Story

As 项目所有者,
I want CI 与发布只覆盖 macOS,
so that 现阶段的验证与分发集中在唯一实际使用的平台上；**这是暂时收窄，不是永久放弃**，恢复路径见本文末尾。

## Acceptance Criteria

1. **Given** 只承诺 macOS
   **When** CI 运行
   **Then** 编译/运行 crate 的 job 只在 macOS 上跑；启动时间门禁在 macOS 上测量（不再在 Linux 上量一个不发布的平台）
2. **Given** dist 配置
   **When** 打 tag 发版
   **Then** 只产出两个 Apple target 的归档 + shell 安装器 + brew formula；`release.yml` 由 `dist generate` 重新生成而非手改
3. **Given** Windows/Linux 发布配置已不可达
   **When** 清理
   **Then** 不留任何「没有门禁执行、会静默腐烂」的休眠配置；story 写清恢复路径
4. **Given** 体积门禁
   **When** 检查
   **Then** 两个出货 target 全部本机实测、各有预算；NFR2 的 5,000,000 B 承诺照旧独立检查

## Tasks / Subtasks

- [x] Task 1: CI 收窄 (AC: 1)
  - [x] `ci.yml` 的 `build & test` matrix → 只留 `macos-latest`
  - [x] `startup time gate` 从 ubuntu 搬到 macOS（hyperfine 改用 `brew install`）
  - [x] `contract tests` 搬到 macOS（它编译并运行 crate，见 Dev Notes）
  - [x] `runtime-yaml-guard` 留在 ubuntu，但改为**显式枚举出货 triple**（见 Dev Notes：依赖图是平台相关的）
  - [x] 新增 `windows-source-lint` job（macOS 宿主，clippy-only，无需 Windows runner）
- [x] Task 2: dist target 收窄 (AC: 2)
  - [x] `targets` → `aarch64-apple-darwin` + `x86_64-apple-darwin`（保留 Intel，不用 universal2，理由见 Dev Notes）
  - [x] `installers` → `["shell", "homebrew"]`（去掉 `msi`）
  - [x] 去掉只为手维护 wxs 存在的 `allow-dirty = ["msi"]`
  - [x] `dist generate` 重新生成 `release.yml`；`dist generate --check` 通过
- [x] Task 3: 删除不可达的 Windows/Linux 发布配置 (AC: 3)
  - [x] 删 `crates/otl/wix/main.wxs`、`scripts/check-wxs-drift.sh`、`scripts/check-msi-version.sh`、`scripts/install-linux-musl-toolchain.sh`
  - [x] 删 `crates/otl/Cargo.toml` 的 `[package.metadata.wix]`
  - [x] `release-guards.yml` 去掉两个 MSI 步骤与 msi/musl 产物断言
  - [x] 恢复所需的 GUID 等值**逐字记录在本文恢复路径**中
- [x] Task 4: 修一个因收窄而产生的静默失效 (AC: 1)
  - [x] build-setup 注入的结构性断言原本 `if: runner.os == 'Linux'`——收窄后永不触发；去掉该条件
  - [x] `check-release-gating.sh` 增加断言：门禁 step 必须**无条件**，而不只是「存在」
- [x] Task 5: 体积门禁 (AC: 4)
  - [x] 预算表只留两个 Apple target，全部本机实测
  - [x] header 去掉 musl 外推，改写余量说明（绑定 target 换人、余量翻倍）
  - [x] `check-all.sh` 两个 target 都测
- [x] Task 6: 文档与验证
  - [x] README 安装章节、平台表、开发章节、CI 描述
  - [x] 变异验证四条（见 Dev Agent Record）
  - [x] `check-all.sh --windows` + 全套 release 门禁

## Dev Notes

- **哪些 job 该搬、哪些不该，判据是「这个 job 编译或运行 crate 吗」**。编译/运行的 job 必须针对出货平台，否则红灯可能来自没人拿到的 target，绿灯也不说明用户拿到的东西是好的。据此：`build & test`、`startup gate`、`contract tests` 全部搬到 macOS。`contract tests` 这条 coordinator 原倾向留在 ubuntu（平台无关的契约检查、runner 更便宜）——我搬了，理由是它**编译并运行**产物，留在 ubuntu 等于隐性承担「Linux 必须一直能构建」的维护义务，而 Linux 一旦编译坏了会用一个我们不发布的平台把 CI 弄红。另外本仓库是 **public**，macOS runner 分钟数免费，成本论据不成立。
- **`runtime-yaml-guard` 我没有搬宿主，而是把它改成显式枚举 target——这比搬宿主更对**。实测本 workspace 的运行时依赖图**是平台相关的**：Linux 宿主会解析出 `linux-raw-sys`、`openssl-probe`、`rustls-native-certs`（我们不发布），而**漏掉** `core-foundation`、`core-foundation-sys`、`errno`、`security-framework`、`security-framework-sys`（我们发布）。也就是说这个 guard 一直在检查一份不是我们出货的 crate 集合，且**看不见只在 macOS 可达的 YAML 解析器**——与「在 Linux 上量启动时间」是同一类错误。改成 `cargo tree --target <triple>` 逐个出货 triple 检查后，宿主选择不再影响正确性（`cargo tree` 不编译，也不需要装该 target），于是留在便宜的 ubuntu 上既正确又省钱。这是比「搬到 mac 就对了」更彻底的修法：把正确性从「runner 恰好是 mac」变成「显式声明」。
- **保留两个 Apple target，不用 universal2**。universal2 看起来更整洁（一个产物），但 fat binary 同时装两个 slice，实测两个瘦产物是 3,464,608 + 4,007,712，合起来约 7.5 MB，**直接违反 NFR2 的 ~5 MB 承诺**。两个瘦产物守得住承诺，一个胖产物守不住。Intel Mac 仍在用，所以也不能只留 arm64。
- **`win-check.sh` 与 `--windows`：不删，反而搬进 CI**。coordinator 倾向一起删（没人执行的检查等于没有检查），这个原则我同意，但它在这里指向的结论相反。关键事实：Windows **源码**是明确要保留的（`#[cfg(windows)]` 分支、`tests/portability.rs`，删了「后面再加」就变考古），而 **macOS 永远不编译那些分支**——`cargo test`、`cargo fmt`、乃至 macOS 上的 `cargo clippy` 都看不见它们。这正是那两次 `doc_lazy_continuation` 溜进来的机制。所以「保留 Windows 源码 + 删掉唯一编译它的东西」= 留下一堆无人验证、必然腐烂的代码，恰好是那条原则要防的事。
  解法不是保留一个本地 opt-in flag（那还是「没人执行」），而是**把它变成被执行的**：`cargo clippy --target x86_64-pc-windows-msvc` 只做类型检查与 lint、**从不链接**，所以无需 Windows runner、无需 MSVC 工具链，在 macOS runner 上 21 秒跑完。已作为 `windows-source-lint` job 进 CI。本地 `--windows` flag 保留为可选加速手段。
  顺带说明它**不是**平台承诺：它 lint 的是保留下来的源码，不代表 Windows 被支持、被测试或被发布。
- **coordinator 提的「把额外 lint 视角用 `cargo doc -D warnings` 留住」这条我试了，做不到**：`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` 当前**失败**，`spec-compile` 有 `public documentation for 'compile_json' links to private item 'document'` 等私有 intra-doc link 错误（agent-discovery 的 crate，不是我的改动范围）。所以那个替代方案现在不可用，而额外 lint 视角的真实来源本来就是「编译 cfg(windows) 分支」，已由上一条保住。**这条 doc 警告本身值得单独处理**：`check-all.sh` 的 `cargo doc` 不带 `-D warnings`，所以它们目前完全不可见。
- **删除而非休眠，但恢复所需的不可再生信息必须留下**。wxs 的重建成本其实很低——`check-wxs-drift.sh` 当初就证明了它 = dist 模板输出 + 一个注释块 + 一个 attribute，所以 `dist generate` 加两处手改即可复现。真正**不可再生**的是 GUID：MSI 的 `upgrade-guid` 必须跨版本稳定，重新生成一个会让 Windows 把新版本当成另一个产品。因此 GUID 逐字记在下面的恢复路径里。（今天尚无任何已发布的 MSI，所以即便丢了也无既有安装受损——但记下来零成本。）
- **收窄暴露了一个静默失效，这是本 story 最值得记的部分**。build-setup 注入的结构性断言带着 `if: runner.os == 'Linux'`（当初为了绕开 Windows runner 上 Git Bash 没有可靠 python3）。收窄后发布构建**没有 Linux leg**，该条件永不为真：step 仍在生成的 workflow 里，`check-release-gating.sh` 仍能按名字找到它并断言「它在 build-local-artifacts 里跑」——**断言通过，检查从不执行**。又一次「声明大于实现」。修法两步：(a) 去掉该条件（而不是改成 macOS，免得下次平台集变动再腐烂一遍）；(b) 把断言从「存在」升级为「存在且无条件」——具体做法是把 job 切成 step 块、定位包含脚本名的那一块、断言其中没有 `if:`。只针对门禁 step，其他 step（比如将来重新加回的交叉工具链安装）仍可合法地带平台条件。
- **绑定 target 换人了，余量翻倍**。musl 出货期间它是最大产物，4,556,160 B 对 5,000,000 B 承诺只剩约 443,840 B（8.9%）；现在最大产物是 `x86_64-apple-darwin` 4,007,712 B，余量 **992,288 B（19.8%）**。脚本 header 明确写了这个变化，免得下一个人带着旧的紧张感做决定；也写明了「Linux/Windows 回来时绑定 target 与这个数都会变，要重测而不是沿用」。
- **本机能测全部出货 target 了**，musl 那个「唯一裁决者是 CI、本地只能外推、而外推错了 0.74 MB」的结构性问题随收窄消失。header 里的外推段落已删除，只保留一段历史性的「target 之间不可互推」的实测对照，供将来重新加平台的人参考。

### Project Structure Notes

```
outline-cli/
  dist-workspace.toml                     # targets → 2 个 Apple；installers 去 msi；去 allow-dirty
  .github/
    workflows/
      ci.yml                              # matrix → macos；startup/contract → macos；+ windows-source-lint
      binary-size.yml                     # matrix → 两个 Apple triple
      release.yml                         # dist generate 重新生成
      release-guards.yml                  # 去掉 MSI 两步与 msi/musl 产物断言
    build-setup/
      release-build-setup.yml             # 删 musl 步骤；去掉断言的 Linux 条件
  scripts/
    check-binary-size.sh                  # 预算表 2 行，全部实测；余量说明重写
    check-release-gating.sh               # + 门禁 step 必须无条件
    check-all.sh                          # 两个 target 都测；--windows 说明改写
    check-msi-version.sh                  # 删除
    check-wxs-drift.sh                    # 删除
    install-linux-musl-toolchain.sh       # 删除
  crates/otl/
    Cargo.toml                            # 删 [package.metadata.wix]
    wix/main.wxs                          # 删除
  README.md                               # 安装/平台表/开发/CI 描述
```

## 恢复路径（把平台加回来）

这一节是删除的前提条件：**「后面需要我再加」必须是一条明确指令，而不是考古**。

### 加回 Linux（`x86_64-unknown-linux-musl`）

1. `dist-workspace.toml`：`targets` 加 `"x86_64-unknown-linux-musl"`。
2. `scripts/check-binary-size.sh`：`budget_for_target()` 加一行。**必须先实测再定预算**——`check-release-gating.sh` 会因为「published target 没有预算」直接失败，这是故意的。历史参考值（勿直接沿用，代码已变）：曾实测 4,523,120 B，预算 4,920,000。
3. 恢复 musl 交叉工具链安装：`git show 31ca4fb:scripts/install-linux-musl-toolchain.sh`（本次删除前的最后版本可用 `git show HEAD~1:scripts/install-linux-musl-toolchain.sh`），并在 `.github/build-setup/release-build-setup.yml` 里加回一个 `if: runner.os == 'Linux'` 的调用步骤。**注意**：只有工具链安装步骤可以带平台条件，门禁步骤不可以（`check-release-gating.sh` 会拒绝）。
4. `.github/workflows/binary-size.yml`：matrix 加 `ubuntu-latest` + musl target，并加回工具链安装步骤。
5. `.github/workflows/ci.yml`：matrix 加 `ubuntu-latest`。
6. `runtime-yaml-guard` 的 target 循环里加上该 triple。
7. `release-guards.yml` 的 `expected=()` 加 `"outline-cli-x86_64-unknown-linux-musl.tar.xz"`。
8. `README.md` 平台表加一行。
9. `dist generate` 重新生成 `release.yml`，提交。
10. 已知坑：musl 需要 `musl-tools cmake clang libclang-dev`（aws-lc-sys 编译 C）；dist 自带的 `[dist.dependencies.apt]` **不能用**（生成的 `apt-get install` 没有 `--yes`，Actions 的 stdin 是 /dev/null，apt 读到 EOF 直接 Abort）。

### 加回 Windows（`x86_64-pc-windows-msvc`，含 MSI）

1. `dist-workspace.toml`：`targets` 加 `"x86_64-pc-windows-msvc"`；`installers` 加 `"msi"`；加回 `allow-dirty = ["msi"]`。
2. `crates/otl/Cargo.toml` 加回（**GUID 必须逐字一致**，否则 Windows 视新版本为另一个产品）：
   ```toml
   [package.metadata.wix]
   upgrade-guid = "339F52C9-2FA2-4724-B43E-04C01774EB48"
   path-guid = "3E6CF235-552B-4E3C-A786-10EF1CD89BC0"
   license = false
   eula = false
   linker-args = ["-sice:ICE61"]
   ```
3. `dist generate` 会生成一份全新的 `crates/otl/wix/main.wxs`，然后**手工加回一个 attribute**：`MajorUpgrade` 上的 `AllowSameVersionUpgrades='yes'`。理由：cargo-wix 把 SemVer 预发布塞进 MSI ProductVersion 第四段，而 Windows Installer 只比较前三段，所以 `1.2.3-rc.1`/`1.2.3-rc.2`/`1.2.3` 在升级检测里是同一版本；不加这个 attribute，后一个 MSI 发现不了前一个，会并存或拒装。代价是同一核心版本的预发布之间失去降级保护（跨核心版本仍有保护）。上一版完整文件（含 27 行说明注释）：`git show HEAD~1:crates/otl/wix/main.wxs`。
4. 恢复两个脚本：`git show HEAD~1:scripts/check-wxs-drift.sh`、`git show HEAD~1:scripts/check-msi-version.sh`，并在 `release-guards.yml` 里加回对应两个步骤（原文见 `git show HEAD~1:.github/workflows/release-guards.yml`）。`check-msi-version.sh` 也要加回 build-setup 的断言步骤里。
5. `check-binary-size.sh` 加预算行（历史参考：曾实测 3,757,568 B，预算 4,094,000）。
6. `binary-size.yml` matrix 加 `windows-latest`；`ci.yml` matrix 加 `windows-latest`；`release-guards.yml` 的 `expected=()` 加 `.msi`；README 平台表与「Windows/MSI/SmartScreen 未签名」段落（见 `git show HEAD~1:README.md`）。
7. `windows-source-lint` job 此时变成冗余（真 Windows leg 会做同样的 lint），可删可留。
8. 已知坑：MSI 未签名会触发 SmartScreen，签名需要采购证书（dist 的 `ssldotcom-windows-sign`）。

### 两者共通

- **`release.yml` 永远只能由 `dist generate` 产生**；`release-guards.yml` 的 `dist generate --check` 会抓手改（本 story 变异验证过：手改一处 `runs-on` → 非零退出）。
- 加任何 target 都必须**先实测体积再写预算**。历史教训：musl 的预算曾用一个从未实测的「比 darwin 高 9%」系数外推，实际低估 0.74 MB，CI 一测就红。

### References

- [Source: planning/epics.md NFR2、NFR3]（NFR3「三平台一等公民」在本 story 期间**暂时不成立**，这是用户的显式决定）
- [Source: specs/spec-outline-cli/SPEC.md#Constraints]
- [Source: stories/4-5-release-pipeline.md]（发布管道全部设计与四轮对抗审查记录）

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-27

### Debug Log References

- `RUSTDOCFLAGS="-D warnings" cargo doc` 当前失败（`spec-compile` 的私有 intra-doc link），因此 coordinator 提的「用 doc 警告替代 win-check 的 lint 视角」不可行；已如实记录并把该 doc 问题作为独立发现上报。
- 第四条变异验证第一次是**假阴性**：我用 `runs-on: "macos-14"` 做替换，而生成文件里没有这个字面量（matrix 用 `${{ matrix.runner }}`），替换没生效、文件未变，`dist generate --check` 自然通过。改用确实存在的 `runs-on: "ubuntu-22.04"` 并**断言替换真的改变了文件**之后，才得到正确结果（rc=255）。教训：变异测试必须先证明自己变异了。

### Completion Notes List

- 变异验证（四条，全部实做）：
  1. 给 size 门禁 step 加 `if: runner.os == 'Linux'` → `check-release-gating.sh` 报「the step running check-binary-size.sh is unconditional」并 exit 1。
  2. 给结构性断言 step 加同样条件 → 同样被抓，exit 1。
  3. `targets` 加 `x86_64-unknown-linux-musl` 而不加预算 → 「every published target has a size budget; missing [...]」exit 1。
  4. 手改生成的 `release.yml`（`ubuntu-22.04` → `ubuntu-24.04`，并断言文件确实被改）→ `dist generate --check` 非零；未改动的对照组 rc=0。
- 门禁结果：`check-all.sh --windows` 全绿（fmt / clippy / test / doc / 两个 target 的体积 / win-check）；`dist plan`、`dist generate --check`、`check-release-gating.sh`、`check-action-pins.sh`、`verify-cargo-dist-installer.sh`、`shellcheck scripts/*.sh`、`actionlint -shellcheck=` 全部 ok（每项都在管道之前捕获退出码）。
- 实测体积：`aarch64-apple-darwin` 3,464,608 B（92% 预算 / 69% NFR2）、`x86_64-apple-darwin` 4,007,712 B（92% / 80%）。合并 agent-discovery 后已重测并更新 header（见 File List 的最终提交）。
- `dist plan` 产物由 6 个降为 4 个：`outline-cli-installer.sh`、`outline-cli.rb`、两个 `.tar.xz`（另有 source tarball 与 sha256.sum）。
- 未改动：`crates/**` 里的 `cfg(windows)` 分支、`tests/portability.rs`（用户会加回平台，删除是无谓 churn）；`authors` / LICENSE 署名（人名，非组织 handle）。
- 外部前置未变：`zhew1585/homebrew-tap` 与 `HOMEBREW_TAP_TOKEN` 仍不存在，打 tag 会在 preflight 快速失败且不发布任何东西。

### File List

- .github/workflows/ci.yml（matrix → macos；startup/contract → macos；yaml guard 显式 target；+ windows-source-lint）
- .github/workflows/binary-size.yml（matrix → 两个 Apple triple）
- .github/workflows/release.yml（dist generate 重新生成）
- .github/workflows/release-guards.yml（去 MSI 两步、去 msi/musl 产物断言）
- .github/build-setup/release-build-setup.yml（删 musl 步骤、去断言的 Linux 条件）
- dist-workspace.toml（targets、installers、去 allow-dirty、注释）
- scripts/check-binary-size.sh（预算表、header 重写）
- scripts/check-release-gating.sh（+ 门禁 step 无条件断言）
- scripts/check-all.sh（两个 target、--windows 说明）
- scripts/check-msi-version.sh（删除）
- scripts/check-wxs-drift.sh（删除）
- scripts/install-linux-musl-toolchain.sh（删除）
- crates/otl/wix/main.wxs（删除）
- crates/otl/Cargo.toml（删 [package.metadata.wix]）
- README.md（安装、平台表、开发、CI 描述）
- stories/4-7-mac-only-ci.md（本文件）
- stories/sprint-status.yaml
