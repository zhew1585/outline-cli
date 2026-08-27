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
9. (R1) **Given** the OpenAPI document states a parameter's meaning only in prose
   **When** `describe` runs
   **Then** that prose is printed, sanitized through the same path a summary takes — and a cache written
   by a build with the previous IR schema is reported as *outdated*, discarded, and rebuildable with
   `otl spec sync`, with no command failing in the meantime

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
  - [x] 32 mutations, 32 red (table in Dev Agent Record)
- [x] Task 9 (R1): the `--help` alias refuses what `describe` refuses (F3)
- [x] Task 10 (R1): one renderer for JSON `otl` authors; `doctor` uses it; the `crate::text` exemption
      narrowed to "a server response", and the rule recorded in `project-context.md` (F1)
- [x] Task 11 (R1): the false Known-gaps entry about validation diagnostics corrected, in the story and
      in `describe.rs` (F2)
- [x] Task 12 (R1, user scope): `ParamSpec::description`, `IR_SCHEMA_VERSION` 5 -> 6, sanitized through
      the one existing display-text path, with the migration path driven end to end

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
  total **53,024 bytes** — 3.3× more, on the target that binds NFR2. And the asymmetry in value runs
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
  src/commands/doctor/report.rs         # self-authored JSON scrubbed; `emit` split so it is testable
  src/render.rs                         # + render_json_scrubbed (one renderer for authored JSON)
  src/spec/bounded.rs                   # decode_table -> decode_meta + decode_ops
  src/spec/cache.rs                     # versions checked BEFORE any operation record is decoded
  src/spec/mod.rs                       # converts and validates the new field
  src/text.rs                           # the --json exemption narrowed to "a server response"
  src/main.rs                           # passes `Cli::command` to api::run
  tests/api_describe.rs                 # new, 19 end-to-end
  tests/ir_upgrade.rs                   # new, 5 end-to-end: the v5 -> v6 migration path
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

**M1's red count was wrong (6, actually 7 → now 8).** The second counting error in the table, after the
one already confessed. Root cause both times: the first run was made without `--no-fail-fast`, so cargo
stopped at the first failing target and later targets never ran. The whole table has been re-measured,
not patched row by row.

### Scope added by the user during review: parameter descriptions in the IR

See D6, D10 and D11. In short: `describe` was faithful to the spec and misleading about the API on the
29 operations that mark no parameter required, the disambiguation was sitting in prose the IR did not
carry, so `IR_SCHEMA_VERSION` went 5 → 6 with `ParamSpec::description`. Request parameters only
(16,055 bytes vs 53,024 for response fields). Sanitized through the one existing display-text path, with
the hostile fixture aimed at it. The cache-invalidation path is driven by `tests/ir_upgrade.rs` against
a real v5 cache, and a latent ordering defect it would have activated is fixed (D10).

### Mutation verification (break the behaviour → confirm red → restore)

Driven by a script that patches one anchor, runs `cargo test --no-fail-fast` over twelve targets
(`--lib`, `api_list`, `api_describe`, `spec_sync_e2e`, `no_phone_home`, `startup_guard`, `ir_upgrade`,
`completions`, `doctor_e2e`, `doctor_golden`, `spec_cache`, `spec_cache_rejects`), records the failures
and restores the file. **32 of 32 turn red.** Every row below was re-measured after the R1 changes; none
is carried over from the first run.

`--no-fail-fast` is load-bearing and the reason for the re-run: without it cargo stops at the first
failing target, so a mutation caught only by a later target reads as caught by fewer tests — or, if the
only test that catches it lives in a target that never ran, as **not caught at all**. That is the
difference between a coverage table and a comforting one.

