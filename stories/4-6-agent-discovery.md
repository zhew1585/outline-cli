# Story 4.6: agent discovery (`otl api describe`, `api list --json`, operation-level `--help`)

Status: review

## Story

As an AI agent (and as any first-time caller) driving `otl api`,
I want the CLI to tell me what an operation takes and returns,
so that I can call it without leaving the terminal for external documentation —
which is the whole reason this CLI exists.

## Context: the three gaps, measured against a real instance

Walked end to end against `https://docs.91aql.com`. Discovery stalled in three places:

1. **No parameter information anywhere.** `otl api list` gave 113 names and a one-line summary each.
   Nothing said `documents.info` takes an `id`. The irony is that the binary already knows: the
   `--no-validate` help text says it skips "local schema-facet checks (enum, numeric bounds, format)",
   and `engine::ir::ParamSpec` carries `name`/`ty`/`required`/`nullable`/`enum_values`/`format`/
   `minimum`/`maximum` while `OpSpec` carries `path`/`summary`/`content_type`/`body_mode`/`params`/
   `response_fields`. **Nothing new had to be computed. The data had no opening.**
2. **`otl api list` ignored `--json`.** Piped — and even with `--json` spelled out — it printed the
   tab-separated terminal form, breaking the dual-state contract on the one path where a program most
   needs structure.
3. **`otl api documents.info --help` silently printed the generic `otl api` help.** The most natural
   probe returned authoritative-looking text that had nothing to do with `documents.info`. This is worse
   than an error: an error makes a caller try something else; a plausible wrong answer does not.

## Acceptance Criteria

1. **Given** `otl api describe <operation>`
   **When** it runs
   **Then** it prints that operation's whole contract — every parameter with `name`/`type`/`required`/
   `nullable`/`enum_values`/`format`/`minimum`/`maximum`, every `response_field`, plus `path`, `summary`,
   `content_type` and `body_mode` — from data already compiled into the binary; no network, no
   credential, no configuration
2. **Given** a spec cache installed by `otl spec sync`
   **When** `describe` runs
   **Then** it describes the **effective** table, the one `api list` and the call path dispatch from,
   and says which of the two answered
3. **Given** stdout is a terminal / is not a terminal / `--json` is passed
   **When** `describe` or `list` runs
   **Then** the first gets a human rendering and the other two get JSON — one object for `describe`,
   one array for `list`
4. **Given** an unknown operation name, with or without `--help`
   **When** `describe` or the call path resolves it
   **Then** the existing message is produced verbatim (naming the `resource.method` convention and
   pointing at `otl api list`) with exit code 2, and **no** generic help is printed instead
5. **Given** `otl api <operation> --help`
   **When** it runs
   **Then** it prints exactly what `otl api describe <operation>` prints, byte for byte; `otl api --help`
   still prints the command's own help, global flags included
6. **Given** any of the above
   **When** it runs with a credential and an instance configured
   **Then** not one byte leaves the machine
7. **Given** spec text (summaries, formats, enumerated values) from a document this CLI did not write
   **When** it reaches stdout in either state
   **Then** it has passed the crate's own hazard filter — no exceptions for `--json` here
8. **No new exit code.** Every failure is an existing code 2

## Tasks / Subtasks

- [x] Task 1: split `commands/api.rs` by responsibility (AC: 1, 3)
  - [x] `api/mod.rs` — CALL an operation (unchanged behaviour; `run` gained the help-renderer argument)
  - [x] `api/list.rs` — WHICH operations exist, both states
  - [x] `api/describe.rs` — WHAT one operation is, both states, plus the shared `safe`/`optional`/
        `body_mode_name` helpers `list` reuses
  - [x] `api/reserved.rs` — what the first positional MEANS, and the collision policy
  - [x] `tests/guard_registry/mod.rs` — the two `--body` file-read exceptions move to `api/mod.rs`
- [x] Task 2: `otl api describe <operation>` (AC: 1, 2, 3, 4)
  - [x] JSON object: `operation`/`summary`/`path`/`content_type`/`body_mode`/`callable`/`paginates`/
        `source`/`parameters[]`/`response_fields[]`
  - [x] human form: header block + one aligned line per parameter and per response field
  - [x] `source` = `built-in` | `synced`, read from `ops::is_synced()`
  - [x] unknown name reuses `reserved::find`, the single lookup the call path also uses
