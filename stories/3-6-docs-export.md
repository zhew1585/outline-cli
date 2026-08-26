# Story 3.6: docs export

Status: done

## Story

As a 备份与迁移者,
I want 整个 collection 批量导出为本地 markdown,
so that 内容可进 git 或离线阅读。

## Acceptance Criteria

1. **Given** `otl docs export --collection <id> --out ./docs-backup`
   **When** 执行
   **Then** 该 collection 全部文档经自动分页完整导出为 .md 文件，文件名安全化处理，目录结构反映文档层级
2. **Given** 导出中途某文档失败
   **When** 继续执行
   **Then** 失败文档汇总在结尾报告，退出码反映部分失败

## Tasks / Subtasks

- [x] Task 1: 文件名安全化 (AC: 1)
  - [x] `crates/otl/src/export.rs`（纯函数，全单测）：`safe_stem` + `Names`
  - [x] 路径穿越：分隔符 `/` `\` 替换，2 个以上连续点整段替换，首尾点/空白/连字符裁掉
  - [x] Windows 保留名：CON/PRN/AUX/NUL/COM0-9/LPT0-9/CLOCK$/CONIN$/CONOUT$，忽略大小写与扩展名，加 `_` 前缀
  - [x] 非法字符 `< > : " / \ | ? *` + 控制符 → `-`；不可见重排字符（RLO/ZWJ/BOM 等）直接丢弃
  - [x] 长度上限按**字节**（96 B），按 char 边界截断，截断后再裁一次尾部点
  - [x] 大小写不敏感文件系统重名：`Names` 以「全 Unicode 小写 + NFC」为键去重，冲突加 `-2`/`-3`…（后缀也守字节上限）
- [x] Task 2: 层级重建 (AC: 1)
  - [x] `commands/docs/tree.rs`（纯函数）：由 `parentDocumentId` 建森林
  - [x] 父不在清单内 / 自己是自己的父 / 父链成环 → 一律提升为 root（环用三色状态机在一点切断）
  - [x] 无 id 的行丢弃并警告；重复 id 保留首次出现
  - [x] 兄弟按 (title, id) 排序 → 同一 collection 重复导出得到同一棵树（对 git 友好）
- [x] Task 3: 写盘 (AC: 1)
  - [x] `commands/docs/export.rs`：`documents.list collectionId=` 自动分页枚举 → 每篇 `documents.info` 取 markdown
  - [x] 有子文档的节点 → 目录 `Stem/`，自身写成 `Stem/Stem.md`，子文档写在其中（子目录有独立 `Names`）
  - [x] 每篇正文前置 `# <title>`（文件名已被安全化，标题靠这行保真），已以 `# ` 开头则不重复加
  - [x] 目录深度上限 8 层，更深的**用队列**（不是递归）平铺到该层并警告一次
  - [x] 写盘原子化：同目录 temp（`create_new`）→ `write_all` → `fsync` → `rename`；失败清理 temp
  - [x] R3 重做：**不再占位**。temp 写完 fsync 后，无 `--overwrite` 用 `hard_link`（**不替换**语义）落地、
        再删 temp；有 `--overwrite` 用 `rename`（替换语义）。目标处永远只有「完整文档」或「什么都没有」
  - [x] R3 修复：temp 名由 OS 播种的 `RandomState` 生成（不再是 pid+counter），且清理只删
        `create_new` **确实由本次创建**的那个文件（`TempFile` 的 Drop），不再按路径删别人的文件
  - [x] R2 修复：目录写完后 `fsync` 目录项（Unix），rename 的持久性不再只依赖文件内容 fsync
  - [x] R3 修复：目录 fsync 失败进入退出码与 `--json`（`durable:false` + exit 9），不再只警告
  - [x] R4 修复：`durable` 改三态（`true`/`false`/`null`）。无法 flush 目录的平台报 `null` + stderr 提示，
        绝不报 `true`——Windows 分支不许假装成功
  - [x] R4 修复：新建 `--out` 时逐级记录**自己创建的目录**，结束时按「最深优先」flush 它们的**父目录**
        （目录名是父目录的目录项，只 flush 存文档的那一层等于没保住这些名字）
  - [x] 不跟随符号链接：子目录 `create_dir` + `symlink_metadata` 校验 + canonicalize 后必须仍在 root 内；
        文件只经 `create_new` + `rename` 落地（`rename` 替换链接而非跟随），无 check-then-open 竞态
  - [x] R2 修复：目录被**钉住**（`Dir` 记录 open 时的 (dev, ino)），每次写入前 `verify()` 复核；
        目录被换成链接或另一个目录会报错而非静默接收导出
  - [x] R3 修复：`Dir::sync()` 也 verify（覆盖「最后一篇写完之后才替换」），
        且每篇写完用 `landed_inside` 重新 canonicalize 目标、要求仍在 root 内——
        逃逸出去的写入会被**报告为失败**，不会以 exit 0 + 列出外部路径收场
  - [x] R4 重做：落地确认改为**比对文件身份**（temp 的 `fstat` 身份 vs 目标路径的 stat 身份），
        不再 canonicalize 路径——路径解析挡不住「写入时替换、检查前还原」
  - [x] R4 修复：`TempFile` 记录创建时的身份，Drop 只删「路径仍指向自己那个 inode」时；
        无覆盖模式发布成功后 temp 删除失败会作为 `stray` 上报（stderr + JSON），不再 `let _ =` 吞掉
  - [x] `documents.info` 无 `text` 字段（或为 null）→ 记为失败，不写只有标题的文件；空字符串照常导出
  - [x] `--out` 非空目录在**任何请求之前**拒绝（退出码 2），`--overwrite` 才允许覆盖