| # | What was broken | Red | Which tests |
|---|-----------------|-----|-------------|
| M1 | `list` ignores the output mode (always the terminal form) | 8 | 4 × `api_list` + 2 × `spec_sync_e2e` + `ir_upgrade` + `api_describe` |
| M2 | `list` JSON always says `callable: true` | 2 | `an_operation_that_cannot_be_called_is_flagged_in_both_states`, `api_list_flags_operations_that_are_not_callable` |
| M3 | the human form of `describe` renders nothing | 2 | `the_human_form_names_the_operation_and_every_parameter`, `an_uncallable_operation_says_so_in_both_states` |
| M4 | `describe` drops `enum_values` | 2 | `numeric_bounds_and_enumerations_reach_the_output`, `describe_prints_the_enumerations_local_validation_enforces` |
| M5 | `describe` drops `minimum`/`maximum` | 1 | `numeric_bounds_and_enumerations_reach_the_output` |
| M6 | `describe` drops `response_fields` | 2 | `the_json_form_carries_the_response_shape`, `describe_prints_the_response_shape` |
| M7 | `source` always reports `built-in` | 2 | `describe_answers_from_the_effective_table_not_the_built_in_one`, `sync_rebuilds_the_cache_and_reset_clears_it` |
| M8 | lookups answer from the built-in table, not the effective one | 6 | 3 × synced-table + `sync_makes_a_new_endpoint_available_to_api_list` + hostile-text + `sync_rebuilds_...` |
| M9 | a shadowed reserved word warns nobody | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M10 | spec text reaches the HUMAN rendering unscrubbed | 1 | `a_hostile_string_is_neutralised_in_both_states` |
| M11 | `--help` on an unknown operation falls back to the generic help | 2 | `help_on_an_unknown_operation_is_an_error_not_a_generic_help`, `..._reports_an_unknown_name_before_a_stray_argument` |
| M12 | `--help` always prints the generic help (**the original defect**) | 5 | all four `operation_level_help_*` / `the_short_help_flag_*` + the unknown-name test |
| M13 | the generic help is rendered from an unbuilt command tree (globals lost) | 2 | `command_level_help_still_works_and_names_both_reserved_words`, `help_on_a_reserved_word_prints_the_command_help` |
| M14 | request flags are accepted on a local path | 3 | `api_list_rejects_request_flags`, `describe_rejects_request_flags`, `operation_level_help_refuses_what_describe_refuses` |
| M15 | extra positionals are accepted on a local path | 3 | `api_list_rejects_extra_arguments`, `describe_needs_exactly_one_operation`, `operation_level_help_refuses_...` |
| M16 | `describe` with no operation silently picks one | 1 | `describe_needs_exactly_one_operation` |
| M17 | `describe` is routed to the network call path | 14 | every `describe_*` + `discovery_sends_nothing_even_when_it_could` + `a_local_command_works_with_every_outbound_route_dead` + 8 more |
| M18 | an absent facet becomes `""` instead of `null` | 1 | `an_absent_facet_is_null_rather_than_an_empty_string` |
| M19 | `paginates` is always `false` | 2 | `pagination_is_reported_and_agrees_with_the_call_path`, `describe_says_whether_an_operation_paginates` |
| M20 | a real operation wins over the reserved word | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M21 | a reserved word collides with a real operation name (`list` = `documents.info`) | 27 | `api_reserved_words_name_no_built_in_operation` + 26 others |
| M22 | the `describe` candidate is dropped from the completion scripts | 1 | `operation_candidates_cover_the_whole_ir_table` |
| M23 | the `list` JSON drops `content_type` (says "no" without saying why) | 1 | `an_operation_that_cannot_be_called_is_flagged_in_both_states` |
| M24 | the reserved-word warning fires only for `describe`, not `list` | 1 | `an_operation_named_like_a_reserved_word_is_reported_not_hidden` |
| M25 | the `--help` alias stops refusing what `describe` refuses (**F3**) | 1 | `operation_level_help_refuses_what_describe_refuses` |
| M26 | doctor's self-authored JSON goes back to `render_json` (**F1**) | 1 | `the_json_report_is_scrubbed_too_because_otl_wrote_it` |
| M27 | the JSON sink stops scrubbing | 2 | `a_hostile_string_is_neutralised_in_both_states`, `third_party_text_reaches_stdout_scrubbed_in_both_paths` |
| M28 | `describe` drops the parameter description | 3 | `third_party_text_...`, `sync_rebuilds_the_cache_and_reset_clears_it`, `a_document_with_terminal_escapes_is_neutralized` |
| M29 | the human form drops the parameter's prose | 1 | `the_human_form_carries_each_parameters_prose_on_its_own_line` |
| M30 | a description is compiled in raw instead of sanitized | 1 | `a_document_with_terminal_escapes_is_neutralized` |
| M31 | the cache stops checking its versions before using a table | 5 | `a_cache_from_the_previous_ir_version_is_outdated_not_damaged` + 4 pre-existing cache tests |
| M32 | the version check moves back AFTER the operation decode (the D10 defect, restored) | 2 | `a_cache_from_the_previous_ir_version_is_outdated_not_damaged`, `describe_falls_back_to_the_built_in_table_too` |

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
tests catch M32; the four pre-existing cache tests do not, because they were written when the old order
was correct.

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

