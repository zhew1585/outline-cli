# Story 4.3: doctor

Status: review

## Story

As a 排障的用户,
I want 一条命令看清环境健康,
so that 问题自查不开 issue。

## Acceptance Criteria

1. **Given** 执行 `otl doctor`
   **When** 诊断运行
   **Then** 输出：认证状态、实例连通性、线上 API 与本地 spec 差异（缺失/已弃用操作）、spec 缓存健康
2. **Given** 任一检查发现阻塞性问题
   **When** 报告打印完毕
   **Then** 退出码等于「第一条阻塞发现在其它命令里本来会产生的那个码」（本地问题 2 / 需重新认证 4 /
   实例侧 3·5·6·7·8），**不新增退出码**；仅告警（缓存被丢弃、本地表落后、spec 源不可达、环境里有明文 key）
   退 0
3. **Given** Story 2-6 AC 5/6 交给 doctor 的部分
   **When** 报告凭证健康
   **Then** 输出凭证文件路径、存在性、权限是否合规、各 profile 有哪些凭证类型，且 Windows 上明示
   「不设权限位、保护完全依赖 profile 目录 ACL」；**绝不打印任何凭证值或其片段**
4. **Given** `--offline`
   **When** 诊断运行
   **Then** 不发出任何网络请求（实例与 spec 源都不碰），其余检查照常报告

## Tasks / Subtasks

- [x] Task 1: 报告模型与双态渲染 (AC: 1, 2)
  - [x] `commands/doctor/report.rs`：`Status{Ok,Warn,Problem(ExitCode),Skipped}` + `Check` + `Report`
  - [x] `Report::blocking()`＝**第一条** Problem（依赖序＝该修顺序），`exit_code()` 由它决定
  - [x] stdout 为数据（人类行 / `--json` 对象），stderr 只有 `main` 打印的那条阻塞摘要
  - [x] 人类渲染在 **sink 层**过 `stdio::scrub_terminal_controls` 并强制单行；`--json` 按 `text.rs` 的
        既有决定豁免（payload 要能 round-trip）
  - [x] golden file 测试（合成报告，不含机器相关值）
- [x] Task 2: 本地检查 (AC: 1, 2, 3)
  - [x] `configuration`：config 文件位置/实际读到的文件/profile 及其来源层/文件里定义了哪些 profile
  - [x] `instance`：经 `auth::resolve_instance`（含 TLS 规则），报 origin 与 URL 来源层
  - [x] `credentials`：直接调 `auth::report::credential_health()`（2-6 留的接缝），加平台保护说明
  - [x] `credential`：经 `auth::resolve_credential`，报 method / 被遮蔽的候选 / 过期与 scope
  - [x] 阻塞码一律取自 `auth::exit_code_of`（新增：`map_auth_error` 的借用版，同一张表）
- [x] Task 3: 连通性 (AC: 1, 2, 4)
  - [x] 唯一一次实例请求：`auth.info` 走 engine execute 通道（与 `auth login` 收尾同一条调用）
  - [x] 凭证只能来自 `resolve_credential` 的 `Resolved`，`into_client` 消费它——没有第四条凭证路径
  - [x] 401→4、5xx→6、传输失败→7、429 耗尽→8：全部由既有 mapper 决定
- [x] Task 4: spec 检查与线上差异 (AC: 1, 2, 4)
  - [x] `local-spec`：`ops::table()` / `ops::is_synced()` / `cache::load()`；损坏或过期＝**告警**且回退内置
  - [x] `online-spec`：`spec::upstream_table()`（复用 `spec sync` 的 fetch+compile 入口，**不写缓存**）
  - [x] 三类差异：`missing`（线上有本地无）、`withdrawn`（本地有线上已不声明）、
        `deprecated`（线上标记弃用且本地仍可调用）
  - [x] 缓存 hash 与刚抓到的文档 hash 对比，明说「缓存就是这份文档编出来的」或「不是」
  - [x] `spec-compile`：`CompiledOp.deprecated`（只到编译器层，**不进 IR**，不动 IR schema 版本）