- [x] Task 4: 部分失败 (AC: 2)
  - [x] 失败逐条收集（id + 原因），结尾汇总到 stderr（最多列 20 条 + "and N more"）
  - [x] 有失败 → 退出码 9（新增），已登记 `docs/exit-codes.md`
  - [x] **枚举被 CLI 页上限截断 → 同样退出码 9**，`--json` 里 `complete:false` + `enumeration_truncated:true`
  - [x] R2 修复：`--limit` 截断是用户要求的，退出码 0、`complete:true`，另给 `limit_reached:true`
        （用 `Rows::incomplete()` 而不是 `truncation.is_some()`）
  - [x] R2 修复：枚举里**无可用 id 的行**计入 Failure（`Plan::unusable`），
        不再「丢掉 + 只警告 + complete:true」；重复 id 仍只警告（文档本身没丢）
  - [x] R3 修复：`Unusable::label` 的服务端标题经 `text::quote` 清洗（控制符→空格、换行折平、
        Cf 格式字符丢弃、长度封顶）才进 stderr；且「全是 unusable 行」不再同时喊「没有文档」和「导出失败」
  - [x] R4 修复：**可用文档行的服务端 id** 同样清洗后才进终端摘要；JSON 里保留原样（重试要用它）
  - [x] 枚举本身失败 → 沿用其自身退出码（3-7），不写任何文件，不报 9
  - [x] stdout：Table 模式逐行输出相对路径；JSON 模式输出 `{out, exported[], failed[]}`
- [x] Task 5: 测试 (AC: 1, 2)
  - [x] `export.rs` 单测 22 项：穿越/非法字符/控制符/不可见字符/保留名/尾点/空标题/字节上限/去重/
        大小写冲突/NFC-NFD 冲突/兼容字符不折叠/不同标题不误合/`claim_exact`/500 次冲突全唯一
  - [x] `tree.rs` 单测 11 项：扁平/嵌套/父缺失/自环/双环/长环/无 id/重复 id/确定性排序/空清单
  - [x] `tests/docs_export.rs`（tempfile）：层级落盘、跨页 101 篇全导出、恶意标题不越界（含 out 目录外哨兵文件）、
        同名三文档三文件、单篇失败→9 且其余落盘、枚举失败→5 且零写入、非空目录→2 且零请求、
        `--overwrite`、JSON 摘要、空 collection、子目录建不出来时整棵子树进汇总、分支目录与自身文件同名
  - [x] R1 回归测试：100 页耗尽页上限 → 退出 9 + `complete:false`（且断言真的打了 100 次 list，
        否则测试不成立）、`text:null` → 失败而非空文件、`text:""` → 正常导出、NFC/NFD 两篇各得一文件、
        run 后无残留 temp 文件、rename 失败时旧内容完好、`--overwrite` 下目标 symlink 被替换而非穿透、
        `--out` 是 symlink → 退出 2

## Dev Notes

