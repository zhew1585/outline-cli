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
  - [x] `installers = ["shell", "homebrew", "msi"]`；`tap = "zhew1585/homebrew-tap"` + `publish-jobs = ["homebrew"]`
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
- [x] Task 3b: 发布 preflight（`.github/workflows/release-guards.yml`，注册为 `local-artifacts-jobs`）
  - [x] `dist plan`、`dist generate --check`、无 updater 产物、六个必需产物齐全
  - [x] cargo-dist 安装器校验和核验后再执行（R1-[4]）
  - [x] 全部 action 必须是 40 位 SHA（R1-[3]）
  - [x] MSI 版本可表达性 + `main.wxs` 关键设置存在（R1-[5]）
  - [x] Homebrew tap 仓库与 token 可达性，tag 上为硬失败、PR 上完全不注入 token（R1-[6]、R2-[5]）
  - [x] `scripts/check-release-gating.sh` 断言整条 needs/if 链真的能拦住发布（R2-[1]、R2-[2]）
  - [x] `scripts/check-wxs-drift.sh` 去掉 allow-dirty 重生成后逐行 diff，只放行既定 delta（R2 方向 3）
- [x] Task 5b: R2 收尾
  - [x] preflight 从 `plan-jobs` 改为 `local-artifacts-jobs`（R2-[1]，唯一能拦住 host 的自定义 job 位置）
  - [x] 廉价结构断言额外注入 `build-local-artifacts`（Linux leg），冗余地挂在能报 failure 的杠杆上
  - [x] 给全局产物补 attestation（R2-[3]），载体在 R3 后改为 `post-announce-jobs`（见 R3-[1]）
  - [x] 体积门禁加 85% 警戒带 + 依据改用实测的分支增量（R2-[4]）
  - [x] `check-action-pins.sh` 扫 `.github` 全树、含 `.yaml`（R2-[7]）
  - [x] `install-linux-musl-toolchain.sh` 注释路径纠正（R2-[6]）
  - [x] `binary-size.yml` 显式指定三个 triple，与发布路径量同一个东西
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
  - [x] `actionlint -shellcheck=` 全部 workflow 零错误（默认带 shellcheck 集成时报 6 项 info/style，**全部在不可手改的生成文件 `release.yml` 里**，见 Completion Notes）；shellcheck 0.11.0 所有脚本零告警
  - [x] `dist plan`、`dist generate --check`、`dist build --artifacts=global`、`dist build --artifacts=local --target=aarch64-apple-darwin` 本地实跑
  - [x] 各门禁的反向失败路径都实测过（体积超限、README 漂移、action 未钉 SHA、MSI 版本非法）

## Dev Notes

- **`dist-workspace.toml` 而非 `[workspace.metadata.dist]`**：dist ≥0.23 的新式配置文件。副作用正好合意 —— 发布配置几乎全部落在一个新文件里，对根 `Cargo.toml` 只追加 `[profile.dist]` 与 `[workspace.package]` 两处元数据，`[workspace.dependencies]` 零改动（本 story 与另外三条 track 并行，那一段是冲突热点）。
- **`[profile.dist]` 必须去掉 dist 默认的 `lto = "thin"`**：`inherits = "release"` 之后再写 `lto = "thin"` 会把 `[profile.release]` 的 fat LTO 降级。实测 aarch64-apple-darwin 上 thin LTO 3_290_656 B vs fat LTO 2_567_312 B —— 白涨 0.69 MB。NFR2 比 release 构建墙钟时间重要，故 `[profile.dist]` 只留 `inherits`，发布产物与 `--release` 逐字节一致。
- **体积门槛 4 MiB 的依据（R2-[4] 后修正）**：本分支实测 2_567_312 B（≈2.45 MiB，占 61%），但那是一条**只有发布配置、零功能代码**的分支——原来写的「约 40% 余量」是拿这个数说的，对合并后的产物不成立。R2 逐分支实测的增量：auth +317KB、commands +283KB、config +233KB、specsync +116KB；加性最坏估计合并后 darwin ≈3.35 MiB，musl 再高约 9% ≈**3.66 MiB ≈ 预算的 91%**。
  **合并前实测更新（2026-08）**：`develop` 已经并入 config/completions track，实测 release 二进制 **2_800_112 B（66% 预算）**——与 R2 单独测 `feat/epic4-config` 的数字一字不差，说明那条 track 的内容就是当前 develop。剩余待并三条（auth/commands/specsync）加性最坏估计 ≈3.35 MiB darwin（84%）、musl ≈3.66 MiB（91%）。脚本头部已换成这组数字。
  **结论：维持 4 MiB，不上调。** 理由：NFR2 的承诺是 5MB≈4.77 MiB，只比预估合并值高 14%，把门设在承诺线上等于什么都不管；4 MiB 是「仍然有意义」的最大值。为了让 9% 余量可管理而不是悬崖，脚本新增 **85% 警戒带**：越过就打 `::warning::` 但仍然通过，让「什么变大了」这个问题在变红之前就被提出来，而不是让维护者面对一个红叉和一个显然可以调高的常量。脚本头部的实测值已全部换成上面这组真实数字，并写明合并后必须在 develop 上重测四个目标。