- [x] Task 3: `otl api list` obeys the dual-state contract (AC: 3)
  - [x] JSON array of `{name, summary, path, content_type, body_mode, callable}`
  - [x] terminal form unchanged (`name<TAB>summary`, with the not-callable marker)
- [x] Task 4: operation-level `--help` (AC: 5)
  - [x] `disable_help_flag` on `ApiArgs` + a hand-rolled `-h/--help`
  - [x] `operation` becomes `Option<String>` with `required_unless_present = "help"`
  - [x] `main` passes `Cli::command` (the builder, not the built command) so `otl api --help` renders
        from the real tree with globals propagated
- [x] Task 5: reserved-word policy (AC: 4)
  - [x] `describe` joins `list` as a reserved first positional
  - [x] a synced table that declares an operation by either name gets a stderr warning, not a silent
        shadow
  - [x] `completions` stops spelling the reserved words a second time and offers both
- [x] Task 6: text safety (AC: 7)
  - [x] every string out of the IR goes through `stdio::scrub_terminal_controls` on the way to stdout
  - [x] a test pins that the scrubber's one exception (newlines survive) is vacuous for IR strings
- [x] Task 7: documentation (AC: 8)
  - [x] `README.md`: a "Discovering what to call" section, the quick start, the spec-sync section, and
        the note that `api list`'s pipe output changed (allowed: `otl api` is outside semver)
  - [x] `docs/exit-codes.md`: the discovery paths introduce no code of their own
- [x] Task 8: tests (AC: 1-8)
  - [x] 17 end-to-end (`tests/api_describe.rs`) + 7 end-to-end (`tests/api_list.rs`, rewritten) +
        12 unit + `no_phone_home` extended
  - [x] 24 mutations, 24 red (table in Dev Agent Record)

## Dev Notes

### Design decisions, and what was rejected

**D1 — `otl api describe <op>` as a RESERVED WORD, not a clap subcommand.**
Both occupy the same namespace: `otl api describe` cannot simultaneously be a subcommand and an
operation named `describe`, so a subcommand buys no safety. It costs grammar, though — a struct with
both a required positional and subcommands needs `subcommand_negates_reqs` plus
`args_conflicts_with_subcommands` before it parses at all. `list` already established the reserved
word, and one mechanism for one problem beats two.

*The collision question, answered rather than waved at.* All 113 vendored operation names are
`resource.method` and none is `list` or `describe` (checked; `api_reserved_words_name_no_built_in_operation`
pins it, and mutation M21 confirms that assertion is sensitive). But `spec_compile::is_safe_op_name`
accepts any run of ASCII letters, digits, `.`, `_` and `-`, so a document with a `/describe` path
compiles to an operation named `describe`, and `otl spec sync` will accept documents this project did
not write. Three policies were considered:

- *Operation wins.* Rejected: discovery would disappear because a server chose a name, and it would
  disappear **silently** — the same class of failure this whole story is about.
- *Reserved word wins, silently.* Rejected for the same reason with the roles swapped: an operation
  would become uncallable with nothing said.
- **Chosen: reserved word wins, loudly.** `reserved::warn_if_shadowed` writes one stderr line naming
  the operation and saying the word is reserved. The lookup costs nothing on the normal path, because
  only the two reserved words ever ask. Tested end to end by syncing a document with a `/describe`
  path AND one with a `/list` path — both words, because a shared helper is exactly the kind of thing
  that gets called from one branch and not the other — and the same test asserts the ordinary case
  stays silent for both.

**D2 — `list --json` carries `path`/`content_type`/`body_mode`/`callable`, and stops there.**
`callable` is not optional: the terminal form flags operations the generic client cannot call, and the
structured form must not be the one that says less. The terminal form also names the CONTENT TYPE that
makes an operation uncallable, so `content_type` and `body_mode` come along for the same reason — "not
less than the text" is a rule about every fact in the text, not just the flag. (The first version of
this change carried only `body_mode`, which said "no" without saying why; `an_operation_that_cannot_be_called_is_flagged_in_both_states`
now asserts the content type in the JSON row appears in the corresponding text line, so the two forms
cannot diverge again.) `path` is what an operation *is* on the wire and costs one short string.

