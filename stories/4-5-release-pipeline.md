# Story 4.5: 发布管道

Status: done

## Story

As a 新用户,
I want brew 一条命令安装,
so that 上手零门槛 —— 打一个 tag 就自动产出三平台的 brew tap / shell 安装器 / Windows MSI，
且体积与公共契约（退出码、semver 面）在 CI 里被守住。

## Acceptance Criteria

1. **Given** cargo-dist 配置
   **When** 打 tag 发版
   **Then** 自动产出 brew tap、shell 安装器、Windows MSI
2. **Given** release 构建
   **When** CI 检查
   **Then** 二进制体积在门槛内（目标 ~5MB），README 含退出码表、semver 契约与 `otl api` 不稳定声明

## Tasks / Subtasks

- [x] Task 1: cargo-dist 配置就位 (AC: 1)
  - [x] 用 dist 0.32.0 `dist init` 生成 `dist-workspace.toml`（新式 `[dist]` 表，不落 `[workspace.metadata.dist]`）
  - [x] `installers = ["shell", "homebrew", "msi"]`；`tap = "weizhesafeheron/homebrew-tap"` + `publish-jobs = ["homebrew"]`
  - [x] `targets` 收敛为四个：aarch64/x86_64-apple-darwin、x86_64-unknown-linux-musl（静态）、x86_64-pc-windows-msvc
  - [x] `install-updater = false`、`pr-run-mode = "skip"`（NFR4 + tag-only 发布）
  - [x] 根 Cargo.toml 追加 `[profile.dist]`（纯 `inherits = "release"`，见 Dev Notes 体积说明）与 `[workspace.package]` 的 authors/homepage
  - [x] `crates/otl/Cargo.toml` 追加 repository/authors/homepage 继承 + dist 写入的 `[package.metadata.wix]` GUID
- [x] Task 2: tag 触发的 release workflow (AC: 1)
  - [x] `dist generate` 产出 `.github/workflows/release.yml`（`on: push: tags`，无 pull_request）
  - [x] `crates/otl/wix/main.wxs` 一并生成并入库（MSI 定义）
  - [x] 现有 `.github/workflows/ci.yml` 一字未动
  - [x] musl 交叉工具链经 `github-build-setup` 注入（`.github/build-setup/release-build-setup.yml` → `scripts/install-linux-musl-toolchain.sh`）
- [x] Task 3: 二进制体积门禁 (AC: 2)
  - [x] `scripts/check-binary-size.sh`：默认 `--profile dist`，上限 4 MiB，支持 `BINARY_SIZE_TARGET` 交叉编译与 Windows `.exe`，并复刻 dist 的 per-target RUSTFLAGS
  - [x] 门禁以 step 形式注入 dist 自己的 `build-local-artifacts`，失败 → 该 job failure → host skip → announce skip → 不产生 Release（R1-[1]）
  - [x] `.github/workflows/binary-size.yml` 三平台矩阵做 PR 期反馈，Linux 侧构建真正发布的 musl 目标
- [x] Task 3b: 发布前 preflight（`.github/workflows/release-guards.yml`，注册为 `plan-jobs`）
  - [x] `dist plan`、`dist generate --check`、无 updater 产物、六个必需产物齐全
  - [x] cargo-dist 安装器校验和核验后再执行（R1-[4]）
  - [x] 全部 action 必须是 40 位 SHA（R1-[3]）
  - [x] MSI 版本可表达性 + `main.wxs` 关键设置存在（R1-[5]）
  - [x] Homebrew tap 仓库与 token 可达性，tag 上为硬失败（R1-[6]）
- [x] Task 4: README 公共契约 (AC: 2)
  - [x] 安装章节：brew / shell installer / MSI / 预编译归档平台表 / 从源码构建 + 明示不检查更新
  - [x] 「Stability and versioning」章节：受 semver 约束面 vs 不受约束面，`otl api` 输出明示不稳定
  - [x] 退出码表以生成块形式内嵌，`crates/otl/tests/readme_exit_codes.rs` 断言其与 `docs/exit-codes.md` 一致
  - [x] Development 章节补 `check-binary-size.sh` 与发布流程说明