- [x] Task 5: 文档与登记 (AC: 2, 3)
  - [x] `docs/exit-codes.md`：登记 doctor 的四条规则（first-not-worst / 告警不阻塞 / 报告必打印 / 不自己分类）
  - [x] README：`## Checking your environment`；并修正 Design 里「唯一例外是 spec sync」的表述
  - [x] `project-context.md`：运行时解析 OpenAPI 的例外归属改述为「模块」而非「命令」
- [x] Task 6: 测试 (AC: 1-4)
  - [x] 26 个单测（report / checks / drift）+ 13 个 wiremock 端到端 + 2 个 golden + 1 个 speccompile
  - [x] 每个断言都做过回退验证（17 处变异全部转红，清单见 Dev Notes）
  - [x] 全部端到端用 `tests/common/mod.rs` 的 `isolate` + 自己的临时目录

## Dev Notes

- **先读 `project-context.md`**。本 story 触碰的红线：不打印凭证（含 debug）、凭证获取只走既有三个入口、
  HTTP 只走既有三条通道、文本清洗、退出码是公共 API、库层禁 unwrap、文件 <800 行 / 函数 <50 行、
  Windows 分支显式处理、不 phone home。

- **退出码语义为什么这样定**（这是本 story 唯一的公共 API 决策，先论证后登记）：
  - 候选 A「永远退 0，只是报告」：CI 里 `otl doctor` 就没有任何用处——一个 world-readable 凭证文件
    或压根没配凭证的机器，脚本无法从退出码看出来。
  - 候选 B「发现问题就退 1」：把「本地少个环境变量」和「内部错误」混成一个码，与既有表冲突
    （1 是 generic failure）。
  - 候选 C「新增一个 doctor 专属码（比如 10）」：新增码就是新增公共 API，而它承载的信息量为零——
    调用方还要再读报告才知道该干什么。
  - **选定 D**：**不新增码，取第一条阻塞发现在其它命令里本来会产生的那个码**。语义与全表一致
    （2＝本地修、4＝重新认证、7＝网络），脚本已有的分支逻辑直接复用；实现上 code 只能来自
    `auth::exit_code_of`，所以诊断不可能与被诊断的命令给出不同的码。
  - **first-not-worst**：检查按依赖序跑，早出现的问题既是后面失败的原因、也是该先修的东西。
    取「最严重」会把「`OUTLINE_URL` 没设」报成网络错误。
  - **告警永不改码**：缓存被丢弃、本地表落后、spec 源不可达、环境里有明文 key —— 都不妨碍 `otl` 工作。
    尤其 spec 源是第三方主机，让它的 404 把一个能用的环境判成坏的，doctor 就没法进 CI。
  - **报告先打印再退出**：`--json` 消费方每次都拿到同一个对象；阻塞摘要另外走 stderr（由 `main` 打印）。

- **「已弃用操作」到底指什么**（FR23 / SPEC.md「doctor 能发现本地 spec 缺失或已弃用的操作」）：
  两种读法，本 story 都实现了，因为单靠任一种都会被挑：
  1. **diff 读法**：本地表里有、线上文档已不再声明 → `withdrawn`。这是 `spec sync` 的 added/removed
     同一套词汇，也是「差异报告」的字面含义。
  2. **flag 读法**：OpenAPI 的 `deprecated: true`。实测 vendored 文档里 **operation 级 `deprecated` 为 0 条**
     （19 处 `"deprecated": true` 全在 schema/参数层），所以只做这一种会得到一份永远为空的报告。
     为此在 **spec-compile** 加了 `CompiledOp.deprecated`（唯一构造点，约 10 行），
     **故意不进 `engine::ir`**：IR 是 dispatch 表，加一个位就要动 `IR_SCHEMA_VERSION`，
     那会作废所有用户已有的缓存——而唯一需要这个位的消费方（doctor）本来就自己编译刚抓到的文档。
     报告只取「线上标记弃用 ∩ 本地仍可调用」的交集：本地压根没有的操作被弃用不是用户的问题。
  - failure-modes.md #8 里还有一条「弃用操作 IR 标记并警告一个版本」——那是**调用时**警告，需要
    IR 带弃用位，属于另一条 story 的范围；4.3 的 AC 只要求 doctor 的报告。**故意留下的缺口**。