- **musl + aws-lc-sys 是本 story 最脆的一环**：reqwest 0.13 的 `rustls` feature 走 aws-lc-rs → aws-lc-sys，那是 C 代码。从 glibc 宿主构建 musl 目标需要 musl-gcc（`musl-tools`）、cmake，以及 bindgen 兜底时的 libclang。dist 自带的 `[dist.dependencies.apt]` **不能用**：它生成的是 `sudo apt-get install <pkgs>`，没有 `--yes`，而 GitHub Actions 的 `run:` 步骤 stdin 挂在 /dev/null，apt 的确认提示读到 EOF 会直接 Abort。因此改为 `github-build-setup` 注入一个手写步骤，调用 `scripts/install-linux-musl-toolchain.sh`；包清单只在脚本里出现一次，release 构建与 `binary-size.yml` 的 musl 预检共用它，两条路径不可能漂移。
- **`github-build-setup` 片段放在 `.github/build-setup/` 而不是 `.github/workflows/`**：GitHub 会把 workflows 目录里每个 .yml 都当 workflow 解析，一个裸 step 列表会被报成 invalid workflow file。dist 把配置里的路径按 `.github/workflows/` 解析，所以配置值是 `"../build-setup/release-build-setup.yml"`。
- **release.yml 是生成物，禁止手改**：它被 `dist generate --check` 逐字节校验，往里插一个 step 会立刻让校验失败。所以一切自定义能力都必须走配置项：`github-build-setup` 往构建 job 注入 step，`local-artifacts-jobs` 注册 preflight，`post-announce-jobs` 挂 attestation，`github-action-commits` 钉 action，`github-release` 决定 Release 在哪一步创建。
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
- **preflight 必须注册为 `local-artifacts-jobs`，不能是 `plan-jobs`**（R2-[1]，我在 R1 犯的错）。我在 R1 为体积门禁写下的推理——「plan job 失败时构建是 *skipped*，而 `host` 的 `if` 是 `always() && (skipped || success)`，skipped 会放行」——完全适用于 preflight 自身，但我当时没有把它用在自己身上，于是三份文档一起宣称了一个假的保证。逐条核对 dist 0.32.0 模板后的结论：
  - `plan-jobs`：只有 `build-local-artifacts` needs 它 → 失败即 skip → host 放行。**不拦。**
  - `host-jobs`：**拦 publish，不拦 announce**。`announce` 确实 needs 它却用 `always()` 且不读它的 result；但 `publish-homebrew-formula` 也 needs 它（在 `partials/publish_homebrew.yml.j2` 里，不在 `release.yml.j2` 里），且它的 `if` 没有 `always()` —— 所以 host-job 失败会 skip formula 推送，而 announce 把 skipped 的 publish 当成成功继续发 Release。**这正是 R3-[1]，我在 R2 只 grep 了 release.yml.j2 一个文件就下了「不拦任何东西」的结论。**
  - `global-artifacts-jobs`：同 local，`host` 的 `needs` 与 `if` 都含它 → **拦得住 host**。
  - `user-publish-jobs`（`publish-jobs = [..., "./x"]`）：`announce` 的 `needs` 与 `if` 都含它 → **拦得住 announce**。
  - `post-announce-jobs`：**没有任何 job needs 它** → 拦不住任何东西。
  - `local-artifacts-jobs`：`host` 的 `needs` 含它，且 `if` 对它要求 `skipped || success` → **failure 拦得住**。且它只 `needs: [plan]`，起跑时机与 plan-jobs 相同，不牺牲 fail-fast。
  这张表是把 `plan_jobs|local_artifacts_jobs|global_artifacts_jobs|host_jobs|user_publish_jobs|post_announce_jobs` 六个变量在 `templates/ci/github/` **整个目录**（含 `partials/`）里 grep 出全部消费点得到的。只 grep 主模板会漏掉 partials —— R3-[1] 就是这么产生的。
  真实后果（R2 描述准确）：改之前，稳定 tag 上 preflight 失败仍会让 `publish-homebrew-formula` 带着 tap 写权限 PAT 去检出外部仓库并跑第三方 Ruby；预发布 tag 上 `announce` 会直接运行，只靠 `gh release create` 对空目录报错兜底。