- [x] Task 5: 供应链加固（对抗审查 R1）
  - [x] `github-release = "announce"`：Release 创建挪到 announce，Homebrew 失败不再留半发布状态（R1-[6]）
  - [x] `github-attestations = true`：产物构建溯源，同时把 `build-local-artifacts` 的 token 降到 `contents: read`（R1-[7]、R1-[2]）
  - [x] `[dist.github-action-commits]` 固定四个 action 的 commit（R1-[3]）
  - [x] `allow-dirty = ["msi"]` + 手改 `main.wxs` 加 `AllowSameVersionUpgrades='yes'` + `linker-args = ["-sice:ICE61"]`（R1-[5]）
- [x] Task 6: 门禁全绿 (AC: 1, 2)
  - [x] fmt / clippy / test --workspace / check-binary-size.sh
  - [x] actionlint 1.7.12 四个 workflow 零错误；shellcheck 0.11.0 五个脚本零告警
  - [x] `dist plan`、`dist generate --check`、`dist build --artifacts=global`、`dist build --artifacts=local --target=aarch64-apple-darwin` 本地实跑
  - [x] 各门禁的反向失败路径都实测过（体积超限、README 漂移、action 未钉 SHA、MSI 版本非法）

## Dev Notes

- **`dist-workspace.toml` 而非 `[workspace.metadata.dist]`**：dist ≥0.23 的新式配置文件。副作用正好合意 —— 发布配置几乎全部落在一个新文件里，对根 `Cargo.toml` 只追加 `[profile.dist]` 与 `[workspace.package]` 两处元数据，`[workspace.dependencies]` 零改动（本 story 与另外三条 track 并行，那一段是冲突热点）。
- **`[profile.dist]` 必须去掉 dist 默认的 `lto = "thin"`**：`inherits = "release"` 之后再写 `lto = "thin"` 会把 `[profile.release]` 的 fat LTO 降级。实测 aarch64-apple-darwin 上 thin LTO 3_290_656 B vs fat LTO 2_567_312 B —— 白涨 0.69 MB。NFR2 比 release 构建墙钟时间重要，故 `[profile.dist]` 只留 `inherits`，发布产物与 `--release` 逐字节一致。
- **体积门槛 4 MiB 的依据**：实测 `--profile dist` 二进制 2_567_312 B（≈2.45 MiB）。NFR2 的 ~5MB 是对用户的承诺上限，不是有用的回归门 —— 从 2.5MB 涨到 5MB 时损害早已合并。4 MiB 对最大产物留约 40% 余量（musl 静态链接 libc 是四个目标里最大的），既不会被日常代码增长和工具链漂移打扰，又能拦住这个门真正要拦的东西：一个 YAML 解析器 / TUI 栈 / async runtime 悄悄进入发布二进制。阈值与依据都写在脚本头部，抬高阈值要求同时更新实测值。
- **musl + aws-lc-sys 是本 story 最脆的一环**：reqwest 0.13 的 `rustls` feature 走 aws-lc-rs → aws-lc-sys，那是 C 代码。从 glibc 宿主构建 musl 目标需要 musl-gcc（`musl-tools`）、cmake，以及 bindgen 兜底时的 libclang。dist 自带的 `[dist.dependencies.apt]` **不能用**：它生成的是 `sudo apt-get install <pkgs>`，没有 `--yes`，而 GitHub Actions 的 `run:` 步骤 stdin 挂在 /dev/null，apt 的确认提示读到 EOF 会直接 Abort。因此改为 `github-build-setup` 注入一个手写步骤，调用 `scripts/install-linux-musl-toolchain.sh`；包清单只在脚本里出现一次，release 构建与 `binary-size.yml` 的 musl 预检共用它，两条路径不可能漂移。
- **`github-build-setup` 片段放在 `.github/build-setup/` 而不是 `.github/workflows/`**：GitHub 会把 workflows 目录里每个 .yml 都当 workflow 解析，一个裸 step 列表会被报成 invalid workflow file。dist 把配置里的路径按 `.github/workflows/` 解析，所以配置值是 `"../build-setup/release-build-setup.yml"`。
- **release.yml 是生成物，禁止手改**：它被 `dist generate --check` 逐字节校验，往里插一个 step 会立刻让校验失败。所以一切自定义能力都必须走配置项：`github-build-setup` 往构建 job 注入 step，`plan-jobs` 注册 preflight，`github-action-commits` 钉 action，`github-release` 决定 Release 在哪一步创建。
- **「门禁必须真的拦住发布」是本 story 最容易做假的一条**。同一个 tag 触发的旁路 workflow 与 release.yml **并行**，只能把自己标红。连 `plan-jobs` 自定义 job 也不够：它失败时构建 job 是 *skipped*，而 dist 的 `host` job 判据是 `always() && ... (result == 'skipped' || result == 'success')`，skipped 一样放行。唯一可靠的杠杆是让 **构建 job 本身 failure**，因为 failure 既不是 skipped 也不是 success。因此体积门禁以 step 形式注入 `build-local-artifacts`：
  ```
  size step fail -> build-local-artifacts = failure
                 -> host      skipped（if 要求 skipped-or-success）
                 -> announce  skipped（if 要求 host == 'success'）
                 -> GitHub Release 从不创建（github-release = "announce"）
  ```
  顺带的好处：它编译的是同一 job 里 `dist build` 稍后要编译的同一 package/profile/target/RUSTFLAGS，所以 dist 直接复用这次编译，几乎不额外花时间；而且四个发布目标全覆盖（含本地无法验证的 x86_64-apple-darwin 交叉构建）。