- **为什么 doctor 不自己抓文档**：`tests/no_phone_home.rs` 把 `fetch_document` / `get_text` /
  `UPSTREAM_SPEC_URL` 收敛在 `crates/engine/src/fetch.rs` 与 `crates/otl/src/commands/spec.rs`。
  doctor 调 `commands::spec::upstream_table()`，于是**那张收敛表一个字都不用改**——
  「codebase 里只有三处 `.send()`」这个不变量原样成立。代价是 `commands/spec.rs` 从 559 涨到 626 行（限 800）；把它拆到新文件反而要放宽收敛表，那是更差的交易。

- **doctor 与 `auth info` 的一处**故意**分歧**：完全没有凭证时，`auth info` 打 `method: none` 且退 0
  （它是**描述**命令），doctor 判定为 Problem/2（它是**判定**命令：这个环境不能用）。两者读的是同一个
  `resolve_credential`，只是对同一状态的**结论**不同，这在 `credential()` 的注释里写明了。

- **凭证文件不可用时 doctor 一个字节都不发**：`choose()` 先 `store.load()`，失败即 `Refused`，
  连通性检查被跳过。测试 `an_over_wide_credential_file_exits_two_before_anything_is_sent` 断言
  mock 服务器收到 **0** 个请求，并且**故意同时导出了 `OUTLINE_API_KEY`**——否则「没发请求」只是因为
  没东西可发，这个断言就不敏感（回退验证 M14 第一次跑出 GREEN，就是这个原因，随后把 env key 加进去才转红）。

- **文本清洗在 sink 层**：doctor 的报告走 stdout，不经 `write_diagnostic_line` 的清洗，而它要插入
  config 文件里的 profile 名、环境变量给的路径、抓到的文档里的 operation 名、服务端的错误消息。
  所以 `report::human_line()` 对**每一行**做 `stdio::scrub_terminal_controls` 并把换行压成空格：
  一个带换行的外来值不能伪造成另一条检查的结论。`--json` 按 `text.rs` 既有的决定豁免。

- **平台差异用 `cfg!` 而不是 `#[cfg]`**：`protection_note()` 两个分支在所有平台都编译，
  文本在所有平台可测，配对关系由 `assert_eq!(protection_note().is_some(), cfg!(windows))` 钉住
  （回退验证 M12：把条件换成 `cfg!(unix)` 后该测试在 macOS 上立刻转红，所以这条断言是敏感的）。

- **golden file 用合成报告**：真实报告含机器相关值（凭证路径、操作数、时钟），拿它做 golden 等于
  把开发机钉进仓库。合成报告覆盖四种 status、多行 detail、以及一个带 ESC/BEL 的敌意 summary，
  于是 golden 同时是**布局**和**清洗**的证据。

- **回退验证清单（17/17 全部转红）**：见 Dev Agent Record。

### Project Structure Notes