One assertion was rewritten mid-flight for the same reason: the completions test first said
`script.contains("describe")`, which passes with no candidate emitted at all, because the word also
appears in the `--help` flag's own description that the generator embeds. It now asserts the
shell-specific **candidate form** (`list describe ` for bash/zsh, `-a "describe"` for fish), and M22
confirms that version is sensitive.

### Gates (measured, exit status captured before any pipe)

`bash scripts/check-all.sh --windows`, status captured before any pipe (`bash -c '... ; echo
"EXIT=$?"'`): **EXIT=0** — `cargo fmt --check` ok, `cargo clippy -D warnings` ok, `cargo test
--workspace` ok, `cargo doc` ok, binary size ok, `win-check.sh` ok.

`--linux` deliberately not run (the user asked to skip local Linux; Docker cannot start containers on
this machine, and CI has native Linux and Windows legs).

Worth recording because the script's header is about exactly this: the R1 round's first full run came
back **FAIL** on clippy and on `win-check.sh`, for a `doc_lazy_continuation` in a doc comment I had just
written — the same lint that caught Story 4.3's author, for the same reason (comments added after the
last `cargo test`). Two gates, one cause, and neither `cargo test` nor `cargo fmt` saw it.

### Binary size

Measured with `scripts/check-binary-size.sh` (`--profile dist`), all three states built the same way on
the same machine. No budget constant was changed.

| target | `develop` | R1 (before the IR bump) | final | total delta |
|--------|-----------|-------------------------|-------|-------------|
| aarch64-apple-darwin | 3,464,608 | 3,481,184 | **3,514,960** | **+50,352 B** (+1.45%) — 93% of budget, 70% of NFR2 |
| x86_64-apple-darwin | 4,007,712 | 4,028,216 | **4,057,632** | **+49,920 B** (+1.25%) — 93% of budget, 81% of NFR2 |

**The IR bump alone costs +33,776 B (aarch64) / +29,416 B (x86_64), and it is fully accounted for** —
which the review asked to be checked rather than assumed, on the grounds that a delta well above the
text volume means something else came along. It did not:

- **16,055 B** of description text (244 descriptions, measured on the generated `ir_table.rs`; the
  reviewer's 10,608 B estimate was low because it did not follow `$ref`/`allOf`);
- **9,192 B** of struct growth: `Cow<'static, str>` is 24 bytes (verified with `size_of`), and the
  static table has 383 `ParamSpec` entries — `383 × 24`;
- the remaining ~8,500 B is the pointer relocation each of those 383 `Cow::Borrowed` entries needs in
  `__DATA_CONST`, plus the rendering code.

All three terms are inherent to putting a string field on a 383-entry static table; none is a
dependency, and none is trimmable without dropping the feature.

`x86_64-unknown-linux-musl` and `x86_64-pc-windows-msvc` can only be measured by CI (MSVC cannot be
linked off Windows; the musl toolchain is not installed here). Extrapolating with the platform factor
the script's own header derives (musl ≈ ×1.139 of x86_64-darwin, the figure the review confirmed), and
cross-checking against the CI musl figure from Story 4.3 (4,551,792 B) plus the scaled delta:

- musl ≈ **4,608,000–4,621,000 B** ≈ **94%** of its 4,920,000 budget and **92%** of the 5,000,000 NFR2
  promise, leaving roughly **385,000 B** of headroom where 448,000 B stood before this story.

That is the number a future feature has to live inside, and it is worth flagging plainly: this story
spent about 14% of the remaining NFR2 headroom, most of it on the IR bump. The regression budget is not
breached on any target and no constant was touched, but musl moving from 92% to ~94% of its budget is
the thing CI should be watched for.

### Known gaps, left deliberately

- **Response-field descriptions are not compiled in** (D6): 53,024 bytes against 16,055 for the
  request side, on the target that binds NFR2, for the half of the contract that needs prose least.
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
- **`otl doctor`'s `--json` was scrubbed by this story, not by its own.** Its exposure was nil in
  practice (its only third-party strings are operation names and origin hosts, both ASCII-constrained by
  `is_safe_op_name` / origin serialization), but two `--json` policies in one binary is the drift
  `crate::text` exists to prevent.

### Completion Notes

- The workspace suite goes from **1,218 to 1,261 passing** (+43), 0 failed.
- No new exit code; `docs/exit-codes.md` gained a note and the README's generated table is unchanged.
- No new network entry point: `tests/no_phone_home.rs`'s convergence table is untouched (its
  behavioural case gained the two new commands).
- No binary-size budget constant changed.
- The engine gains exactly one data field (`ParamSpec::description`) and stays Outline-agnostic.
- `project-context.md` gained one rule: which JSON renderer a new `--json` surface must use.