- **`check-binary-size.sh` 必须复刻 dist 的 per-target RUSTFLAGS**（msvc 加 `+crt-static`，musl 加 `+crt-static -Clink-self-contained=yes`，见 cargo-dist 0.32.0 `src/build/cargo.rs`）。不复刻的话量到的不是发布出去的那个二进制，而且 cargo 会因为 flag 不同整个重编译，"复用编译"的好处也没了。
- **`github-release = "announce"` 是顺序修复**：默认 `auto` 会在 `host` 阶段就创建并公开 Release，之后 Homebrew publish 才跑；tap 仓库不存在、token 过期时就留下一个「Release 已公开但 README 宣传的 brew 通道装不了」的半发布状态。挪到 `announce` 后，`announce` 的 `if` 要求 host 成功 **且** homebrew publish 成功或跳过，失败即无 Release。
- **workflow 级 `permissions: contents: write` 无法通过配置降级**（cargo-dist 0.32.0 `backend/ci/github.rs` 里 `root_permissions` 是硬编码的）。能做到的最小权限是 `github-attestations = true`：它让 dist 给 `build-local-artifacts` 显式写上 `{attestations: write, contents: read, id-token: write}`，也就是把可写 token 从**唯一会执行依赖 build script 的 job** 里拿掉。其余持写权限的 job（plan / build-global / host / publish / announce）都只跑 dist 自己的代码，不编译 crate 图。这一条只能缓解不能根治，已如实记录。
- **cargo-dist 自身的 `curl | sh` 安装**（上游 issue #2420）没有配置项可以钉校验和。做法是 `scripts/verify-cargo-dist-installer.sh` 在 preflight 里先按 `scripts/cargo-dist-installers.sha256` 核验、再执行核验过的那份字节。这条链是完整的：安装器脚本内嵌了它会下载的每个 dist 二进制 tarball 的 SHA-256（已核对与上游 `sha256.sum` 逐字节一致），所以钉住脚本哈希等于钉住二进制。诚实边界：preflight 是 plan 阶段 job，dist 自己的 `plan` job 与它**并行**，被换掉的安装器可能已经在那一个 job 里跑过 —— 对 `plan` 是检测，对**发布**是阻断（后续所有 job 都 needs 它）。
- **退出码表的单一来源**：`docs/exit-codes.md` 是 source of truth（另外三条 track 正在各自往里追加新码），README 里那份是派生块，夹在 `<!-- BEGIN/END GENERATED EXIT CODES -->` 之间，由 `crates/otl/tests/readme_exit_codes.rs` 断言一致。这是有意的「会失败」设计：谁往 doc 里加了新码，测试就红，直到 README 也补上。为了让修复零思考成本，测试支持 `UPDATE_README_EXIT_CODES=1` 自动重写 README 块（trybuild/insta 的套路）。测试同时断言码值唯一且升序。
- **semver 契约的分界**（NFR5，README「Stability and versioning」）：受约束 = 精选命令的名字/flag/输出形状（人类可读与 `--json` 两者）、退出码表、env 变量与配置/凭证文件的位置与键名；不受约束 = `otl api` 输出（它印的是服务器的契约，会随 Outline 实例、vendored spec、用户本地 `spec sync` 变化）、stderr 诊断措辞、通用表格渲染器的选列与排版、缓存格式。0.x 期间明示「意图而非保证」。
- **NFR4 有了机器检查**：`install-updater = false` 不是注释里的君子协定 —— release-guards 断言 dist plan 里不出现任何 updater 产物，把这个选项被悄悄打开的可能性变成一次 CI 红灯。另外本地验证过生成的 shell installer：虽然脚本里 `INSTALL_UPDATER=1` 是默认值，但所有 target 分支的 `_updater_name` 都是空串且下载被 `[ -n "$_updater_name" ]` 守着，因此永远不会装 updater。
- **`repository` / `authors` / `homepage` 是 dist 的硬需求**，不是可选润色：缺 repository 时 GitHub CI backend 直接拒绝生成（安装器与 formula 要拼 release 产物 URL）；缺 authors 时 MSI 的 Manufacturer 字段无从填写，`main.wxs` 生成失败；缺 homepage 时 homebrew publish job 只是告警。`authors` 里故意不放邮箱 —— 那串字符会原样嵌进发布出去的安装器元数据。
- **MSI 的预发布升级语义**（R1-[5]）：cargo-wix 0.3.9 把 SemVer 预发布塞进 MSI ProductVersion 的第四段（`src/create.rs::version`），而 Windows Installer **只比较前三段**。于是 1.2.3-rc.1 / 1.2.3-rc.2 / 1.2.3 在升级检测里是同一个版本，默认 `AllowSameVersionUpgrades='no'` 让新包发现不了旧包 —— 并存或拒装。修法是手改 `main.wxs` 加 `AllowSameVersionUpgrades='yes'`，并用 `allow-dirty = ["msi"]` 让 dist 不再覆盖/比对该文件；代价是包元数据（description 等）不再自动同步进 wxs，因此 preflight 反过来断言 wxs 里那几个关键设置还在。ICE61 是该设置引发的**警告**（cargo-wix 不传 `-wx`），仍用 `[package.metadata.wix] linker-args = ["-sice:ICE61"]` 显式静音，免得真的链接问题被淹没。同一函数还揭示第二个坑：预发布不含数字（`1.2.3-alpha`）时 cargo-wix 直接报错，`scripts/check-msi-version.sh` 在 plan 阶段就以同样规则拦下并给出改名建议 —— 该脚本的规则在本地对 10 组版本号实测过。
- **本地无法验证的部分**（详见 Completion Notes）：musl 实际构建、MSI 实际产出、homebrew tap 推送。