Parameters were deliberately left out, and size is the weaker half of the argument (the full contract
of all 113 operations is roughly a hundred times this payload). The stronger half: a **partial**
contract is the more dangerous object. A list of parameter names with no types, no facets and no
response shape reads like the answer while being a fragment of it — which is gap 3 wearing different
clothes. Two steps (`list` to triage, `describe` for the one operation chosen) keep every contract
published in exactly one place and complete.

**D3 — `otl api <op> --help` DESCRIBES rather than erroring.**
The brief allowed either. Describing is what the caller was asking for, and an alias costs one branch.
The price is real and worth naming: clap's help flag had to be disabled for this subcommand, the
operation positional had to become `Option<String>` with `required_unless_present = "help"`, and `main`
now hands `api::run` a `fn() -> clap::Command`. That last piece is what keeps `otl api --help`
identical to what clap used to print — it is rendered from the real command tree after `Command::build`,
which is what propagates `--json`/`--profile`/`--url`/`--config` down into the subcommand. Rejected
alternative: rebuild the help from `ApiArgs::augment_args` inside the library. It needs no plumbing and
silently loses all four global flags, and it would be a second copy of the help text free to drift.

Two visible costs, recorded rather than glossed:

- `otl api --help` now shows `[OPERATION]` in its usage line instead of `<OPERATION>`, because the
  argument really is optional to clap. `otl api` with no arguments still reports it as required
  (`required_unless_present` is what clap checks then), so the misleading form appears only in the
  help text itself.
- `otl api -h` and `otl api --help` now print the same LONG help. clap's convention is short for `-h`
  and long for `--help`, and one boolean cannot tell the two apart. Distinguishing them would mean two
  flags whose only difference is verbosity, on a command whose `-h` is now mostly a way to ask about an
  operation; long help is the more useful of the two answers.

**D4 — `describe` describes the EFFECTIVE table.**
`ops::table()` resolves to the synced cache when one is usable and to the built-in table otherwise, and
both `api list` and the call path already dispatch from it. Describing anything else would hand a caller
a contract that disagrees with what the very next command does. `source` (`built-in`/`synced`) says which
answered, so a surprising contract is traceable instead of mysterious. Mutation M8 (look up in the
built-in table) turns four tests red, M7 (`source` always `built-in`) one.

**D5 — `--json` is scrubbed here, unlike a response payload.**
`crate::text`'s module documentation states an exemption: `--json` emits exactly what arrived, bidi and
all, and `json_mode_is_exempt_from_hazard_scrubbing` pins it. That exemption is **about a server
response round-tripping byte for byte** — its stated justification is `render_golden`'s JSON round-trip
test. `describe`'s JSON is not a response; it is a document `otl` writes about a third-party spec,
nothing round-trips it, and its intended reader is a program that will put the text in front of a
language model. So it is scrubbed, in both states.

The layering is worth stating precisely, because "we already validate this" was nearly the answer:

- Both IR entry points already filter this text. `spec_compile` sanitizes display text and **rejects**
  a document whose meaningful text (parameter names, content types, formats, enumerated values) carries
  a dangerous character; `crate::spec` applies the same `is_display_safe` to every string of a cache it
  loads. So no control character, newline, `U+2028`/`U+2029`/`U+FEFF` or bidi override/isolate can be
  in the IR at all.