- **为什么用 `documents.list` 而不是 `collections.documents`**：AC 明确要求"经自动分页完整导出"。
  `collections.documents` 一次返回整棵导航树，没有 offset/limit，用它就没有分页可言；
  `documents.list` 带 Pagination 且返回 `parentDocumentId`，层级可以由它重建。
  代价是层级来自父指针而非服务端的导航结构（排序信息 `index` 不用），换来的是分页保证与更强的防御性。
- **正文单独取**：`documents.list` 的行里也有 `text`，但精选命令选择每篇 `documents.info`，
  这样"某篇失败"是天然的部分失败单元，且不依赖 list 是否裁剪正文。
  代价是 1+N 次请求（engine 的令牌桶 10 req/s 会限速）。**这是已知的性能缺口**，见下。
- **递归的两处风险都堵住了**：
  1. 父链成环 → `break_cycles` 把环上一点变成 root，森林里每个节点恰好被写一次；
  2. 超深链在深度上限后若继续递归会爆栈 → 平铺分支改用 `VecDeque`，
     递归深度因此被 `MAX_DEPTH` 硬性封顶。
- **相对 `--out` 曾经必然假警报**（R5 finding 2）：`Path::new("backup").parent()` 是 `Some("")` 而不是 `None`，
  于是 `flush_directory("")` 必然 NotFound，一次**完全成功**的导出被判成 exit 9 + `durable:false` + `complete:false`
  且 `failed` 为空。`--out backup` 失败而 `--out ./backup` 成功——同一个目录。
  空路径现在映射成 `.`（同一个目录，但能打开）。`parents_of` 是纯函数，补了 7 条单测覆盖
  裸相对 / 嵌套相对 / `./` 前缀 / 绝对 / 已存在 / 重复父目录 / 「任何情况下都不产出空路径」。
- **回归测试必须对被测行为敏感**（R5 finding 3）：R4 为父目录 flush 写的回归测试断言的是
  `durable == true`，但 `durable` 只在 flush **返回错误**时才为 false——**根本不 flush 不产生错误**，
  所以删掉整段 flush 逻辑该测试照样全绿。现在改成：预先造一个 0o300（可写可进、不可读）的目录，
  `File::open` 它必然 EACCES，于是「有没有真的去 flush 它」变成可观测的——删掉 flush 循环，测试变红。
  **这一点已实测验证过**（删掉循环 → FAILED，恢复 → ok）。
- **平台专属代码的 cfg 有了自动门禁**（R5 finding 1）：拆分文件时把 `#[cfg(unix)]` 落在了原处，
  裸露的 `use std::os::unix::...` 让 **Windows CI 的整个 test harness 编译失败**——
  本机三条门全绿，因为本机是 Unix。新增 `tests/portability.rs` 扫描全工作区，
  要求每一处 `std::os::{unix,windows,wasi}` 都在同级或更外层的 `#[cfg]` 之下，并自带负向测试。
  另外本轮起用 `--target x86_64-pc-windows-msvc` 做真实交叉编译验证（`rustls-no-provider` 绕开
  `aws-lc-sys` 需要 `windows.h` 的构建脚本），实测复现了该 E0433 并确认修复。
- **文件长度上限也有了门禁**（R5 finding 7）：`tests/limits.rs` 对全工作区执行 `project-context.md`
  的 800 行上限（测试文件不豁免），带一条只会变短的 grandfather 列表。
  它在第一次运行时就抓到 `export.rs` 因本轮修复涨到 919 行，于是拆出 `outdir.rs`（输出目录的校验与创建）。
- **文件系统层拆成两个模块**：`dir.rs`（`Dir` 目录钉住、身份、`flush_directory`、`Durability`）
  与 `target.rs`（temp 写入、落地、`confirm_landing`）。R4 修完后单文件超过 800 行上限，按职责切开。
- **占位法是错的**（R3 finding 2）：R2 用「先 `create_new` 占名、再 rename 覆盖占位」拿到了互斥，
  但代价是目标路径在那个窗口里是一个**零字节的正式文件**。并发读者会读到空文档；
  更糟的是进程在窗口内被 SIGKILL 或断电，那个空 `Document.md` 会永久留下，
  而下一次无 `--overwrite` 的导出又会因「目标已存在」而拒绝——一次崩溃变成需要人工清理的持久故障。
  现在：temp 写完 fsync 后用 `hard_link`（**不替换**语义，重名即失败）落地，再删 temp。
  互斥仍是内核给的，而目标处从不出现半成品。`--overwrite` 仍用 `rename`（替换语义）。
