# Story 4.6: agent discovery (`otl api describe`, `api list --json`, operation-level `--help`)

Status: done

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
9. (R1) **Given** the OpenAPI document states a parameter's meaning only in prose
   **When** `describe` runs
   **Then** that prose is printed, sanitized through the same path a summary takes — and a cache written
   by a build with the previous IR schema is reported as *outdated*, discarded, and rebuildable with
   `otl spec sync`, with no command failing in the meantime
10. (R2) **Given** any `otl auth` result
    **When** it is printed in either state
    **Then** every string in it - the profile name from a config file, the path from the environment, and
    the `account`/`workspace`/`scope` the SERVER supplied - has been scrubbed, with the published field
    names, their order and the human line structure unchanged

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
  - [x] 19 end-to-end (`tests/api_describe.rs`) + 6 (`tests/api_list.rs`, rewritten) + 5
        (`tests/ir_upgrade.rs`) + 17 unit + `no_phone_home` / `spec_sync_e2e` / `completions` extended
  - [x] 36 mutations, 36 red (table in Dev Agent Record, emitted by the harness)
- [x] Task 9 (R1): the `--help` alias refuses what `describe` refuses (F3)
- [x] Task 10 (R1): one renderer for JSON `otl` authors; `doctor` uses it; the `crate::text` exemption
      narrowed to "a server response", and the rule recorded in `project-context.md` (F1)
- [x] Task 11 (R1): the false Known-gaps entry about validation diagnostics corrected, in the story and
      in `describe.rs` (F2)
- [x] Task 12 (R1, user scope): `ParamSpec::description`, `IR_SCHEMA_VERSION` 5 -> 6, sanitized through
      the one existing display-text path, with the migration path driven end to end
- [x] Task 13 (R2): `otl auth`'s output scrubbed in BOTH states, with the one-line fold shared out of
      `doctor` into `stdio::scrub_to_one_line`, `emit` split so the bytes are testable, and the first
      terminal-safety assertions that module has ever had (F3)
- [x] Task 14 (R2): `doctor/report.rs`'s module header no longer contradicts its own `emit` (F1)
- [x] Task 15 (R2): `cache.rs`'s doc link retargeted from the split-away `decode_table` (F2)
- [x] Task 16 (R2): the mutation table is emitted by the harness instead of transcribed, after three
      rounds of counting errors (F4)

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
- **Every consumer of that text closes the gap at its own sink, and R1 review established that `otl
  api`'s validation diagnostics already did.** The story originally recorded them as an open hole
  ("`allowed values are …` still quotes enum values unfiltered"). That was factually wrong, and wrong in
  the safe direction: a validation failure travels `EngineError::InvalidParamValue` → `CliError` →
  `main` → `stdio::write_diagnostic_line`, which applies the FULL crate hazard table, not the
  compiler's narrow one. Verified, not inferred — a synced document whose enum value carries `U+200F`
  and `U+206A` produces `error: invalid value for parameter "mode": allowed values are: ok, bad`, both
  characters gone.

After R1, the JSON side is scrubbed **once, at the sink** (`render::render_json_scrubbed`) rather than
field by field in the builders. Same argument `crate::stdio` makes for scrubbing diagnostics at the
sink: it holds for fields that do not exist yet, whereas a per-field call has to be remembered by
whoever adds the next field. The human form still scrubs per value, because there layout is interleaved
with the text and there is no single string to clean at the end.

**D6 — parameter `description` prose IS printed, and the IR went to schema version 6 for it.**
This reversed during R1 review. The original decision was to leave the prose out (it is not in the IR,
so exposing it is a new capability rather than opening a door) — and that was wrong for a reason neither
the story nor the first review caught: without it, `describe` is faithful to the spec and **misleading
about the API**.