- **`scripts/check-release-gating.sh` 是这次的真正教训**：链条不能靠读文档相信，要断言。它对生成的 `release.yml` 逐条断言 needs/if 语义（preflight 失败拦 host、构建失败拦 host、体积门禁在构建 job 里、编译 job 是 `contents: read`、Release 建在 announce 而非 host），并断言 `dist-workspace.toml` 没打开 `msvc-crt-static` / `cargo-auditable`（这两项会让体积门禁量到的东西与实际发布物不同）。R3 后又补了反方向的断言：**任何名字含 attest 的 job 都不得出现在 `host` / `publish-*` / `announce` 的 `needs` 里**，且它必须 `needs: announce`。这条是「additive job 不得阻塞发布」的机器化，比记住哪个 slot 安全更可靠。四条反向路径实测：改回 `plan-jobs` → 报 2 条失败；`github-attestations-phase = "host"` → 报出编译 job 的可写 token 回来了；attest job 改回 `host-jobs` → 报 3 条（publish 与 announce 都依赖它、且它不在 announce 之后）；attest job 改成 `publish-jobs` → 报 2 条。
- **安装器分发面的精确图景（R4-[1] 订正）**：之前把 `build-global-artifacts` 说成「独立下载安装器」是错的，又漏了 `host`。对着生成的 `release.yml` 逐 job 核实后的实况：**独立下载并 pipe** 的只有 `plan`（`curl | sh`）与 `build-local-artifacts` 的每个 leg（Windows 上是 `irm | iex`）；`build-global-artifacts` 与 `host` 走 `download-artifact` + `chmod` 复用 `plan` 装好的那份缓存二进制；`publish-homebrew-formula`、`announce` 与两个自定义 job 根本不装 dist。所以独立下载面是 2 类 job，缓存传播面是另外 2 个。安全结论不变（无法阻止已执行、只能阻断发布），改的是精度。
- **cargo-dist 自身的 `curl | sh` 安装**（上游 issue #2420）没有配置项可以钉校验和。做法是 `scripts/verify-cargo-dist-installer.sh` 在 preflight 里先按 `scripts/cargo-dist-installers.sha256` 核验、再执行核验过的那份字节。这条链是完整的：安装器脚本内嵌了它会下载的每个 dist 二进制 tarball 的 SHA-256（已核对与上游 `sha256.sum` 逐字节一致），所以钉住脚本哈希等于钉住二进制。诚实边界（R2 补充了两条，都属实）：(a) preflight 与各发布 job **各自独立下载**安装器，preflight 证明的是它自己那一刻的字节，存在 TOCTOU；(b) Windows 构建 job 走的是无校验的 `irm ... .ps1 | iex`，`.ps1` 的哈希虽被钉住，钉扎只在 preflight 这个 Linux job 里发挥作用。因此这是**发布阻断**（校验失败 → preflight failure → host 被拦），不是执行阻断。
- **退出码表的单一来源**：`docs/exit-codes.md` 是 source of truth（另外三条 track 正在各自往里追加新码），README 里那份是派生块，夹在 `<!-- BEGIN/END GENERATED EXIT CODES -->` 之间，由 `crates/otl/tests/readme_exit_codes.rs` 断言一致。这是有意的「会失败」设计：谁往 doc 里加了新码，测试就红，直到 README 也补上。为了让修复零思考成本，测试支持 `UPDATE_README_EXIT_CODES=1` 自动重写 README 块（trybuild/insta 的套路）。测试同时断言码值唯一且升序。
- **semver 契约的分界**（NFR5，README「Stability and versioning」）：受约束 = 精选命令的名字/flag/输出形状（人类可读与 `--json` 两者）、退出码表、env 变量与配置/凭证文件的位置与键名；不受约束 = `otl api` 输出（它印的是服务器的契约，会随 Outline 实例、vendored spec、用户本地 `spec sync` 变化）、stderr 诊断措辞、通用表格渲染器的选列与排版、缓存格式。0.x 期间明示「意图而非保证」。
- **NFR4 有了机器检查**：`install-updater = false` 不是注释里的君子协定 —— release-guards 断言 dist plan 里不出现任何 updater 产物，把这个选项被悄悄打开的可能性变成一次 CI 红灯。另外本地验证过生成的 shell installer：虽然脚本里 `INSTALL_UPDATER=1` 是默认值，但所有 target 分支的 `_updater_name` 都是空串且下载被 `[ -n "$_updater_name" ]` 守着，因此永远不会装 updater。
- **`repository` / `authors` / `homepage` 是 dist 的硬需求**，不是可选润色：缺 repository 时 GitHub CI backend 直接拒绝生成（安装器与 formula 要拼 release 产物 URL）；缺 authors 时 MSI 的 Manufacturer 字段无从填写，`main.wxs` 生成失败；缺 homepage 时 homebrew publish job 只是告警。`authors` 里故意不放邮箱 —— 那串字符会原样嵌进发布出去的安装器元数据。
- **MSI 的预发布升级语义**（R1-[5]）：cargo-wix 0.3.9 把 SemVer 预发布塞进 MSI ProductVersion 的第四段（`src/create.rs::version`），而 Windows Installer **只比较前三段**。于是 1.2.3-rc.1 / 1.2.3-rc.2 / 1.2.3 在升级检测里是同一个版本，默认 `AllowSameVersionUpgrades='no'` 让新包发现不了旧包 —— 并存或拒装。修法是手改 `main.wxs` 加 `AllowSameVersionUpgrades='yes'`，并用 `allow-dirty = ["msi"]` 让 dist 不再覆盖/比对该文件；代价是包元数据（description 等）不再自动同步进 wxs，因此 preflight 反过来断言 wxs 里那几个关键设置还在。ICE61 是该设置引发的**警告**（cargo-wix 不传 `-wx`），仍用 `[package.metadata.wix] linker-args = ["-sice:ICE61"]` 显式静音，免得真的链接问题被淹没。同一函数还揭示第二个坑：预发布不含数字（`1.2.3-alpha`）时 cargo-wix 直接报错，`scripts/check-msi-version.sh` 在构建开始前就以同样规则拦下并给出改名建议 —— 该脚本的规则在本地对 10 组版本号实测过。
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
      release-guards.yml                  # 新增：preflight，注册为 dist local-artifacts-jobs
      binary-size.yml                     # 新增：PR 期体积/musl 反馈（非发布门禁）
      attest-global-artifacts.yml         # 新增：全局产物 attestation（post-announce-jobs）
    build-setup/
      release-build-setup.yml             # 新增：注入 build-local-artifacts 的 step 片段
  scripts/
    check-binary-size.sh                  # 新增：体积门禁（发布门禁的实现）
    install-linux-musl-toolchain.sh       # 新增：musl 工具链清单单一来源
    verify-cargo-dist-installer.sh        # 新增：cargo-dist 安装器校验和核验
    cargo-dist-installers.sha256          # 新增：上述校验和的钉子
    check-action-pins.sh                  # 新增：所有 action 必须是 commit SHA
    check-msi-version.sh                  # 新增：MSI 版本可表达性
    check-release-gating.sh               # 新增：断言 needs/if 链真的能拦住发布
    check-wxs-drift.sh                    # 新增：main.wxs 与 dist 模板的受控 diff
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
  - `host` 的 `if` 用 `always() && (result == 'skipped' || 'success')`，所以自定义 `plan-jobs` 失败（导致构建 skipped）**不**阻断 host。`host-jobs` 失败不阻断 `announce`（它用 `always()` 且不读其 result），但**会**阻断 `publish-homebrew-formula`（partials 里 needs 它、`if` 无 `always()`）—— 见 R3-[1]。
  - `allow-dirty = ["msi"]` 走 `DirtyMode::AllowList` → `should_run(Msi) == false`，`dist generate` 与 `--check` 都完全跳过 wxs；实跑确认手改的 `main.wxs` 不再被覆盖且 `--check` 通过。
  - cargo-dist 安装器脚本内嵌的 tarball SHA-256 与上游 `sha256.sum` 逐字节一致（实测比对），所以钉脚本哈希即钉二进制。
  - `github-build-setup` 只注入 `build-local-artifacts`（模板里唯一的 `for step in github_build_setup`），不会误伤 global 构建 job。