```
crates/
  speccompile/src/lib.rs                 # + CompiledOp.deprecated（不进 IR）
  otl/
    src/commands/spec.rs                 # + fetch_remote() / compile_document() / upstream_table()
    src/commands/doctor/
      mod.rs                             # 参数、编排、退出码语义（模块文档即契约）
      report.rs                          # Status/Check/Report + 双态渲染 + sink 层清洗 + fact 值转换
      checks.rs                          # 「一次请求会怎样」：configuration / instance / credential / connectivity
      credentials.rs                     # 「凭证库长什么样」：文件与目录分评级（R2 F1 后独立成文件）
      drift.rs                           # local-spec / online-spec（缺失·撤回·弃用）
    src/auth/mod.rs                      # + exit_code_of() / instance_answered()（借用版判定）
    src/auth/error.rs                    # + StoreError::condition()（描述，不带裁决）
    src/auth/file_guard.rs               # permissions() 改 symlink_metadata（与 O_NOFOLLOW 同一个问题）
    src/auth/report.rs                   # + file_readable / lines_without_verdict()
    src/errors.rs                        # + engine_exit_code() / server_answered()
    tests/common/doctor.rs               # 两个端到端 suite 共用的 fixture
    tests/doctor_e2e.rs                  # 环境半边：13 个 wiremock 端到端
    tests/doctor_spec_e2e.rs             # spec 半边：6 个 wiremock 端到端
    tests/doctor_golden.rs               # 2 个 golden
    tests/golden/doctor_report.txt
```

R2 修复把三个文件推过了 800 行,`tests/limits.rs` 当场抓到。按**职责**而不是按行数拆:
- `checks.rs` 923 → 479 + `credentials.rs` 455:前者问「一次请求会怎样」,后者问「凭证库长什么样」——
  R2 F1 的整个争点就是这两个问题对同一个目录给出不同答案,所以它们本来就该是两个文件。
  三个 fact 值转换(`optional` / `optional_number` / `path_value`)移进 `report.rs`,
  因为两个 check 模块都要产出 fact,而「缺失值渲染成什么」有两份就会分叉。
- `auth/error.rs` 808 → 737:`condition()` 的测试移进 `auth/report.rs`——它保护的性质是**那个**模块的
  (「描述不得带裁决」),放在消费方比放在定义方更能说明为什么。
- `tests/doctor_e2e.rs` 878 → 510 + `doctor_spec_e2e.rs` 213 + `common/doctor.rs` 200:
  环境半边与 spec 半边是不同主题、不同 fixture(与 `common/cache.rs` 因同样理由存在的先例一致)。

### References