Measured on the compiled table: of the 109 operations that take parameters, **29 mark none of them
required**. `documents.info` is one — `id` and `shareId` are both `required: false`, and one of them
must be sent. The upstream document states no `required` array and no `oneOf`, so the looseness is
upstream's, not this CLI's; but the disambiguation is right there in the prose ("Either the UUID or the
urlId is acceptable", "a shareId may be used in place of a document UUID"). An agent reading
`required: false` on every parameter concludes that nothing has to be sent, which is worse than having
no answer. 23 of the 29 carry prose that resolves it; the remaining 6 are as loose upstream as they
look, and `describe` does not invent a constraint the spec never stated.

So: `ParamSpec::description`, `IR_SCHEMA_VERSION` 5 → 6. Four consequences, each handled rather than
assumed:

- **It is sanitized, not validated**, through the same `text::sanitize_display` a summary goes through:
  dangerous characters dropped, whitespace folded to one line, length capped at 200 characters. There is
  exactly one construction site (`schema.rs::compile_param`), so there is no second, unfiltered entry
  point — which was the explicit requirement, this being the only new injection surface in the change.
  A document is therefore ACCEPTED with a hostile description and the text is stripped, which is how a
  summary behaves; text with MEANING still refuses the document. Pinned end to end in
  `spec_sync_e2e::a_document_with_terminal_escapes_is_neutralized`, and the hostile-document fixture in
  `api_describe.rs` now aims all seven residual `Cf` classes at the description specifically.
- **It follows `$ref`/`allOf`**, because it is collected through the existing facet walk rather than
  read off the property. First declaration wins, like `enum` and `format`: a property's own prose is
  more specific than that of the shared schema it points at.
- **Request parameters only; response fields deliberately not.** Measured on the vendored document:
  244 request-parameter descriptions total **16,055 bytes**, while 942 response-field descriptions
  total **53,024 bytes** — 3.3× more, on whichever target binds NFR2. And the asymmetry in value runs
  the other way: the caller's problem is "which of these must I send", which is the request side. A
  response field's name and type are enough to consume a response. If the response prose is wanted
  later it is an additive change to `FieldSpec` and another version bump.
- **The cache migration is real and is tested rather than reasoned about.** See D10.

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

**D10 — the cache checks its schema version BEFORE it decodes any operation record** (R1, from the
version bump).
This was a latent defect the bump would have activated. `load_at` used to decode the whole framed table
and only then compare versions. Operation records are `bincode`: positional, no field names. Reading a
v5 `ParamSpec` as a v6 one takes the next parameter's bytes as a string length for `description`, so a
genuinely old cache would have been reported as **damaged** — sending a user to look for corruption
that is not there — and, with unlucky byte alignment, could in principle have decoded into a table that
is merely wrong.

Fix: `bounded::decode_table` splits into `decode_meta` (the provenance record, which is the first thing
in the body, self-delimiting and length-bounded) and `decode_ops`. `load_at` decodes the metadata,
checks the versions, and only then asks for the operations — so a table from another version is never
interpreted at all. Rejected alternative: keep the single decode and treat the resulting error as
staleness. It infers the cause from a symptom, and it would still have interpreted the records.

**D11 — the migration path is driven, not argued** (R1 requirement).
`tests/ir_upgrade.rs` writes a real v5 cache: the v5 structs are mirrored field for field and variant
for variant and serialized with the same encoder, because there is no way to ask the current types for
the previous layout, and a test that wrote a CURRENT record and only lowered the version number in the
metadata would prove nothing — those bytes still decode. Five cases: commands keep working on the
built-in table; the warning says *outdated*, not damaged, and names `spec sync`; `describe` falls back
the same way `list` does; `spec sync` rebuilds and the new field is there; `spec reset` clears a file
this build cannot read.

**D9 — the engine changes only by one data field, and stays Outline-agnostic.**
Before R1 there was no engine change at all. The version bump adds one field to `engine::ir::ParamSpec`
— a plain `Cow<'static, str>` on a data table, with no Outline-specific content, no behaviour and no
new API. Everything else `describe` prints was already `pub` on `OpSpec`/`ParamSpec`/`FieldSpec`.
Formatting,
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

One more after R2, and it is the interesting one: **the rule this story introduced then failed to apply
to a module it did not think it was touching.** `otl auth`'s output was already unscrubbed before this
branch existed, so it is not a regression — but the moment the story wrote "always" into
`project-context.md`, `text.rs` and `render.rs`, the exception became this story's problem. A rule stated
as universal has to be true universally, or the statement is the defect. Checked by hand after the fix:
the remaining `render_json` call sites are `render.rs`'s own definition and `render::render`'s response
path, which is the one the exemption is about.

### Project Structure Notes

```
crates/engine/
  src/ir.rs                             # + ParamSpec::description, IR_SCHEMA_VERSION 5 -> 6
crates/speccompile/
  src/lib.rs                            # + CompiledParam::description
  src/schema.rs                         # collected through the facet walk, sanitized at the one site
crates/otl/
  build.rs                              # renders the new field; version constant 5 -> 6
  src/commands/api.rs -> api/mod.rs     # calling an operation
  src/commands/api/list.rs              # which operations exist
  src/commands/api/describe.rs          # what one operation is
  src/commands/api/reserved.rs          # what the first positional means (+ the --help alias checks)
  src/commands/completions.rs           # reserved words imported, not respelled
  src/commands/auth/output.rs           # R2: both states scrubbed; `emit` split so it is testable
  src/commands/doctor/report.rs         # self-authored JSON scrubbed; `emit` split so it is testable
  src/stdio.rs                          # + scrub_to_one_line, shared by doctor and auth
  src/render.rs                         # + render_json_scrubbed (one renderer for authored JSON)
  src/spec/bounded.rs                   # decode_table -> decode_meta + decode_ops
  src/spec/cache.rs                     # versions checked BEFORE any operation record is decoded
  src/spec/mod.rs                       # converts and validates the new field
  src/text.rs                           # the --json exemption narrowed to "a server response"
  src/main.rs                           # passes `Cli::command` to api::run
  tests/api_describe.rs                 # new, 19 end-to-end
  tests/ir_upgrade.rs                   # new, 5 end-to-end: the v5 -> v6 migration path
  tests/auth_output_terminal.rs         # new, 2 end-to-end: a hostile profile name through the binary
  tests/api_list.rs                     # rewritten for the dual state
  tests/spec_sync_e2e.rs                # + a hostile parameter description
  tests/{guard_registry,no_phone_home,completions,spec_cache*,...}
README.md, docs/exit-codes.md, project-context.md
```

The split is by responsibility, not by line count: `api/mod.rs` would have reached roughly 700 lines
otherwise, but the reason to split is that "call an operation", "enumerate operations", "describe one
operation" and "decide what the caller meant" are four questions with four different answers. Final
sizes, all inside the 800-line limit: `mod.rs` 396, `describe.rs` 642, `reserved.rs` 265, `list.rs` 174;
`cache.rs` 638, `bounded.rs` 536, `render.rs` 591, `tests/api_describe.rs` 522, `tests/ir_upgrade.rs`
296, `tests/spec_sync_e2e.rs` 638. `describe.rs` is the one to watch — the split that would come next is
the human rendering away from the JSON assembly.

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

claude-opus-5 (Claude Code agent), 2026-08-27 (initial + R1 response)

### R1 adversarial review, and what it changed

R1 verdict: mergeable, no BLOCKER, no MAJOR, four MINOR. The reviewer built a hostile OpenAPI document
carrying `U+200F`/`U+200E`/`U+061C`/`U+206A`/`U+00AD`/`U+180E`/`U+200B`, synced it, and checked all three
entry points in both states plus a PTY run through `cat -v`; it also verified the zero-network claim with
dead routes independently. All four findings are fixed, each with a mutation behind it.

**F3 [MINOR] — the `--help` alias swallowed extra arguments and request flags.** The only real
behavioural divergence, and it was this story's own defect in the flag's clothing: `otl api
documents.info --help id=x` returned a contract and silently discarded `id=x`, while `otl api describe
documents.info id=x` exits 2. Fixed by making the alias an alias in both directions — `help_for` now
runs `reject_request_flags` and `reject_extra_arguments(cmd, _, 0)`.

Two details came out of writing the fix. The rejection messages take the INVOCATION rather than the
reserved word, because "`otl api documents.info` sends no request" is false of the command that name
usually spells. And the `--help` branch resolves the NAME before checking the invocation shape, which is
the opposite order from the `describe` branch: there the positional count decides which name there even
is, while here the name is fixed, so with both a typo and a stray argument present the typo is the root
cause and the more useful thing to report.

Why no existing test caught it: AC 5 pinned only that the success paths are byte-identical, and
`describe_needs_exactly_one_operation` / `api_list_rejects_request_flags` both exercise the paths
WITHOUT `--help`, which the alias short-circuits ahead of. The new assertions are on the combinations
(`--help` + stray positional, `--help` + each request flag).

**F1 [MINOR] — `otl doctor`'s self-authored JSON was not scrubbed.** The reviewer's ruling was that this
story's reading is right and doctor's was wrong, so the fix lands in three places rather than one:

1. `render::render_json_scrubbed` — one renderer for JSON that `otl` AUTHORS, scrubbing every string
   (and object key) at the sink. `doctor::report::emit` and `api describe` both use it; `render::render`
   (a server response) stays exempt.
2. `crate::text`'s exemption paragraph is narrowed from "`--json`" to "a SERVER RESPONSE in `--json`",
   states explicitly that JSON `otl` writes is not covered, and records that both surfaces once got this
   wrong by analogy. `project-context.md` gains the same rule, since the question recurs every time a
   command grows a `--json` summary.
3. `emit` is split so the bytes it writes are testable: a test that called `render_json_scrubbed`
   directly would keep passing if `emit` stopped calling it.

Exposure was nil in practice — doctor's only third-party strings are operation names and origin hosts,
both ASCII-constrained — but two `--json` policies in one binary is exactly the drift `crate::text`
exists to prevent.

**F2 [MINOR] — a Known-gaps entry was false.** The story claimed `otl api`'s validation diagnostics
still quoted enum values unfiltered. They do not: that path runs through
`stdio::write_diagnostic_line`, i.e. the full crate hazard table. Re-measured here (`allowed values are:
ok, bad`, both `Cf` characters stripped) and corrected in both the story and `describe.rs`'s module
documentation. The bullet is rewritten rather than deleted, because a false gap is a standing invitation
to "fix" something that is not broken.

**M1's red count was wrong (reported as 6, actually 7).** The second counting error in the table, after
the one already confessed. Root cause both times: the first run was made without `--no-fail-fast`, so
cargo stopped at the first failing target and later targets never ran. The whole table was re-measured
rather than patched row by row — and then miscounted again, which is why R2's F4 stopped treating it as
an arithmetic problem. See the head of the mutation section.

### Scope added by the user during review: parameter descriptions in the IR

See D6, D10 and D11. In short: `describe` was faithful to the spec and misleading about the API on the
29 operations that mark no parameter required, the disambiguation was sitting in prose the IR did not
carry, so `IR_SCHEMA_VERSION` went 5 → 6 with `ParamSpec::description`. Request parameters only
(16,055 bytes vs 53,024 for response fields). Sanitized through the one existing display-text path, with
the hostile fixture aimed at it. The cache-invalidation path is driven by `tests/ir_upgrade.rs` against
a real v5 cache, and a latent ordering defect it would have activated is fixed (D10).

### R2 adversarial review, and what it changed

R2 verdict: mergeable, no BLOCKER, no MAJOR, four MINOR. The reviewer drove the cache reader with
malformed files against the real binary — truncation at six offsets, oversized length prefixes for the
metadata record / an operation record / the operation count, a count at exactly the limit with too few
bytes behind it, trailing bytes, a metadata record with its own trailing bytes, and forged schema
versions 5/7/200 — all refused before allocation, no panic, no silently wrong table. It also byte-checked
that the `decode_table` split was verbatim, re-derived the v5 record layout from `git show
5ded310:crates/engine/src/ir.rs`, and independently re-encoded v5 and v6 caches to drive the real binary.

Two of my own stated risks were disproved, both in the safe direction, and both are recorded because a
retracted worry is as useful as a confirmed one:

- **`decode_meta` cannot be used to trust a table.** It is `pub(super)`, has one call site, and returns
  `(CacheMeta, usize)` — no operations. The path I was worried about does not exist in the type.
- **A description cannot forge a facet line.** `sanitize_display` folds it through `split_whitespace`
  at compile time and `is_display_safe` refuses controls on the cache path, so it cannot carry a
  newline; a PTY run confirms a hostile description produces one line, indented into the value column.

The size arithmetic was reproduced independently and closes (`Cow<str>` 24 B, 383 `ParamSpec ` entries,
244 non-empty descriptions, 16,047 B by the reviewer's unescaping against my 16,055 — escape noise).

**F3 [MINOR, fixed properly rather than documented away] — `otl auth`'s output was not scrubbed.**
`commands/auth/output.rs::emit` still used `render_json` for JSON and joined its human lines raw, while
this story had asserted "always" in three places (`project-context.md`, `text.rs`, `render.rs`). The
reviewer measured `U+202E` in a config-file profile name arriving byte-for-byte in both states.

The reviewer offered narrowing the documentation instead; the user required the fix, and the reason is
the right one: `account`, `workspace` and `scope` in that output come from the **server**, and `otl auth
info` is the command a program runs to learn whether it has a credential. That is not a third pending
path, it is the scenario the rule exists for. Narrowing the docs would have demoted a live hole to a
known gap.

- JSON → `render_json_scrubbed`. Human → per line through a new shared `stdio::scrub_to_one_line`.
- The one-line fold was already in `doctor::report::human_line`; rather than copy it, it moved to
  `stdio` and `doctor` delegates. Both surfaces that assemble their own line LIST need it, and a second
  copy is how one of them ends up without it — which is the shape of this very finding.
- `emit` split into a testable `rendered()`, for the same reason it was in `doctor`: a test that called
  `render_json_scrubbed` itself would keep passing if `emit` stopped calling it. That is exactly how
  this gap survived R1.
- **The semver surface is untouched**: scrubbing removes hazard characters and renames, drops, adds and
  reorders nothing. `both_renderings_scrub_foreign_text` asserts the key set and order are identical to
  the unscrubbed object, and the e2e checks the published field names are all still there.
- Why it hid: `auth/output.rs` had **no assertion of this shape at all** — every existing test there was
  about credential leakage or about field presence. Two were added, at both levels: a unit test
  rendering both states with a hostile profile name AND server-supplied `account`/`workspace`/`scope`,
  and `tests/auth_output_terminal.rs`, which drives the real binary against a config file whose default
  profile name carries `U+202E`/`U+200F`/`U+061C` (with a control asserting the character really is in
  the file, so "no hazard on stdout" is not "no profile name on stdout").

**F1 [MINOR] — `doctor/report.rs`'s module header contradicted its own implementation.** It still said
"`--json` is exempt … JSON is the payload, not a rendering" two screens above an `emit` that had been
switched to `render_json_scrubbed`. Rewritten rather than deleted: it now names the sentence that used
to stand there and why it is false of a report `otl` writes, because leaving a ready-made justification
next to the code is how a fix gets reverted.

**F2 [MINOR] — a broken doc link.** `cache.rs::checked_body` pointed at `super::bounded::decode_table`,
which this story split into `decode_meta`/`decode_ops`. Retargeted to `decode_meta`, with a note saying
what `decode_table` was, so a reader chasing "why is the order load-bearing" is not sent after a symbol
that no longer exists. (Both F1 and F2 are invisible to the gate: `cargo doc` runs without
`-D warnings`. Worth adding — deliberately NOT in this round, per the user.)

**F4 [MINOR] — the mutation table miscounted for the third time.** See the note at the head of the
mutation section: the fix is to stop transcribing it by hand, not to correct two more numbers.

### Mutation verification (break the behaviour → confirm red → restore)

Driven by a script that patches one anchor, runs `cargo test --no-fail-fast` over thirteen targets
(`--lib`, `api_list`, `api_describe`, `spec_sync_e2e`, `no_phone_home`, `startup_guard`, `ir_upgrade`,
`completions`, `doctor_e2e`, `doctor_golden`, `spec_cache`, `spec_cache_rejects`,
`auth_output_terminal`), records the failures and restores the file. **36 of 36 turn red.**

#### How this table is produced, after miscounting three times

The counts in this table were wrong in three consecutive rounds — M14 (self-reported), M1 (three
different numbers in three reports), then M27 and M31's attribution. The pattern is diagnostic: the
TOTALS were usually right and the ATTRIBUTIONS usually wrong, which is not what a slip looks like. It is
what a manual copy step looks like. The harness knew the answer every time; a human retyped it.

So the table is now **emitted by the harness, not transcribed**: it writes
`/tmp/mutation-table.md` with one row per mutation and the **full list of failing test names**, and that
file is pasted in verbatim. Two consequences worth stating, because they are the actual fix:

- there is no step at which a number can be summarised, rounded, or grouped by hand ("1 + 4" instead of
  "2 + 3" was exactly that);
- every row now names its tests, so a wrong count is falsifiable by anyone with the repo rather than by
  a reviewer re-running the whole table.

`--no-fail-fast` is the other half, and the reason for each re-run: without it cargo stops at the first
failing target, so a mutation caught only by a later target reads as caught by fewer tests — or, if the
only test that catches it lives in a target that never ran, as **not caught at all**.

One discrepancy from the R2 report is resolved rather than papered over. The reviewer measured **M27 as
3 red including `the_json_report_is_scrubbed_too_because_otl_wrote_it`**; this harness measures 2, and
both are right about different mutations. M27 as written here changes which renderer *`api describe`*
asks for, which cannot affect `doctor` — `doctor` calls `render_json_scrubbed` itself. The mutation that
does reach all three consumers is neutering the shared sink, and it was missing from the table; it is
now **M35**, and it measures **5** (all three authored-JSON surfaces plus the two `describe` tests).
That is the row the reviewer's number belongs to.

**And on its first run the emitted table immediately paid for itself: M32 came back 0 red.** Not because
the ordering is unpinned — the ad-hoc measurement during R1 had found 2 — but because the mutation
DEFINITION was wrong. It moved the version check past `check_meta` and stopped there, which is still
before `decode_ops`, so it expressed no change at all. Re-run as a two-part patch that actually puts the
check after the operation decode, it is 2 red, and those two are the migration tests. A hand-written
table would have carried the number 2 from the earlier run and never shown that the mutation it claimed
to measure was a no-op. That is the failure mode behind all three counting errors, seen from the inside:
the number was right and the thing it described was not.

| # | What was broken | Red | Which tests |
|---|-----------------|-----|-------------|
| M1 | list ignores the output mode (always the terminal form) | 8 | `a_cache_from_the_previous_ir_version_is_outdated_not_damaged, a_document_with_terminal_escapes_is_neutralized, a_synced_spec_replaces_the_built_in_one_entirely, an_operation_named_like_a_reserved_word_is_reported_not_hidden, api_list_flags_operations_that_are_not_callable, api_list_includes_known_operations_with_their_summary, api_list_prints_one_object_per_spec_operation_without_config, the_explicit_json_flag_and_the_pipe_default_agree` |
| M2 | list JSON always says callable | 2 | `api_list_flags_operations_that_are_not_callable, an_operation_that_cannot_be_called_is_flagged_in_both_states` |
| M3 | the human form of describe renders nothing | 2 | `an_uncallable_operation_says_so_in_both_states, the_human_form_names_the_operation_and_every_parameter` |
| M4 | describe drops enum_values | 2 | `numeric_bounds_and_enumerations_reach_the_output, describe_prints_the_enumerations_local_validation_enforces` |
| M5 | describe drops numeric bounds | 1 | `numeric_bounds_and_enumerations_reach_the_output` |
| M6 | describe drops response_fields | 2 | `the_json_form_carries_the_response_shape, describe_prints_the_response_shape` |
| M7 | source always reports the built-in table | 2 | `describe_answers_from_the_effective_table_not_the_built_in_one, sync_rebuilds_the_cache_and_reset_clears_it` |
| M8 | lookups answer from the built-in table instead of the effective one | 6 | `a_document_with_terminal_escapes_is_neutralized, an_operation_named_like_a_reserved_word_is_reported_not_hidden, describe_answers_from_the_effective_table_not_the_built_in_one, sync_makes_a_new_endpoint_available_to_api_list, sync_rebuilds_the_cache_and_reset_clears_it, third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M9 | a shadowed reserved word warns nobody | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M10 | spec text reaches the human rendering unscrubbed | 1 | `a_hostile_string_is_neutralised_in_both_states` |
| M11 | --help on an unknown operation falls back to the generic help | 2 | `help_on_an_unknown_operation_is_an_error_not_a_generic_help, operation_level_help_reports_an_unknown_name_before_a_stray_argument` |
| M12 | --help always prints the generic help (the original defect) | 5 | `help_on_an_unknown_operation_is_an_error_not_a_generic_help, operation_level_help_describes_that_operation, operation_level_help_refuses_what_describe_refuses, operation_level_help_reports_an_unknown_name_before_a_stray_argument, the_short_help_flag_describes_too` |
| M13 | the generic help is rendered from an unbuilt command tree | 2 | `command_level_help_still_works_and_names_both_reserved_words, help_on_a_reserved_word_prints_the_command_help` |
| M14 | request flags are accepted on a local path | 3 | `api_list_rejects_request_flags, describe_rejects_request_flags, operation_level_help_refuses_what_describe_refuses` |
| M15 | extra positionals are accepted on a local path | 3 | `api_list_rejects_extra_arguments, describe_needs_exactly_one_operation, operation_level_help_refuses_what_describe_refuses` |
| M16 | `describe` with no operation silently picks one | 1 | `describe_needs_exactly_one_operation` |
| M17 | `describe` is routed to the network call path | 14 | `a_document_with_terminal_escapes_is_neutralized, a_local_command_works_with_every_outbound_route_dead, an_operation_named_like_a_reserved_word_is_reported_not_hidden, describe_answers_from_the_effective_table_not_the_built_in_one, describe_falls_back_to_the_built_in_table_too, describe_flags_an_operation_the_generic_client_cannot_call, describe_prints_every_request_facet_without_configuration, describe_prints_the_enumerations_local_validation_enforces, describe_prints_the_response_shape, describe_says_whether_an_operation_paginates, discovery_sends_nothing_even_when_it_could, operation_level_help_describes_that_operation, sync_rebuilds_the_cache_and_reset_clears_it, third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M18 | an absent facet becomes an empty string instead of null | 1 | `an_absent_facet_is_null_rather_than_an_empty_string` |
| M19 | pagination is always reported as absent | 2 | `pagination_is_reported_and_agrees_with_the_call_path, describe_says_whether_an_operation_paginates` |
| M20 | a real operation wins over the reserved word | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M21 | a reserved word collides with a real operation name | 27 | `a_cache_from_the_previous_ir_version_is_outdated_not_damaged, a_damaged_cache_falls_back_to_the_built_in_spec, a_document_that_is_not_a_spec_is_rejected_and_the_cache_kept, a_document_with_terminal_escapes_is_neutralized, a_local_command_works_with_every_outbound_route_dead, a_local_document_can_be_compiled_without_any_network, a_synced_spec_replaces_the_built_in_one_entirely, an_operation_named_like_a_reserved_word_is_reported_not_hidden, an_unknown_operation_still_gets_the_cli_error_not_a_clap_error, api_known_op_resolves_in_ir_with_no_spec_file_reachable, api_list_flags_operations_that_are_not_callable, api_list_includes_known_operations_with_their_summary, api_list_prints_one_object_per_spec_operation_without_config, api_list_rejects_request_flags, api_reserved_words_name_no_built_in_operation, discovery_sends_nothing_even_when_it_could, help_on_a_reserved_word_prints_the_command_help, help_on_an_unknown_operation_is_an_error_not_a_generic_help, operation_candidates_cover_the_whole_ir_table, operation_level_help_describes_that_operation, operation_level_help_refuses_what_describe_refuses, reset_removes_a_cache_this_build_cannot_read, reset_returns_to_the_built_in_spec, sync_heals_a_damaged_cache, sync_makes_a_new_endpoint_available_to_api_list, the_short_help_flag_describes_too, third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M22 | the describe candidate is dropped from the completion scripts | 1 | `operation_candidates_cover_the_whole_ir_table` |
| M23 | the list JSON drops content_type | 1 | `an_operation_that_cannot_be_called_is_flagged_in_both_states` |
| M24 | the reserved-word warning fires only for `describe` | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M25 | the --help alias stops refusing what `describe` refuses (F3) | 1 | `operation_level_help_refuses_what_describe_refuses` |
| M26 | doctor's self-authored JSON goes back to render_json (F1) | 1 | `the_json_report_is_scrubbed_too_because_otl_wrote_it` |
| M27 | the JSON sink stops scrubbing | 2 | `a_hostile_string_is_neutralised_in_both_states, third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M28 | describe drops the parameter description | 3 | `a_document_with_terminal_escapes_is_neutralized, sync_rebuilds_the_cache_and_reset_clears_it, third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M29 | the human form drops the parameter's prose | 1 | `the_human_form_carries_each_parameters_prose_on_its_own_line` |
| M30 | a description is compiled in raw instead of sanitized | 1 | `a_document_with_terminal_escapes_is_neutralized` |
| M31 | the cache checks its versions only after decoding operations | 5 | `a_cache_from_the_previous_ir_version_is_outdated_not_damaged, a_version_string_from_the_file_is_never_echoed_raw, another_cli_version_is_stale_not_damaged, another_ir_schema_version_is_stale_not_damaged, describe_falls_back_to_the_built_in_table_too` |
| M32 | the version check moves back AFTER the operation decode (the D10 defect, restored) | 2 | `a_cache_from_the_previous_ir_version_is_outdated_not_damaged, describe_falls_back_to_the_built_in_table_too` |
| M33 | auth output's JSON goes back to render_json (R2 F3) | 2 | `auth_info_json_carries_no_hazard_from_the_config_file, both_renderings_scrub_foreign_text` |
| M34 | auth output's human lines stop being scrubbed (R2 F3) | 1 | `both_renderings_scrub_foreign_text` |
| M35 | the shared authored-JSON sink stops scrubbing | 5 | `auth_info_json_carries_no_hazard_from_the_config_file, a_hostile_string_is_neutralised_in_both_states, both_renderings_scrub_foreign_text, the_json_report_is_scrubbed_too_because_otl_wrote_it, third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M36 | the one-line fold is dropped from the shared scrub | 2 | `both_renderings_scrub_foreign_text, one_line_scrubbing_folds_a_forged_entry_back_into_its_line` |

**M29 came back GREEN on the first attempt**, and that is the one finding in this round I owe to the
harness rather than to the reviewer: deleting the prose line from the human rendering broke nothing. The
JSON assertion covered the field, the hostile-text test covered the scrub, and nobody had asserted that
the prose reaches the terminal at all — on the very feature the version bump was made for.
`the_human_form_carries_each_parameters_prose_on_its_own_line` now asserts it, with its own
anti-vacuity guard (it fails if `documents.info` stops carrying prose) and it checks the layout property
too: the prose is a line of its own, indented into the value column, not appended to the facet line. Its
first version asserted "the prose line does not contain the parameter name", which was wrong for a
different reason — `id`'s prose contains the word "identifier".

**M31 vs M32, stated precisely** because they look redundant: M31 deletes the version check from
`load_at` entirely (so it tests that the check exists), while M32 restores it to where it used to be,
AFTER the operation decode (so it tests the ORDER, which is what D10 is about). Only the two migration
tests catch M32; the four pre-existing cache tests M31 also trips do not, because they were written when
the old order was correct.

Three assertions were written specifically so they could not pass vacuously, following the lesson from
Story 4.3's M14:

- `discovery_sends_nothing_even_when_it_could` sets **both** `OUTLINE_URL` (at a live wiremock) and
  `OUTLINE_API_KEY`, so there is something to send, and ends with a **control call** (`otl api
  auth.info`) that asserts the mock recorded exactly one request. Without that control, "the mock
  received nothing" would also be true of a mock that records nothing.
- `third_party_text_reaches_stdout_scrubbed_in_both_paths` ends by reading the compiled cache file off
  disk and asserting the bytes of all six residual `Cf` characters are **in** it, and that the prose
  really arrived in the contract. Without those controls the test would pass just as well against a
  document that never carried them — which is how a scrub test goes quiet.
- `an_operation_that_cannot_be_called_is_flagged_in_both_states` asserts the set of flagged operations
  is non-empty before comparing the two states, compares counts rather than spot-checking, and checks
  that the content type in each JSON row appears in the corresponding text line.
- R2 added two more of the same shape: `both_renderings_scrub_foreign_text` asserts `has_hazard` of its
  own fixture first (a scrub test whose input is already clean proves nothing), and
  `the_hostile_name_really_is_in_the_config_file_and_in_the_output_path` reads the config file back off
  disk to show the character is genuinely on the path from file to stdout — so "no hazard on stdout" is
  not "no profile name on stdout".

One assertion was rewritten mid-flight for the same reason: the completions test first said
`script.contains("describe")`, which passes with no candidate emitted at all, because the word also
appears in the `--help` flag's own description that the generator embeds. It now asserts the
shell-specific **candidate form** (`list describe ` for bash/zsh, `-a "describe"` for fish), and M22
confirms that version is sensitive.

### Gates (measured, exit status captured before any pipe)

`bash scripts/check-all.sh --windows`, status captured before any pipe (`bash -c '... ; echo
"EXIT=$?"'`): **EXIT=0**, run twice — once on the branch after the R2 fixes, and again **on the merged
`develop`**, because Story 4.7 had rewritten `check-all.sh` and `check-binary-size.sh` underneath this
branch. On the merged tree: `cargo fmt --check` ok, `cargo clippy -D warnings` ok, `cargo test
--workspace` ok, `cargo doc` ok, binary size ok for **both** shipped targets (4.7's script measures both
rather than just the host), `win-check.sh` ok — which under 4.7 is a source lint over the
`#[cfg(windows)]` branches macOS never compiles, not a shipped platform. 1,265 tests passing, 0 failed.

`--linux` deliberately not run (the user asked to skip local Linux, and Docker cannot start containers
on this machine). Under Story 4.7 there is no Linux CI leg to fall back on either — both platform flags
are now local-only conveniences rather than a stand-in for a shipped platform.

Worth recording because the script's header is about exactly this: the R1 round's first full run came
back **FAIL** on clippy and on `win-check.sh`, for a `doc_lazy_continuation` in a doc comment I had just
written — the same lint that caught Story 4.3's author, for the same reason (comments added after the
last `cargo test`). Two gates, one cause, and neither `cargo test` nor `cargo fmt` saw it.

### Binary size

Measured with `scripts/check-binary-size.sh` (`--profile dist`) on the same machine at each stage. No
budget constant was changed.

| target | `develop` before | R1 (before the IR bump) | final | total delta |
|--------|------------------|-------------------------|-------|-------------|
| aarch64-apple-darwin | 3,464,608 | 3,481,184 | **3,514,976** | **+50,368 B** (+1.45%) — 93% of budget, 70% of NFR2 |
| x86_64-apple-darwin | 4,007,712 | 4,028,216 | **4,057,632** | **+49,920 B** (+1.25%) — 93% of budget, 81% of NFR2 |

**The IR bump alone costs +33,776 B (aarch64) / +29,416 B (x86_64), and it is fully accounted for** —
which the review asked to be checked rather than assumed, on the grounds that a delta well above the
text volume means something else came along. It did not:

- **16,055 B** of description text (244 descriptions, measured on the generated `ir_table.rs`; the
  reviewer independently measured 16,047 B by unescaping in Python — escape noise);
- **9,192 B** of struct growth: `Cow<'static, str>` is 24 bytes (verified with `size_of`, and
  independently by the reviewer), and the static table has 383 `ParamSpec` entries — `383 × 24`;
- the remaining ~8,529 B is the pointer relocation each of those 383 `Cow::Borrowed` entries needs in
  `__DATA_CONST`, plus the rendering code.

All three terms are inherent to putting a string field on a 383-entry static table; none is a
dependency, and none is trimmable without dropping the feature.

#### The musl extrapolation became moot at merge time

Every earlier draft of this section carried an extrapolation: `x86_64-unknown-linux-musl` could not be
measured here (no toolchain) and was projected to land near 4,610,000 B, ~94% of its budget and ~92% of
NFR2, leaving ~385,000 B of headroom. It was flagged as the one number only CI could settle, and the
whole point of the flag was that a projection is not a measurement.

It never had to be settled: **Story 4.7 landed on `develop` while this branch was in review and narrowed
CI and the release to macOS only.** `x86_64-unknown-linux-musl` and `x86_64-pc-windows-msvc` are no
longer published, `budget_for_target` no longer defines them, and `install-linux-musl-toolchain.sh` is
gone. The two remaining published targets are exactly the two measured above, both locally, both at 93%
of the same budgets as before.

So the risk closed by deletion rather than by evidence, and that distinction is worth keeping: nothing
was learned about how this change behaves on musl. If either target is ever re-added, the delta to
re-measure is the +50 KB in the table above, and the ~1.14 platform factor in the size script's header
is the only guide — a factor whose own basis was removed with the targets it described.

### Known gaps, left deliberately

- **Response-field descriptions are not compiled in** (D6): 53,024 bytes against 16,055 for the
  request side, for the half of the contract that needs prose least (and on the largest shipped
  target, whichever that is at the time).
  Additive if wanted later, at the cost of another version bump.
- **`pattern` is still not compiled** (pre-existing `ir.rs` TODO: a regex engine costs ~1 MB against a
  5 MB budget for two constraints in the whole vendored spec). `describe` therefore cannot report it,
  and does not pretend to.
- **The terminal form of `api list` is still TSV**, not an aligned table. It is readable, it is what
  users have today, and changing it was not asked for. The dual-state contract is satisfied either way.
- **The compiler's text table is still narrower than the crate's** (D5). Closing it at the source needs
  the hazard table in a crate `spec-compile` may depend on, which it cannot have (it is a build
  dependency and must not pull `engine` into the host build). Every consumer therefore closes it at its
  own sink, and after R1 every consumer is accounted for: this module's two states and `otl api`'s
  validation diagnostics. **The previous version of this bullet claimed the diagnostics were an open
  hole. That was wrong** — see D5 — and it is corrected rather than deleted, because a false gap is a
  standing invitation to "fix" something that is not broken.
- **`otl doctor`'s and `otl auth`'s output were scrubbed by this story, not by their own.** doctor's
  exposure was nil in practice (its only third-party strings are operation names and origin hosts, both
  ASCII-constrained); `auth`'s was not — `account`, `workspace` and `scope` come from the server. Both
  were fixed here rather than deferred, because two `--json` policies in one binary is the drift
  `crate::text` exists to prevent.
- **`otl docs export --json` carries server text verbatim, by a deliberate decision of Story 3.6 that
  this story is NOT overriding.** An earlier draft of this bullet said the remaining `render_json` call
  sites "were checked by hand" — they had not been, and checking them properly found one surface the
  claim would have covered up. All six, classified:
  - `render.rs` (the definition), `docs/search.rs`, `docs/detail.rs`, `docs/view.rs`,
    `collections.rs` — all print a SERVER RESPONSE verbatim (raw rows, the document object). Exempt,
    correctly, and each says so at the call site.
  - `docs/export.rs::print_json` — an **authored summary** (`complete`, `durable`, `stray`, `exported`,
    `failed[]`), so by this story's rule it should be scrubbed. It is not.
  Story 3.6 decided that on purpose: `Failure::id` is documented as "kept RAW … the JSON summary carries
  it verbatim" so a script can retry with it, and
  `docs_export_terminal.rs::a_hostile_document_id_cannot_rewrite_the_terminal` pins it with an
  `assert_eq!` against an id containing `ESC`, `BEL` and a newline. That reasoning ("JSON encoding makes
  it safe to carry") is **sound for control characters** — `serde_json` escapes them to `\u001b`, so
  they cannot reach a terminal as an escape sequence — and **unsound for the residual `Cf` set**, which
  `serde_json` emits raw. So the gap is real but narrow: bidi and invisible characters in a document id
  or failure label, in that one summary.
  Not fixed here, and the asymmetry with R2's F3 is the point. `auth info` was an oversight with no
  decision behind it and no test, so fixing it was strictly a correction. This is a considered,
  documented, test-pinned decision on another story's semver-protected surface; reversing it is a
  judgement call that belongs in review, not in a drive-by edit on a branch that is being merged.
  **What is still missing either way is the guard**: nothing asserts that a command's authored-JSON path
  goes through `render_json_scrubbed`. A hand classification is a snapshot. That test is the real fix
  and it does not exist.
- **`cargo doc` runs without `-D warnings`**, so both R2 documentation findings were invisible to the
  gate — as are two pre-existing broken intra-doc links in files this story did not touch
  (`engine/src/text.rs`, `auth/mod.rs`). Adding the flag was explicitly deferred by the user to keep it
  out of this change.

### Completion Notes

- The workspace suite goes from **1,218 to 1,265 passing** (+47), 0 failed.
- No new exit code; `docs/exit-codes.md` gained a note and the README's generated table is unchanged.
- No new network entry point: `tests/no_phone_home.rs`'s convergence table is untouched (its
  behavioural case gained the two new commands).
- No binary-size budget constant changed.
- The engine gains exactly one data field (`ParamSpec::description`) and stays Outline-agnostic.
- `project-context.md` gained one rule: which JSON renderer a new `--json` surface must use.
- Three surfaces now share one terminal-safety policy where there were three: `api describe`, `doctor`
  and `auth`, with the one-line fold in `stdio` rather than copied per module.