### Completion Notes List

- **只能在 CI / 其他平台验证的部分**（本地一律未声称通过）：
  1. `x86_64-unknown-linux-musl` 的实际构建。本机是 macOS 且没有 musl 交叉工具链，Docker daemon 未运行（不主动替用户启动 Docker Desktop）。风险集中在 aws-lc-sys 的 C 编译；缓解手段有两层：`binary-size.yml` 每次 push/PR 都构建 musl（第一次 CI 运行就会暴露，而不是留到打 tag），且发布路径上的体积门禁也在 musl 构建 job 里跑同一条命令。
  2. MSI 的实际产出（wix 只在 Windows 跑）。已验证的是 `main.wxs` 能生成、`dist plan` 把 `.msi` 列入产物清单。
  3. homebrew tap 推送。需要仓库外的两个前提，**两个目前都还不存在**（2026-08）：`zhew1585/homebrew-tap` 仓库待创建，`HOMEBREW_TAP_TOKEN` secret 待配置。当前接线下的后果是**快速失败、什么都不发**：release-guards preflight 在 tag 上把 tap 不可达判为 fatal → host 被 skip → 无产物、无 Release。（这条早前写的「缺任一会在 publish 步骤失败，但 `.rb` 仍会作为 Release 产物附上」在 R2 把 Release 创建挪到 `announce` 之后就不再成立了——现在没有任何路径能在 formula 未推送的情况下产出 Release。）formula 内容已本地生成并肉眼核对（homepage 与三个 tar.xz URL 全部指向 `zhew1585`）。
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
- 未改动 `docs/exit-codes.md`、`.github/workflows/ci.yml`、`stories/sprint-status.yaml`（`git diff $(git merge-base develop HEAD)..HEAD` 对这三个文件为空）。注意 `git diff develop..HEAD` 现在**会**显示 `docs/exit-codes.md` 有差异——那是 develop 前进了（config track 已并入，给 Examples 列与 Notes 加了 profile/completions 相关内容），不是本分支改的。已实测：把 develop 当前的 `docs/exit-codes.md` 放到本分支上跑 `readme_exit_codes` 测试**通过**，因为它只比对 code 与 Meaning 两列，而 develop 没有新增退出码（仍是 0-8，Meaning 未变）。所以这一处合并无摩擦。
- 门禁结果：`cargo fmt --all -- --check` 通过；`cargo clippy --all-targets --all-features -- -D warnings` 零告警；`cargo test --workspace` 全通过；`bash scripts/check-binary-size.sh` 2_567_312 B / 4_194_304 B（61%）通过；反向验证过门禁真会红（`MAX_BINARY_SIZE_BYTES=1000000` → 退出码 1）。README 一致性测试同样反向验证：删掉一行 → 测试红并打印期望块，`UPDATE_README_EXIT_CODES=1` 重跑 → 文件被逐字节还原。
- **actionlint 措辞更正（R2 指出）**：`actionlint -shellcheck=` 对全部 workflow 零错误；**默认调用**（shellcheck 集成开启）会报 **6 项 info/style**（SC2086×3、SC2129×2、SC2001×1），全部位于**生成的** `release.yml` 中 cargo-dist 自己的 shell 片段，该文件不可手改。之前 story 写「零错误」只在 `-shellcheck=` 下成立，属于声明大于实际，已改准。本 story 手写的 workflow 与全部脚本在两种模式下都零告警。
- **R2 处置汇总**：1 MAJOR + 6 MINOR。MAJOR（preflight 不 gate 发布）已修并加断言防复发；MINOR 逐条：[2] 权限降级已被 `check-release-gating.sh` 断言且反向实测；[3] 新增 `attest-global-artifacts` host-job 给安装器/formula/sha256.sum/source tarball 补 attestation（不改 `attestations-phase`，因为那会把可写 token 还给编译 job——两个属性现在都拿到了）；[4] 见 Dev Notes 体积结论；[5] tap PAT 加 `if: github.event_name != 'pull_request'`，PR 面归零；[6] musl 脚本注释路径已改正；[7] action-pins 改扫 `.github` 全树含 `.yaml`，反向实测能抓到 `.yaml` 里的浮动 tag。
- **R2 那条「未采纳」的驳回，字面对、结论错（R3-[1]）**：我说 `host` 的 `needs` 里没有 host_jobs、`announce` 的 `if` 不读它们的 result —— 这两句经 R3 逐行核实都属实，R2 报告那句「host 会真正 needs 它们」确实是错的。但我由此得出「host-jobs 拦不住任何东西，所以适合放 attestation」是错的：我**只 grep 了 `release.yml.j2`**，漏掉了 `partials/publish_homebrew.yml.j2` 与 `partials/publish_npm.yml.j2` —— `publish-homebrew-formula` 的 `needs` 也循环 host_jobs，且它的 `if` 没有 `always()`。于是 attest job 失败 → formula 推送被 skip → `announce` 把 skipped 当成功 → Release 公开但 tap 里没有 formula，正是 `github-release = "announce"` 要消灭的半发布态。
  修法（采纳 R3 的建议）：改用 `post-announce-jobs`，它是唯一没有任何 job needs 的 slot，实测生成后 `publish-homebrew-formula` 的 needs 回到 `[plan, host]`、attest job 变成 `needs: [plan, announce]`。代价只是 attestation 比 Release 公开晚几秒，对一个验证辅助物无实质影响。
  **元教训**：断言依赖方向，不要记「哪个 slot 安全」。`check-release-gating.sh` 现在直接断言 publish 路径不依赖 attestation job，换载体也踩不进同一个坑。R4 拿六种错放方式逐个试过（host-jobs / publish-jobs / global-artifacts-jobs / local-artifacts-jobs / plan-jobs / 改名后仍放 post-announce），**全部 fail-closed**。