- [Source: planning/epics.md#Story 4.3、FR23]
- [Source: specs/spec-outline-cli/SPEC.md#CAP-7（doctor 差异报告）]
- [Source: specs/spec-outline-cli/failure-modes.md #7 #8 #10]
- [Source: stories/2-6-credential-file-hygiene.md AC 5/6 与其 Dev Notes 的归属说明]
- [Source: docs/exit-codes.md]
- [Source: project-context.md 全文]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-26

### 回退验证（每条都是：改坏被测逻辑 → 确认测试转红 → 复原）

驱动脚本按表逐条打补丁并跑相关目标，结果 17/17 转红：

| # | 改坏了什么 | 转红的测试 |
|---|-----------|-----------|
| M1 | `Report::blocking()` 恒返回 `None` | 10 个（4 个端到端退出码 + report/mod 单测 + 2 个 golden） |
| M2 | `human_line()` 不做清洗 | `human_output_is_scrubbed_and_kept_on_one_line` + 2 个 golden |
| M3 | `Check::detailed()` 不拆多行 | 同上 2 个 |
| M4 | 权限过宽的凭证文件报成正常 | 端到端 `an_over_wide_...` + 单测 `an_over_wide_credential_file_is_a_problem_that_names_it` |
| M5 | 没有凭证只算 Warn | 端到端 `no_credential_anywhere_...` + 单测 `a_missing_credential_names_every_way_to_get_one` |
| M6 | `--offline` 被忽略 | 端到端 `offline_contacts_neither_...` + 单测 `connectivity_is_skipped_offline_...` |
| M7 | 实例不可达只算 Warn | 端到端 7 / 4 / hostile 三个 |
| M8 | 不计算 deprecated | 端到端 `a_deprecated_operation_...` + 单测 `only_deprecations_of_...` |
| M9 | 不计算 missing | 端到端 `the_online_comparison_...` + 单测 `an_operation_only_online_...` |
| M10 | spec 源不可达算 Problem | 端到端 `an_unreachable_spec_source_is_a_warning_...` + hostile |
| M11 | 缓存损坏报成正常 | 端到端 `a_damaged_spec_cache_is_a_warning_...` |
| M12 | 平台说明的条件换成 `cfg!(unix)` | 单测 `the_windows_note_appears_exactly_where_it_is_true` + `a_healthy_credential_file_...` |
| M13 | 不比对缓存/文档 hash | 单测 `the_cache_hash_is_compared_with_the_document_that_was_fetched` |
| M14 | 凭证文件读不了就退回用 env key | 端到端 `an_over_wide_credential_file_exits_two_before_anything_is_sent`（**第一次是 GREEN，见 Dev Notes**） |
| M15 | 报告里回显 `OUTLINE_API_KEY` 的值 | 端到端 `the_report_never_carries_a_credential_or_a_fragment_of_one` |
| M16 | config 文件不可用只算 Warn | 端到端 `an_unparsable_config_file_exits_two_...` |
| M17 | `is_deprecated()` 恒 false | speccompile 单测 + 端到端 deprecated |

R1 修复后新增 6 处变异(全部转红),原 17 处在改动后**重跑**仍全部转红(M4/M7/M10 的补丁位置随重构更新):

| # | 改坏了什么 | 转红的测试 |
|---|-----------|-----------|
| N1 | 目录可写又判回 Problem | 2 个端到端 + `the_file_blocks_and_a_writable_directory_only_warns` + `a_directory_only_warning_names_...` |
| N2 | 文件等级改用复合的 `usable` | 同上 4 个 |
| N3 | 去掉 `read_checked` 对过宽文件的拒绝 | `an_over_wide_credential_file_exits_two_before_anything_is_sent` + 4 个既有凭证测试 |
| N4 | 非法 `--spec-url` 又判 Warn | `an_invalid_spec_url_is_a_usage_problem_not_a_warning` |
| N5 | 任何探测失败都写「could not be reached」 | `an_instance_answering_with_something_other_than_json_exits_one` |
| N6 | `reachable` 恒 false | 同上 |

N3 是审查者点名要求的那条:它证明「文件过宽必须 0 请求」这条收窄后的不变式是**真的被 `read_checked`
执行**的,而不是因为「反正没东西可发」而空过。

### R1 对抗审查处置（deepseek-v4-pro,只读)

审查结论:可合并,一条 MAJOR + 两条 MINOR。三条全部已修,每条都做了回退验证。

**Finding 1 [MAJOR] — doctor 自相矛盾:宣布凭证库不可用,然后拿它发了请求。**
`credentials` 检查用 `credential_health().usable`(**含**目录检查),`choose()` 用
`store.load()` → `read_checked`(**不含**目录检查)。于是目录 0777 + 文件 0600 合法时,同一份报告里
`credentials.usable=false` 与 `connectivity.reachable=true` 并存,mock 收到 1 个请求。

处置经过一次裁决反转,值得记录:先裁定「store 被判不可用时 `credential`/`connectivity` 进 Skipped」,
随后被撤回——因为支撑它的威胁模型不成立:
- `secret_file::read_checked` 对**已打开的描述符**做 `require_regular_owned`(要求属主是本人),
  攻击者无法在世界可写目录里植入一个属主是受害者的文件;symlink 被 `O_NOFOLLOW` 拒;文件 0600 他也读不到。
  残余风险只有「删掉/换成一个会被拒的文件」= 骚扰与拒服,**不是机密性或完整性问题**。
- Story 2.6 AC 6 要求 doctor **报告**权限是否合规,不是阻塞;AC 1 只要求**创建时** 0700,
  Task 1 明确写了「已存在的目录不动」。去警察既有目录会与创建它的那条 story 相矛盾。

**最终修法**:把「文件自身」与「目录」拆成两个等级(`checks.rs::grade`,理由与依据写在函数文档里):
- 文件自身不可用(过宽 / 非 regular / 非本人所有 / 解析失败 / 版本过新)→ **Problem/2**,且一个字节都不发
  (`read_checked` 在描述符上就拒了,doctor 与真实命令答案一致);
- **仅目录**不安全而文件合规 → **Warn/退 0**,`credential` 与 `connectivity` **照常运行**。
- doctor 的 JSON 不再输出复合的 `usable`,改输出 `file_usable`——否则「usable=false」会紧挨着一个
  真的用了它的 `connectivity`,那是同一个矛盾换了个标签。
- `credential_health` 新增 `file_readable`(加字段,**不改 `usable` 语义**,`auth info` 不受影响)。
  必须加:目录坏 **与** 文件坏都会让 `usable=false`,只凭 `usable` + `directory_problem` 两个信号
  无法区分「目录可写且文件损坏」这一格,会把真实的文件问题降级成警告。

**Finding 2 [MINOR] — 非法 `--spec-url` 被降为警告。** `online_spec` 把 `upstream_table` 的任何
`Err` 一律判 Warn,于是 `--spec-url "not-a-url"` 退 0,而 `docs/exit-codes.md` 把同一个错误在
`spec sync` 里定为 code 2。「第三方主机故障不该判环境有罪」这条**对用户自己打错 flag 不适用**。
修法:按 fetch 域**已经**赋予的码分流——`ExitCode::Usage` 只可能是 `FetchError::InvalidUrl`
(本地校验失败、什么都没发出)→ Problem/2;传输失败、404、5xx、429 耗尽、文档编不过 → 维持 Warn。
**不做前置校验**:那会复制一份 fetch 通道的 URL 规则,两份规则终将互相漂移。代价是这条 Problem 排在最后,
被更早的阻塞发现抢占——与整份报告的 first-not-worst 一致。

**Finding 3 [MINOR] — 漏登记可达的退出码 1,且该场景措辞失真。** 实例回 200 + 非 JSON body 时
`fetch_identity` → `EngineError::InvalidResponse` → 退 **1**,而 doctor 段只写了 0/2/3/4/5/6/7/8。
已补 1(并明确 9 不可达)。措辞方面,原先任何探测失败都写「the instance could not be reached」——
实例其实被触达了。现在按码分述(`outcome_summary`),且 `reachable` 只在 `ExitCode::Network`
时为 false:401/5xx/垃圾 body 都是「答了,但答的东西没用」。补了 200+非 JSON 的端到端。

**R1 修复带出的两处附带改动**(都是审查者没提但同一处的诚实性问题):
- `connectivity` 的跳过理由原先只有一句「there is no usable instance and credential to try」——
  用户读不出缺的是哪一半。现在四种理由各自成句(`--offline` / 没有可用实例 URL / 没配凭证 /
  凭证被拒),`Chosen::Unchecked` 的理由原样透传。函数因此超了 50 行,按已有接缝拆成
  `connectivity` + `approved` + `probe`。
- `check-all.sh` 抓到一个我自己漏掉的 clippy `doc_lazy_continuation`(新 doc 注释里一行以 `-` 开头
  被当成列表项)。这正是那个脚本存在的理由:我在加完注释后只跑了 `cargo test`。

### R2 对抗审查处置(deepseek-v4-pro,只读)

R2 结论:可合并,无 BLOCKER 无 MAJOR,5 个 MINOR。三条 R1 处置的核心行为判定正确、被敏感测试钉住;
抽查 4 条变异(含点名的 N3)全红。**5 条 MINOR 全部已修**,它们的共同点是「报告说了不实的话」——
退出码都对,但一份会说假话的诊断报告比没有诊断更糟。

**F1(最重要)—— R1 那个 MAJOR 的文本残留。** 目录 0777 + 文件 0600 时,退出码、JSON 字段名、summary
都已改对,但 detail 块还站在旧裁决那一边:`directory_problem` 仍含「refusing to use it」,detail 仍有
`usable: no`(来自复合 `usable`),而同一份报告里紧挨着的是**真的用了这个文件**的 `connectivity: ok`。
- 「refusing to use it」对**写路径与 lock 路径**为真(那里确实拒绝),对 doctor 走的**读路径**为假
  (`read_checked` 不看目录),所以不能删——它在自己的语境里是对的。修法是在**转成描述的那一刻**分叉:
  新增 `StoreError::condition()`(穷举 match),只陈述事实(哪个目录、什么 mode、意味着什么),
  不带裁决也不带 `chmod` 指令;`credential_health` 的 `directory_problem` 改用它。`Display` 一字未改,
  写路径的报错照旧。测试把两种措辞放在一起断言(`condition_tests`)。
- `usable: no`:`CredentialHealth::lines()` 拆成共享的 `where_lines()` + `what_lines()`,
  `lines()`(auth info,输出逐字节不变)插入复合 verdict 行,新增 `lines_without_verdict()`(doctor)不插。
  两种渲染共用其余每一行,所以将来新增字段不可能只出现在一边;
  `the_verdict_free_rendering_drops_only_the_usable_line` 断言「差异恰好是那一行」。

**F3 —— dangling symlink 被判成「文件不存在/可用」。** `permissions()` 用**跟随链接**的 `fs::metadata`,
dangling 时回 NotFound→`Missing`→`exists=false`,于是 `file_readable = !exists || …` 的 `!exists`
短路把 `loaded=None` 的拒绝信号吞掉;而 `read_checked` 用 `O_NOFOLLOW`,对同一路径报 ELOOP。
修法:`permissions()` 改用 `symlink_metadata`,并对 symlink 单独返回
`Unknown{"it is a symbolic link, and a credential file is never one"}`——即**问与读路径同一个问题**。
于是 dangling/非 dangling symlink 都是 Problem/2,报告说「symbolic link」而不是「does not exist yet」。
`grade` 上那句声称也改成了实际成立的话(并写明它成立**是因为**不跟随链接,而不是天然如此)。

**F5 —— `reachable: true` 说「实例答了」,而请求根本没发出。** `OUTLINE_API_KEY` 含换行时
`EngineError::InvalidRequest` → `Usage(2)`,旧逻辑 `reachable = code != Network` 把它判成 answered。
根因是**用退出码推断一个退出码回答不了的问题**:`Usage` 既是「本地拼不出请求」(未发出)也是
「参数被 spec 拒」(未发出),`Failure` 既是「client 建不起来」(未发出)也是「回包不是 JSON」(发出且答了)。
修法:新增 `errors::server_answered(&EngineError)`(穷举 match,新变体必须自己回答)+
`auth::instance_answered(&AuthError)`(OAuth 失败一律 false——那是 token 端点答的,不是实例),
`reachable` 与 summary 都由它决定。端到端断言 mock **收到 0 个请求**,这才是让这条断言敏感的东西。

**F4 —— 「文件健全(文件不存在)」。** 文件缺失 + 目录 0777 时 Warn summary 无条件拼
`permissions.describe()`,一句话里既说 sound 又说 does not exist。已按 `!health.exists` 分叉。

**F2 —— `auth info --json` 给了判断却不给依据。** `credential_file_usable` 是**复合**值(含目录),
可能因为目录而 false,而同一份 JSON 里没有任何字段能解释它(`directory_problem`/`directory_mode`
原先只在 human 行)。按裁决**加性**补上 `credential_directory` / `credential_directory_mode` /
`credential_directory_problem`,**不改** `credential_file_usable` 的名字或语义(已发布的 semver 面)。
命名不一致(`credential_file_usable` 复合 vs doctor 的 `file_usable` 仅文件)作为已知旧命名问题保留。

R2 修复的变异验证(9 条,全红):

| # | 改坏了什么 | 转红的测试 |
|---|-----------|-----------|
| P1 | detail 又用复合 `lines()` | `a_directory_only_warning_never_claims_the_store_was_refused` + 端到端 0777 |
| P1b | `directory_problem` 又用 `to_string()`(带 refusing) | 端到端 0777 |
| P2 | `auth info` 丢掉解释字段 | `a_directory_problem_is_explained_in_the_same_json` |
| P3 | `permissions()` 又跟随链接 | 3 个(file_guard 单测 + report 单测 + 端到端 dangling) |
| P3b | symlink 按目标 mode 分类 | 同上 3 个 |
| P4 | 缺失文件又被称作 sound | `a_warning_about_the_directory_of_an_absent_file_says_the_file_is_absent` |
| P5 | `reachable` 又由退出码推断 | `a_credential_that_cannot_be_sent_is_not_reported_as_an_answer` |
| P5b | 未发出的请求又描述成 answered | 同上 + `an_unsent_request_is_never_described_as_an_answer` |
| P5c | `server_answered` 一律 true | 3 个(errors/auth 单测 + 端到端) |

R1 的 6 条与最初的 17 条在本轮改动后**全部重跑**,仍全红(M7/N5/N6 的补丁位置随重构更新)。

### 真实 Linux 验证(Docker)与两个**非本 story**的发现

`--lib` 全量 423 个单测(含 doctor 28 + `auth::report` 3)、`doctor_e2e` 17、`doctor_golden` 2、
`credential_hygiene` 17、`credential_paths` 4、`no_phone_home` 10、`limits` / `source_hygiene` /
`portability` / `readme_exit_codes` —— 在 `rust:1.98-slim` 上**全绿**。两个 0777 目录测试与 0644 文件测试
(本轮最需要真机验证的三条)都在真实 Linux 上跑过。

过程中撞到两个与本 story 无关、但会影响别人的问题,已定位到根因,**未擅自修改别人的文件**:

1. **以 root 跑 Docker 会假失败一条既有测试。** `auth::secret_file::tests::
   a_write_into_an_unwritable_directory_reports_failure` 在容器里以 root 运行时失败——root 无视目录权限位,
   于是「往 0500 目录写入必须失败」这个前提不成立。加 `--user "$(id -u):$(id -g)"` 后立刻通过(423/0)。
   `check-all.sh --linux` 目前是 root 跑的。
2. **把 target 目录放在 macOS bind mount 上,会让既有的 `startup_guard` 约 50% 概率 ETXTBSY 失败。**
   `help_/version_works_with_no_spec_file_reachable` 报 `Text file busy (os error 26)`:
   `isolated_otl()` 把二进制 `fs::copy` 到临时目录后立刻 exec,而源在 virtiofs 上时 close 返回后写回可能仍在飞,
   Linux 对「仍被打开写」的文件 exec 就是 ETXTBSY。**已用对照实验确认**:target 放容器内 (`/tmp/lt2`) 连跑两次全绿,
   放 bind mount (`-v .../target/linux:/linux-target`,即 `check-all.sh --linux` 的写法) 连跑两次有一次红。
   本机 macOS 从不复现。也就是说这不是 Linux CI 的问题,而是那条新 gate 的挂载方式的问题;
   最小修法是 copy 之后 `sync_all()` 再 exec,或对 ETXTBSY 重试一次。

### Completion Notes

- 新增 72 个测试（doctor 31 单测 + 19 端到端 + 2 golden + auth::report 7 + auth::error 3 + auth::file_guard 1 + errors 1 + auth 1 + auth::output 2 + speccompile 1 + 其余既有文件内的补充）。
- 不新增退出码；`docs/exit-codes.md` 登记了 doctor 的四条规则，README 的表由它派生（未变）。
- 网络入口零新增：`tests/no_phone_home.rs` 的收敛表未改动。