### Project Structure Notes

```
outline-cli/
  dist-workspace.toml                     # 新增：唯一的发布渠道描述
  Cargo.toml                              # 追加 [profile.dist] + workspace.package 元数据
  .github/
    workflows/
      ci.yml                              # 未改动
      release.yml                         # 新增：dist generate 生成物，禁止手改
      release-guards.yml                  # 新增：preflight，注册为 dist plan-jobs
      binary-size.yml                     # 新增：PR 期体积/musl 反馈（非发布门禁）
    build-setup/
      release-build-setup.yml             # 新增：注入 build-local-artifacts 的 step 片段
  scripts/
    check-binary-size.sh                  # 新增：体积门禁（发布门禁的实现）
    install-linux-musl-toolchain.sh       # 新增：musl 工具链清单单一来源
    verify-cargo-dist-installer.sh        # 新增：cargo-dist 安装器校验和核验
    cargo-dist-installers.sha256          # 新增：上述校验和的钉子
    check-action-pins.sh                  # 新增：所有 action 必须是 commit SHA
    check-msi-version.sh                  # 新增：MSI 版本可表达性
  crates/otl/
    Cargo.toml                            # 追加 repository/authors/homepage + [package.metadata.wix]
    wix/main.wxs                          # 新增：起于 dist 生成物，现为手工维护
    tests/readme_exit_codes.rs            # 新增：README 退出码表 ↔ docs 一致性测试
  README.md                               # Install / Stability and versioning / 退出码表 / Development
```

### References