- **R4 的两条非阻断限制已修，不是只记下来**（这条 track 前三轮都在修「声明大于实现」，收尾不留知而不写的边界）：
  - **不再靠 job 名**：断言的身份来源改为 `dist-workspace.toml` 里注册的 workflow 路径（`./attest-global-artifacts`），并同时断言它出现在 `post-announce-jobs`、不出现在五个阻断 slot 中的任何一个。job 名由该路径推导（`custom-<basename>`），名字含 "attest" 的扫描退化为第二道网（用来兜住「压根没注册」的情况）。实测：把 workflow 改名为 `./provenance` 仍 exit 1，且报的是「`./attest-global-artifacts` 没注册在 post-announce-jobs」这种有指向性的消息，而不是泛化兜底。
  - **`needs_of` 空转会响**：发布路径上的每个 job 现在先断言「needs 块可解析且非空」，再断言「不依赖 attest job」。若将来 dist 改用行内数组 `needs: [plan, host]`，第一条立刻红并说明原因，而不是让第二条空转通过。实测：手工把 `publish-homebrew-formula` 改成行内数组 → 精确报「needs block is parseable」失败并 exit 1。
  - 两条限制的成因与残留边界都写进了 `check-release-gating.sh` 的头注释（regex 切片而非 YAML 解析、身份取自路径而非名字）。