- **清理只能删自己创建的东西**（R3 finding 3 + R4 finding 3）：旧代码按路径 cleanup，
  于是 temp 的 `create_new` 因「已存在」失败时，紧接着的 `remove_file` 把**别人那个文件删了**。
  R3 让 `TempFile` 只在 `create_new` 成功后才存在（guard 建立 = 我们创建了它），
  temp 名也换成 OS 播种的随机名。但随机名只防**预先**占据，防不住「创建之后」被观察并替换：
  攻击者 unlink 掉 temp、在同名放自己的文件，Drop 仍会按路径删掉那个旁观者。
  R4 因此让 `TempFile` 记住创建时的**句柄身份**，删除前先确认路径仍指向那个 inode，否则什么都不做。
- **残留的 temp 会让下一次导出以 exit 2 被拒**（R5 finding 5）：`stray` 文件叫 `.otl-export-*.md`，
  点号开头，`ls` 不显示；而 `is_empty_dir` 认点文件。于是用户对着一个肉眼为空的目录被告知「非空」。
  现在分开两种情况：用户自己放的内容仍是「is not empty，加 --overwrite」，
  本命令自己的残留则给专门文案——列出文件名、说明它们是隐藏的、告诉用户删掉再跑。
- **`failed[].id` 对 unusable 行本来不是 id**（R5 finding 6）：R4 新写的 doc 承诺「id 原样保留，供脚本重试」，
  但无 id 的 listing row 塞进去的是 `"listing row 1 (Title)"` 这种人类可读标签，
  脚本拿它去调 `documents.info` 只会得到本地 schema 校验失败。现在 `Failure` 拆成
  `id: Option<String>`（无 id 时为 `null`）+ `label: String`（终端摘要用，经 `text::quote`）。
- **残留的 temp 是完整副本，不能默默留着**（R4 finding 4）：无覆盖模式下发布成功后，
  temp 名是文档的第二个硬链接。删不掉时它就是输出树里一份隐藏的完整拷贝——
  既污染备份也多一份泄漏。现在 `write_atomically` 把它作为 `Written::stray` 返回，
  stderr 警告 + JSON `stray[]` 列出。退出码不变：文档本身确实导出成功了。
- **写盘为什么必须原子**（R1 finding 3）：旧实现 `--overwrite` 先 `truncate(true)` 再写，
  一旦 `write_all`/`flush` 中途失败（磁盘满、配额），**上一次有效备份已经被清空了**，
  而命令只报一个 exit 9。改为「同目录 temp → fsync → rename」后：目标文件只被 `rename` 整体替换，
  写失败时目标一个字节都没动，temp 被删掉，不留残缺文件。
- **符号链接不再靠 check-then-open**（R1 finding 7）：`symlink_metadata` 后再 `open` 是 TOCTOU。
  现在 temp 文件用 `create_new`（`O_CREAT|O_EXCL`，对已存在项直接失败，不可能跟随链接），
  再用 `rename` 落地——`rename` **替换**目标处的符号链接而不是跟随它。这条竞态窗口被结构性消除，
  不是被缩小。子目录只用 `create_dir`（不用 `create_dir_all`），已存在项必须是真目录，
  并在 canonicalize 后要求仍在 root 内——目录被换成链接会在使用它的那一刻被抓到。
  另外所有路径都是 `root.join(<安全组件>)`，安全组件里不可能出现分隔符或 `..`。
- **`--out` 祖先是符号链接是合法的**：macOS 的 `/tmp`、`/var` 本身就是链接，很多 home 目录也是。
  所以祖先链接照常跟随，但只跟随一次（`--out` 在动手前 canonicalize），
  结尾的 "exported N documents to <path>" 报告解析后的真实位置——重定向可见而非静默。
  `--out` 的**末级**不得是符号链接（退出 2）。