- [Source: planning/epics.md#Story 4.5、NFR2、NFR3、NFR4、NFR5]
- [Source: specs/spec-outline-cli/stack.md#分发]
- [Source: specs/spec-outline-cli/SPEC.md#Constraints]
- [Source: docs/exit-codes.md]
- [Source: project-context.md 全文（CLI 行为契约、禁止 phone home、Windows 显式分支、硬编码提常量）]
- cargo-dist 0.32.0: https://axodotdev.github.io/cargo-dist

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-26

### Debug Log References

- `dist init --yes` 首轮失败两次，都是包元数据缺失：先是 `outline-cli` 没有 `repository`（GitHub CI backend 硬需求），补上后是没有 `authors`（MSI 的 `main.wxs` 生成硬需求）。两次都补在 `[workspace.package]` 并在包里继承。
- `dist init --yes -t <四个 target>` 会把自己的默认 target 与传入的合并（结果六个），必须回头手工收敛 `targets` 并改用 `dist generate` 重新生成。
- 改了 `github-build-setup` 片段后 `dist plan` 直接以退出码 255 失败并给出 release.yml 的 diff —— 即 `dist plan` 本身已隐含同步校验，`dist generate --check` 只是给出更清楚的错误。
- `dist init` 默认写的 `[profile.dist] lto = "thin"` 让二进制从 2.45 MiB 涨到 3.13 MiB；发现于体积门禁首跑（78% 预算），改掉后回到 61%。
- 对抗审查 R1 后的关键实证（都是读 cargo-dist 0.32.0 源码 + 实跑确认，不是推测）：
  - `root_permissions` 在 `backend/ci/github.rs:300` 硬编码为 `contents: write`，没有配置项；`github-custom-job-permissions` 只作用于自定义 job。
  - `host` 的 `if` 用 `always() && (result == 'skipped' || 'success')`，所以自定义 `plan-jobs` 失败（导致构建 skipped）**不**阻断 host；`host-jobs` 失败也不阻断 `announce`（它同样 `always()`）。真正的杠杆只有让构建 job 变成 failure，以及 `announce` 的 `needs.host.result == 'success'`。
  - `allow-dirty = ["msi"]` 走 `DirtyMode::AllowList` → `should_run(Msi) == false`，`dist generate` 与 `--check` 都完全跳过 wxs；实跑确认手改的 `main.wxs` 不再被覆盖且 `--check` 通过。
  - cargo-dist 安装器脚本内嵌的 tarball SHA-256 与上游 `sha256.sum` 逐字节一致（实测比对），所以钉脚本哈希即钉二进制。
  - `github-build-setup` 只注入 `build-local-artifacts`（模板里唯一的 `for step in github_build_setup`），不会误伤 global 构建 job。

### Completion Notes List

- **只能在 CI / 其他平台验证的部分**（本地一律未声称通过）：
  1. `x86_64-unknown-linux-musl` 的实际构建。本机是 macOS 且没有 musl 交叉工具链，Docker daemon 未运行（不主动替用户启动 Docker Desktop）。风险集中在 aws-lc-sys 的 C 编译；缓解手段有两层：`binary-size.yml` 每次 push/PR 都构建 musl（第一次 CI 运行就会暴露，而不是留到打 tag），且发布路径上的体积门禁也在 musl 构建 job 里跑同一条命令。
  2. MSI 的实际产出（wix 只在 Windows 跑）。已验证的是 `main.wxs` 能生成、`dist plan` 把 `.msi` 列入产物清单。
  3. homebrew tap 推送。需要仓库外的两个前提：`weizhesafeheron/homebrew-tap` 仓库存在，以及 `HOMEBREW_TAP_TOKEN` secret。缺任一都会在 publish 步骤失败，但 `.rb` 仍会作为 Release 产物附上。formula 内容已本地生成并肉眼核对（三个 URL 指向正确 triple）。
  4. `x86_64-apple-darwin` 交叉构建（本机 arm64，未装该 target）。
- 本地实跑通过的部分：`dist plan`、`dist generate --check`、`dist build --artifacts=global`（source.tar.gz / installer.sh / outline-cli.rb / sha256.sum 全部产出）、`dist build --artifacts=local --target=aarch64-apple-darwin`（tar.xz + 校验和产出）。
- 故意留下的缺口：
  - **不生成 PowerShell 安装器**。AC1 与 stack.md 只要求 shell + brew + MSI，Windows 路径由 MSI 覆盖。加 `powershell` installer 是超范围。
  - **不做 crates.io publish job**。本 story 的 AC 只涉及三个分发渠道；`cargo publish` 需要 CRATES_IO_TOKEN 与版本号策略，属于另一个决定。
  - **不做 aarch64-unknown-linux-* 与 aarch64-pc-windows-msvc**。NFR3 说的三平台一等公民已由四个 triple 覆盖；`dist init` 默认还想加 aarch64 linux，被有意去掉以免为无人验证的目标背发布责任。
  - **`otl` 自身不提供 `--version` 之外的升级路径**。这是 NFR4 的直接后果，不是遗漏。
  - **MSI 不做代码签名**。需要采购证书（EV/OV），仓库里拿不到；SmartScreen 会告警。已在 README 明写，并给出 `gh attestation verify` 作为替代验证手段。dist 的 `ssldotcom-windows-sign` 配好证书后即可开启，配置位置留在同一处。
  - **workflow 级 `contents: write` 未能降级**。cargo-dist 0.32.0 把它硬编码在模板里；已把可写 token 从编译 job 移除（见 Dev Notes），其余持写权限的 job 不执行第三方构建脚本。要彻底解决需要上游改动。
  - **cargo-dist 的 `curl | sh` 未能改成校验后执行**：生成文件不可手改、上游无配置项（#2420）。已做等价强度的带外校验 + 发布阻断，边界在 Dev Notes 里写清了。
- 越界改动与理由（`crates/**` 与根 `Cargo.toml` 本应只追加）：
  - `crates/otl/Cargo.toml`：`repository`/`authors`/`homepage` 三行继承 + dist 自己写入的 `[package.metadata.wix]`。前者是 dist 生成 CI 与 MSI 的硬前置，后者是 MSI 的升级 GUID（必须入库，否则每次发版都换 GUID，Windows 会把新版本当成另一个产品而不是升级）。都在 `[package]` 段，与依赖段无交集。
  - 根 `Cargo.toml`：`[workspace.package]` 追加 authors/homepage，末尾追加 `[profile.dist]`。`[workspace.dependencies]` 零改动。
  - `crates/otl/tests/readme_exit_codes.rs`、`crates/otl/wix/main.wxs`：全新文件，不改动任何既有源码。
- 未改动 `docs/exit-codes.md`（另外三条 track 正在各自追加新码），也未改动 `stories/sprint-status.yaml`（合并时由 orchestrator 统一更新）。
- 门禁结果：`cargo fmt --all -- --check` 通过；`cargo clippy --all-targets --all-features -- -D warnings` 零告警；`cargo test --workspace` 全通过；`bash scripts/check-binary-size.sh` 2_567_312 B / 4_194_304 B（61%）通过；反向验证过门禁真会红（`MAX_BINARY_SIZE_BYTES=1000000` → 退出码 1）。README 一致性测试同样反向验证：删掉一行 → 测试红并打印期望块，`UPDATE_README_EXIT_CODES=1` 重跑 → 文件被逐字节还原。
- actionlint 1.7.12 对四个 workflow 零错误。带 shellcheck 0.11.0 复跑时唯一的告警全部落在 **生成的** `release.yml` 里（cargo-dist 上游模板的 SC2086/SC2129/SC2001，info/style 级），本 story 的 workflow 与五个脚本零告警。生成物不能手改，故不处理。
- 反向失败路径逐条实测：体积超限（`MAX_BINARY_SIZE_BYTES=1000000` → 1）、README 退出码漂移（红 → `UPDATE_README_EXIT_CODES=1` 逐字节还原）、未钉 SHA 的 action（构造样例 → 1）、非法 MSI 版本（`1.2.3-alpha` / `256.0.0` / `1.2.3-rc.70000` → 1，`1.2.3-rc.4` / `1.2.3-4` / `1.0.0+9` → 0）、安装器校验和（实跑核验两个安装器均匹配）。

### File List

- dist-workspace.toml（新增）
- .github/workflows/release.yml（新增，dist 生成物）
- .github/workflows/release-guards.yml（新增，reusable + dist plan-jobs）
- .github/workflows/binary-size.yml（新增）
- .github/build-setup/release-build-setup.yml（新增）
- scripts/check-binary-size.sh（新增）
- scripts/install-linux-musl-toolchain.sh（新增）
- scripts/verify-cargo-dist-installer.sh（新增）
- scripts/cargo-dist-installers.sha256（新增）
- scripts/check-action-pins.sh（新增）
- scripts/check-msi-version.sh（新增）
- crates/otl/tests/readme_exit_codes.rs（新增）
- crates/otl/wix/main.wxs（新增，起于 dist 生成物，现为手工维护）
- Cargo.toml（追加 [profile.dist]、workspace.package authors/homepage）
- crates/otl/Cargo.toml（追加 repository/authors/homepage 继承、[package.metadata.wix]）
- README.md（Install、Stability and versioning、退出码表、Development）
- stories/4-5-release-pipeline.md（本文件）