- 反向失败路径逐条实测：体积超限（`MAX_BINARY_SIZE_BYTES=1000000` → 1）、README 退出码漂移（红 → `UPDATE_README_EXIT_CODES=1` 逐字节还原）、未钉 SHA 的 action（构造样例 → 1）、非法 MSI 版本（`1.2.3-alpha` / `256.0.0` / `1.2.3-rc.70000` → 1，`1.2.3-rc.4` / `1.2.3-4` / `1.0.0+9` → 0）、安装器校验和（实跑核验两个安装器均匹配）。R2 轮新增四条反向实测：把 preflight 改回 `plan-jobs` → `check-release-gating.sh` 报「host 不 needs custom-release-guards」并 exit 1；把 `github-attestations-phase` 改成 `"host"` → 报「编译 job 的 `contents: read` 没了」并 exit 1；改掉包 description → `check-wxs-drift.sh` 打印精确 diff 并 exit 1；删掉 `AllowSameVersionUpgrades` → exit 1；在 `.github/workflows/` 放一个带浮动 tag 的 `.yaml` → action-pins exit 1。R3 轮新增两条：attest job 改回 `host-jobs` → 生成文件里 `publish-homebrew-formula` 的 needs 确实多出 `custom-attest-global-artifacts`，gating 脚本报 3 条并 exit 1；attest job 改成 `publish-jobs` → 报 2 条并 exit 1。

### File List

- dist-workspace.toml（新增）
- .github/workflows/release.yml（新增，dist 生成物）
- .github/workflows/release-guards.yml（新增，reusable + dist local-artifacts-jobs）
- .github/workflows/binary-size.yml（新增）
- .github/build-setup/release-build-setup.yml（新增）
- scripts/check-binary-size.sh（新增）
- scripts/install-linux-musl-toolchain.sh（新增）
- scripts/verify-cargo-dist-installer.sh（新增）
- scripts/cargo-dist-installers.sha256（新增）
- scripts/check-action-pins.sh（新增）
- scripts/check-msi-version.sh（新增）
- scripts/check-release-gating.sh（新增）
- scripts/check-wxs-drift.sh（新增）
- .github/workflows/attest-global-artifacts.yml（新增）
- crates/otl/tests/readme_exit_codes.rs（新增）
- crates/otl/wix/main.wxs（新增，起于 dist 生成物，现为手工维护）
- Cargo.toml（追加 [profile.dist]、workspace.package authors/homepage）
- crates/otl/Cargo.toml（追加 repository/authors/homepage 继承、[package.metadata.wix]）
- README.md（Install、Stability and versioning、退出码表、Development）
- stories/4-5-release-pipeline.md（本文件）