- **Unicode 去重键**（R1 finding 4 + R2 finding 5）：NFC 的 `é`（U+00E9）与 NFD 的 `e`+U+0301 是不同字节串，
  但在 macOS 卷上是**同一个目录项**；希腊 final sigma `ς` 与 `σ` 在大小写不敏感卷上同理。
  `to_lowercase()` 挡不住后者（`ς` 本来就是小写），所以键是「uppercase→lowercase **迭代到不动点**，再 NFC」——
  一次往返不是不动点：`ẞ` uppercase 是自己、lowercase 得 `ß`，而 `ß` uppercase 得 `SS`、lowercase 得 `ss`，
  单次会给出 `ß` 与 `ss` 两个键（R3 finding 5，属于**欠**折叠，正是会丢文档的方向）；
  迭代后 `ẞ`→`ß`→`ss` 收敛，三种写法同键。
  不带完整 case-folding 表拿到 caseless 比较的标准做法，比单纯 lowercase 更激进。
  两个方向的误差被权衡过并写进代码注释：**折多了**只是让某个名字白拿一个 `-2` 后缀（外观代价），
  **折少了**是两篇文档抢一个目录项、丢一篇（数据代价）。因此故意偏向折多。
  副作用是 `ﬁle`/`file`、`Straße`/`Strasse` 也会共享键——有测试锁死这个方向，改回严格必须是显式决定。
  normalization 仍只用 NFC 不用 NFKC：NFKC 会把全角→ASCII、`№`→`No` 也折掉，那远超任何文件系统。
- **别名兜底必须在覆盖之前**（R2 finding 5）：旧实现在 rename **之后**记 (dev, ino)——
  而 rename 装的是新 temp 的**新 inode**，事后 stat 永远看不到「刚刚覆盖了本次已导出的文件」。
  现在是写入**前**先 stat 目标：若它已经是本次写过的文件，说明这两个名字在该文件系统上是同一个目录项，
  报冲突并跳过，而不是把覆盖计成两次成功。
- **退出码 9 是新增的公共 API**：已写入 `docs/exit-codes.md`，并明确"批量还没开始就失败的用原错误码"。
- **Windows 路径长度**：8 层 × 96 B 的 stem 可能越过传统 `MAX_PATH`(260)。
  这类失败会变成该篇文档的 I/O 失败并进结尾汇总（而不是静默丢失），深度上限也是为此存在。

### 已知缺口（刻意留下）

- **顺序导出，无并发**：SPEC 的依赖基线提到"批量导出并发"是选 reqwest 的理由之一，
  但 v1 用 blocking client + 进程级令牌桶。并发需要重新设计限速与错误聚合，
  收益（墙钟时间）不值得在本 epic 引入 async 运行时；结构上没有阻碍后续加。
- **不导出附件/图片**：正文里的附件仍是 Outline URL。附件是两步协议（SPEC 明列的手写特例预算），
  不在本 story 范围。
- **不写 manifest**：没有 id → 路径的映射文件，因此二次导出后无法按 id 定位旧文件。
  v2 的 pull/push 才需要这个。
- **Windows 的别名保护从 story 提升成了运行时行为**（R3 finding 5）：
  默认（无 `--overwrite`）路径用 `hard_link` 落地，**不替换**语义在所有平台上都会因重名而失败——
  文件系统认为两个名字等价时，第二次 link 直接报错，不再依赖 (dev, ino)。
  `--overwrite` 模式下仍只有 Unix 有身份预检查。
- **`durable` 是三态，不是布尔**（R4 findings 5/6）：R3 引入这个字段是对的方向，但它有两处**未经证明的 true**。
  其一，Windows 的 `Dir::sync` 是无条件 `Ok(())`——什么都没做却让 `durable:true`，
  正撞在项目铁律「Windows 分支不得假装成功」上。现在 `sync` 返回 `Durability::{Flushed, Unconfirmed}`，
  无法 flush 的平台报 `null` 并在 stderr 说明一次；`false` 保留给「试过且失败」。
  其二，`--out parent/export` 由 `create_dir_all` 建出来时，`export` 这个**名字**是 `parent` 的目录项，
  只 flush `export` 本身保不住它。现在改用自写的逐级创建，记录「本次创建了哪些目录」，
  结束时按最深优先 flush 它们的父目录（含第一个已存在的祖先，它持有第一个新建目录的名字）。