- That table is nevertheless a strict **subset** of `crate::text::hazard` (the whole assigned `Cf`
  category, which `crate::text`'s own documentation says review found short three times running).
  `U+200E`/`U+200F`/`U+061C`, the `U+206A..U+206F` block, `U+00AD`, `U+180E` and the `U+13430` block
  pass the compiler and fail this crate's filter. Two tables that disagree is the exact defect that
  module exists to prevent.
- Unifying them at the source would mean giving `spec-compile` the engine's table. It cannot have it:
  it is a build dependency and must not pull `engine` (and reqwest/rustls) into the host build. So the
  gap is closed at the sink, with `stdio::scrub_terminal_controls` — an **existing** path that matches
  exhaustively on `Hazard`, so a category added later must be answered rather than silently forwarded.
  No fourth scrubbing policy was created.
- That scrubber's one exception is that newlines survive. It is vacuous for this input, because
  `is_display_safe` rejects every control character at both entry points — and
  `no_ir_string_carries_a_newline` asserts it, so the exception cannot quietly become a hole.
- `api list`'s summaries now go through the same filter. They did not before; the residual set above
  could reach a terminal through them.

**D6 — no `description` field, and this is a deliberate gap.**
The OpenAPI document carries per-parameter prose ("Either the UUID or the urlId is acceptable" for
`documents.info`'s `id`), and it is exactly what an agent would want. It is **not in the IR**, so
exposing it is not opening a door — it is a new capability: `ParamSpec` would need a field,
`IR_SCHEMA_VERSION` would go from 5 to 6 and **invalidate every user's existing spec cache**, and the
text would cost binary size on a target with ~430 KB of NFR2 headroom left. Out of scope for a story
whose premise is "the data is already there". The README says plainly that `describe` does not print it.

**D7 — the terminal form uses `render_pairs`, not `render_columns`.**
`render_columns` truncates a cell at 40 display columns; a 40-column enumeration of allowed values is a
contract with values missing. `render_pairs` lays out label/value pairs and does not truncate. Length
is bounded upstream anyway (200 characters for a summary, 256 bytes per enumerated value, 64 for a
format, 128 for a name). Note that `render_pairs`'s own `scrub_control_chars` is weaker than the hazard
table — which is why every value is passed through `safe` *before* it gets there, rather than relying
on the renderer.

**D8 — `paginates` is in the output.**
`--limit` is refused on operations that do not paginate, so "does this paginate" is part of "how do I
call this". It is computed from the same `paging::spec_for` the call path uses, and
`pagination_is_reported_and_agrees_with_the_call_path` walks the whole table asserting the two agree —
so the description cannot drift from the behaviour.

**D9 — no engine change at all.**
Every field `describe` prints was already `pub` on `OpSpec`/`ParamSpec`/`FieldSpec`. Formatting,
wire names (`key_value`/`raw_json_only`/`unsupported`) and the human sentences all live in `otl`. The
`BodyMode` wire names are spelled out in an exhaustive match rather than derived from the enum, so a
variant renamed in `engine` is a compile error here instead of a silently changed public string.

### Breaking change, stated

`otl api list` piped to a program now emits a JSON array instead of tab-separated lines. That is the
fix, not a side effect, and `otl api` output is explicitly outside semver (README "Stability and
versioning"). Recorded in the README so a script that parsed the columns knows what to do.

### Red lines this story touches

Not printing credentials (`describe` reads none — the local paths return before `auth::client` is ever
built); HTTP only through the existing channels (**zero** new entry points; `tests/no_phone_home.rs`'s
convergence table is unchanged, and its behavioural case gained the two new commands); text scrubbing
(D5); exit codes are public API (none added); no `unwrap`/`expect` in the library; files under 800
lines and functions under 50; no runtime OpenAPI parsing (`describe` reads the compiled IR — the
`spec sync` module is untouched).

### Project Structure Notes

```
crates/otl/
  src/commands/api.rs        -> src/commands/api/mod.rs   # calling an operation
  src/commands/api/list.rs                                # which operations exist
  src/commands/api/describe.rs                            # what one operation is
  src/commands/api/reserved.rs                            # what the first positional means
  src/commands/completions.rs                             # reserved words imported, not respelled
  src/main.rs                                             # passes `Cli::command` to api::run
  tests/api_describe.rs                                   # new, 17 end-to-end
  tests/api_list.rs                                       # rewritten for the dual state
  tests/guard_registry/mod.rs                             # two file-read exceptions repathed
  tests/no_phone_home.rs                                  # local-command list extended
  tests/spec_sync_e2e.rs                                  # two assertions that counted TSV lines
README.md, docs/exit-codes.md
```

The split is by responsibility, not by line count: `api/mod.rs` would have reached roughly 700 lines
otherwise, but the reason to split is that "call an operation", "enumerate operations", "describe one
operation" and "decide what the caller meant" are four questions with four different answers. Final
sizes: `mod.rs` 396, `describe.rs` 542, `reserved.rs` 233, `list.rs` 160.

### References

- [Source: project-context.md — engine stays service-agnostic, one request channel, no runtime OpenAPI
  parsing, dual-state output, text scrubbing, file/function limits]
- [Source: docs/exit-codes.md — the authority; README's table is derived and checked by
  `tests/readme_exit_codes.rs`]
- [Source: stories/4-2-spec-sync.md, stories/4-3-doctor.md — the effective-table and cache semantics
  `describe` has to agree with]
- [Source: crates/engine/src/ir.rs — `OpSpec`/`ParamSpec`/`FieldSpec`, IR schema version 5]

## Dev Agent Record

### Agent Model Used

claude-opus-5 (Claude Code agent), 2026-08-27

### Mutation verification (break the behaviour → confirm red → restore)

Driven by a script that patches one anchor, runs `cargo test --no-fail-fast` over `--lib`,
`api_list`, `api_describe`, `spec_sync_e2e`, `no_phone_home` and `startup_guard`, records the failures
and restores the file. **24 of 24 turned red.**

`--no-fail-fast` matters and is worth recording: the first run of this table was done without it, and
cargo stopped at the first failing target, so several mutations were reported as covered by fewer
tests than actually catch them (M14 looked like 1 red when it is 2). A mutation table built on a
fail-fast run understates coverage and can hide a mutation that is only caught by a later target.

| # | What was broken | Red | Which tests |
|---|-----------------|-----|-------------|
| M1 | `list` ignores the output mode (always the terminal form) | 6 | 4 × `api_list` + `a_document_with_terminal_escapes_is_neutralized` + `a_synced_spec_replaces_the_built_in_one_entirely` |
| M2 | `list` JSON always says `callable: true` | 2 | `an_operation_that_cannot_be_called_is_flagged_in_both_states`, `api_list_flags_operations_that_are_not_callable` |
| M3 | the human form of `describe` renders nothing | 2 | `the_human_form_names_the_operation_and_every_parameter`, `an_uncallable_operation_says_so_in_both_states` |
| M4 | `describe` drops `enum_values` | 2 | `numeric_bounds_and_enumerations_reach_the_output`, `describe_prints_the_enumerations_local_validation_enforces` |
| M5 | `describe` drops `minimum`/`maximum` | 1 | `numeric_bounds_and_enumerations_reach_the_output` |
| M6 | `describe` drops `response_fields` | 2 | `the_json_form_carries_the_response_shape`, `describe_prints_the_response_shape` |
| M7 | `source` always reports `built-in` | 1 | `describe_answers_from_the_effective_table_not_the_built_in_one` |
| M8 | lookups answer from the built-in table, not the effective one | 4 | the three synced-table tests + `sync_makes_a_new_endpoint_available_to_api_list` |
| M9 | a shadowed reserved word warns nobody | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M10 | spec text reaches stdout unscrubbed | 2 | `a_hostile_string_is_neutralised_in_both_states`, `third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M11 | `--help` on an unknown operation falls back to the generic help | 1 | `help_on_an_unknown_operation_is_an_error_not_a_generic_help` |
| M12 | `--help` always prints the generic help (**the original defect**) | 3 | `operation_level_help_describes_that_operation`, `the_short_help_flag_describes_too`, `help_on_an_unknown_operation_...` |
| M13 | the generic help is rendered from an unbuilt command tree (globals lost) | 2 | `command_level_help_still_works_and_names_both_reserved_words`, `help_on_a_reserved_word_prints_the_command_help` |
| M14 | request flags are accepted on a local path | 2 | `describe_rejects_request_flags`, `api_list_rejects_request_flags` |
| M15 | extra positionals are accepted on a local path | 2 | `describe_needs_exactly_one_operation`, `api_list_rejects_extra_arguments` |
| M16 | `describe` with no operation silently picks one | 1 | `describe_needs_exactly_one_operation` |
| M17 | `describe` is routed to the network call path | 11 | all 5 `describe_*` + `discovery_sends_nothing_even_when_it_could` + `a_local_command_works_with_every_outbound_route_dead` + 4 more |
| M18 | an absent facet becomes `""` instead of `null` | 1 | `an_absent_facet_is_null_rather_than_an_empty_string` |
| M19 | `paginates` is always `false` | 2 | `pagination_is_reported_and_agrees_with_the_call_path`, `describe_says_whether_an_operation_paginates` |
| M20 | a real operation wins over the reserved word | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M21 | a reserved word collides with a real operation name (`list` = `documents.info`) | 21 | `api_reserved_words_name_no_built_in_operation` + 20 others |
| M22 | the `describe` candidate is dropped from the completion scripts | 1 | `operation_candidates_cover_the_whole_ir_table` |
| M23 | the `list` JSON drops `content_type` (says "no" without saying why) | 1 | `an_operation_that_cannot_be_called_is_flagged_in_both_states` |
| M24 | the reserved-word warning fires only for `describe`, not `list` | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |

Three assertions were written specifically so they could not pass vacuously, following the lesson from
Story 4.3's M14:

- `discovery_sends_nothing_even_when_it_could` sets **both** `OUTLINE_URL` (at a live wiremock) and
  `OUTLINE_API_KEY`, so there is something to send, and ends with a **control call** (`otl api
  auth.info`) that asserts the mock recorded exactly one request. Without that control, "the mock
  received nothing" would also be true of a mock that records nothing.
- `third_party_text_reaches_stdout_scrubbed_in_both_paths` ends by reading the compiled cache file off
  disk and asserting the `U+200F` bytes are **in** it. Without that control the test would pass just as
  well against a document that never carried the character — which is how a scrub test goes quiet.
- `an_operation_that_cannot_be_called_is_flagged_in_both_states` asserts the set of flagged operations
  is non-empty before comparing the two states, and compares the counts rather than spot-checking.

One assertion was rewritten mid-flight for the same reason: the completions test first said
`script.contains("describe")`, which passes with no candidate emitted at all, because the word also
appears in the `--help` flag's own description that the generator embeds. It now asserts the
shell-specific **candidate form** (`list describe ` for bash/zsh, `-a "describe"` for fish), and M22
confirms that version is sensitive.

### Gates (measured, exit status captured before any pipe)

`bash scripts/check-all.sh --windows` — see the report accompanying this story for the run. `--linux`
was deliberately not run (the user asked to skip local Linux; Docker cannot start containers on this
machine, and CI has native Linux and Windows legs).

### Binary size

Measured with `scripts/check-binary-size.sh` (`--profile dist`), both branches built the same way on
the same machine:

| target | `develop` | this branch | delta |
|--------|-----------|-------------|-------|
| aarch64-apple-darwin | 3,464,608 | 3,481,184 | **+16,576 B** (+0.48%), 92% of budget, 69% of NFR2 |
| x86_64-apple-darwin | 4,007,712 | 4,028,216 | **+20,504 B** (+0.51%), 93% of budget, 80% of NFR2 |

Both published darwin targets pass. `x86_64-unknown-linux-musl` and `x86_64-pc-windows-msvc` can only
be measured by CI (MSVC cannot be linked off Windows, and the musl toolchain is not installed here).
Extrapolating with the platform factor the script's own header derives (musl ≈ 1.14 × x86_64-darwin),
musl should land near **4,575,000 B** — about **93%** of its 4,920,000 budget and **91.5%** of the
5,000,000 NFR2 promise, leaving roughly **425,000 B** of headroom where 448,000 B stood before.

**No budget constant was changed.** The growth is small and expected: `response_fields` were already
reachable (the table renderer ranks columns from them), so nothing was un-optimized-away; the delta is
the new rendering code, the JSON assembly and the string literals.

### Known gaps, left deliberately

- **Per-parameter `description` prose is not printed** (D6): it is not in the IR, and putting it there
  means `IR_SCHEMA_VERSION` 6, which invalidates every user's spec cache.
- **`pattern` is still not compiled** (pre-existing `ir.rs` TODO: a regex engine costs ~1 MB against a
  5 MB budget for two constraints in the whole vendored spec). `describe` therefore cannot report it,
  and does not pretend to.
- **The terminal form of `api list` is still TSV**, not an aligned table. It is readable, it is what
  users have today, and changing it was not asked for. The dual-state contract is satisfied either way.
- **The compiler's text table is still narrower than the crate's** (D5). Closing it at the source
  needs the hazard table in a crate `spec-compile` may depend on; this story closes it at the sink for
  the two surfaces it owns. `otl api`'s validation diagnostics ("allowed values are …") take a
  different path and were not touched.

### Completion Notes

- 23 new end-to-end tests plus 12 new unit tests; the workspace suite goes from 1,218 to 1,252 passing (+34).
- No new exit code; `docs/exit-codes.md` gained a note and the README's generated table is unchanged.
- No engine change, no `no_phone_home.rs` convergence-table change, no binary-size budget change.