- **(dev, ino) 兜底与目录钉住只在 Unix**：Windows 的 file index 只有 std 的 unstable API 能拿到
  （`MetadataExt::file_index`，`windows_by_handle` feature）。NTFS 大小写不敏感但不做 normalization folding，
  而本键的 uppercase 往返正好覆盖 NTFS 的 uppercase 折叠，所以 Windows 上的残余风险很小；
  但**它确实没有兜底**，这一点在此明记。`create_new` + `rename` 的保证是跨平台的，承重部分在那里。
- **落地确认必须比对身份，不能比对路径**（R4 finding 2）：R3 用 `canonicalize(dest).starts_with(root)`，
  它只能证明「**此刻**这个名字指向 root 内」，而攻击序列恰好绕开这一点：
  写入时把分支目录换成外部 symlink（temp 与最终文件落到外部），在检查**之前**把原目录换回来。
  `--overwrite` 场景下还原后的目录通常还有同名旧文件，于是路径解析成功、`Dir::sync` 看到原 inode，
  命令 exit 0 并列出内部旧路径，而新 markdown 在外面。
  现在改为：temp 的身份取自**打开的文件句柄**（`fstat`，不经路径，不可被重定向），
  落地后 stat 目标路径并比对。还原目录并不能把那个 inode 变出来——原目录要么该名字不存在、
  要么是身份不同的旧文件，两种情况都会失配并报失败。
  仍然只是**检测**而非阻止（字节已经在外面了），但「以成功姿态汇报一个自己没写的路径」被堵死了。
  无身份的平台（Windows）退化为路径解析，代码里明说这是较弱的那一种。
- **逃逸必须至少被报告**（R3 finding 4）：R2 声称「替换会被报告」，那半句不成立——
  `verify()` 之后到落地之间被替换、且最后一篇文档之后不再 verify 时，
  写入可以成功落到 root 外而命令仍 exit 0 并把外部路径列为已导出。两处补上：
  `Dir::sync()` 也 verify（覆盖最后一篇），以及每篇写完后 `landed_inside` 重新解析目标路径。
  后者是**事后检测**（字节已经在别处了），但它堵住了「以成功姿态汇报一个自己没写的路径」。
- **路径式写入无法对抗「导出过程中被重命名目录」的攻击者**（R2 finding 4）：
  resolved path 检查完再使用，中间那个窗口要用 `openat` 系目录句柄才能关掉，std 不提供，
  而本项目禁止 `unsafe`。两点约束损失：这种攻击者本来就对被写入的目录树有写权限（他能直接改导出结果）；
  且目录钉住把「静默改道」变成「报错」——被换掉的目录 inode 与钉住的不一致。
  `target.rs` 的模块文档写的是这个**窄**保证，不再声称 "never through a symlink"。
- **并发导出到同一目录**：无 `--overwrite` 时由 `create_new` 占位保证互斥（内核语义，非检查），
  两个进程不可能都成功写同名文件。带 `--overwrite` 时后写者胜——这正是该 flag 的语义。
- **`index` 排序信息未使用**：兄弟按标题排序（确定性、git 友好），不复刻 Outline 侧边栏顺序。

### References

- [Source: planning/epics.md#Story 3.6]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-6、#Constraints（两步协议不覆盖）、#Non-goals（pull/push）]
- [Source: specs/spec-outline-cli/failure-modes.md #5 #7]
- [Source: docs/exit-codes.md#9]

## Dev Agent Record

### Agent Model Used

claude-opus-5[1m] (Claude Code agent), 2026-08-26

### Completion Notes List

- `startup_guard.rs` 的源码扫描禁止 `read_dir`（防运行时发现数据文件）与 `include_str!`。
  本 story 给出 4 条带理由的 allowlist 条目：export 需要枚举用户给的 `--out` 目录，
  三处 `include_str!` 是 `#[cfg(test)]` 里的 golden fixture。
  该文件对二进制本体的断言（不含 spec 路径/内容）未放宽，仍是硬证据。
- 单篇文档失败的原因字符串来自 `CliError`/`io::ErrorKind`，都是已消毒文本，不会把服务端原文或路径秘密带进汇总。

### File List

- crates/otl/src/export.rs（文件名安全化，纯函数）
- crates/otl/src/commands/docs/{export.rs, tree.rs}
- crates/otl/src/exit.rs（`ExitCode::Partial = 9` + `CliError::partial`）
- docs/exit-codes.md
- crates/otl/tests/docs_export.rs
- crates/otl/tests/startup_guard.rs（allowlist 追加）
