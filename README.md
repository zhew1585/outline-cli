# outline-cli (`otl`)

A fast, single-binary CLI for [Outline](https://www.getoutline.com/) knowledge bases.

Outline's API is pure RPC — every endpoint is `POST /api/resource.method` — so the OpenAPI spec is the
contract. `otl` compiles that spec into a static table at build time and interprets it at runtime, which
means **every** endpoint is callable without hand-written per-endpoint code, and cold start stays in
single-digit milliseconds.

> **Status: pre-1.0.** The generic API engine, OAuth/API-key authentication,
> multi-workspace profiles, shell completions, agent discovery, and the stable
> day-to-day commands below are implemented. Command surfaces may still change
> before 1.0.

## Install

**macOS only, for now.** Releases are cut from git tags and publish one binary per Apple target, plus a
Homebrew formula and a shell installer. Linux and Windows are not built or tested at the moment; the
source still carries their platform branches, so re-adding them is a configuration change rather than a
rewrite. There is no published release yet — the commands below are the contract the release pipeline
implements, and they start working with the first tag.

**Homebrew**:

```sh
brew install zhew1585/tap/outline-cli
```

**Shell installer** (installs into `$CARGO_HOME/bin`):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/zhew1585/outline-cli/releases/latest/download/outline-cli-installer.sh | sh
```

Prebuilt archives are attached to every release for these targets:

| Platform | Target triple | Notes |
|----------|---------------|-------|
| macOS (Apple Silicon) | `aarch64-apple-darwin` | |
| macOS (Intel) | `x86_64-apple-darwin` | separate thin archive, not a universal binary |

**Verifying a download.** Every release artifact — both archives, and also the shell installer, the
Homebrew formula and `sha256.sum` — carries a GitHub build attestation, so you can check
it was produced by this repository's release workflow rather than merely that it matches a checksum
published alongside it:

```sh
gh attestation verify outline-cli-aarch64-apple-darwin.tar.xz --repo zhew1585/outline-cli
```

**`otl` never checks for updates.** No telemetry, no update ping, no background spec fetch — the binary
makes exactly the network requests your command implies. Upgrading is something you do: `brew upgrade`,
or re-run the shell installer.

**From source** (Rust stable, `rust-version` 1.85):

```sh
git clone https://github.com/zhew1585/outline-cli
cd outline-cli
cargo build --release
# binary at target/release/otl
```

## Quick start

```sh
export OUTLINE_URL=https://outline.example.com
export OUTLINE_API_KEY=...            # Settings → API in your Outline instance

otl api list                          # every callable operation in the vendored spec
otl api describe documents.info       # what it takes and what it returns
otl api documents.info id=<doc-id>    # call any of them
otl api documents.search query=deploy --json | jq '.[].document.title'
```

Arguments are `key=value` pairs coerced to the types the spec declares, and validated locally before any
network request is made:

```sh
otl api documents.list limit=25 template=false   # native JSON number and boolean on the wire
otl api documents.info id=not-a-uuid             # exits 2, no request sent
otl api shares.create --body @share.json         # oneOf/anyOf bodies go through --body verbatim
```

### Discovering what to call

Both discovery commands are purely local — they read the operation table compiled into the binary (or
the one `otl spec sync` installed) and send nothing.

```sh
otl api list                             # every operation: name, summary, path, content type, callable, curated command
otl api describe documents.info          # one operation's full contract
otl api documents.info --help            # the same thing, from the flag you would reach for
otl api describe documents.list --json | jq '.parameters[] | {name, type, required, description}'
otl api describe documents.search --json | jq '.response_fields[] | select(.name == "document") | .fields'
otl api list --json | jq -r '.[] | select(.curated_command) | "\(.name)\t\(.curated_command)"'
```

Both also answer the question that comes before "how do I call this": **is there already a stable
command for it?** 26 of the 116 operations have one, and until they said so, a list of 116 names
answered "how do I reach this" with `otl api` — the less stable of the two paths, chosen because the
more stable one was invisible from there. `curated_command` names it (`"otl docs search"`,
`"otl docs delete --archive"`) or is `null`; the text state carries the same fact as a
`[stable command: ...]` marker, because neither state may say less than the other.
`crates/otl/tests/curated_index.rs` keeps the index honest in both directions: every command it names
must exist, and every operation a curated command's own `--help` names must be in it.

`describe` prints exactly what the CLI itself knows: every parameter with its type, whether it is
required, whether it may be `null`, its `format`, its allowed values, its numeric bounds — the same
facets local validation enforces and `--no-validate` skips — and the one-line description the OpenAPI
document gives it, plus the recursively nested response fields, the request path and content type,
whether the operation paginates, and which table the answer came from (`built-in` or `synced`). Complex
response fields report a `container` (`object`, `array`, or `union`) and nested `fields`; array fields'
children describe one array item. Union alternatives are not merged into a shape the server does not
guarantee.

Two of those shapes have no finite expansion, and the output says so rather than looking empty:
`"fields_omitted": true` marks a field some of whose properties exist but are not listed — a model that
repeats one of its own ancestors (Outline's `User.invitedBy` is a `User`), or a field at the depth limit.
Some, not all: a recursive model with extra properties of its own lists those and still sets the flag.
Without it, `"fields": []` would tell an agent that `…createdBy.invitedBy.id` does not exist, which is
worse than saying nothing: the whole point of this output is that a caller can trust it.

The descriptions matter more than they look. Of the 109 operations that take parameters, **29 mark none
of them required** — `documents.info` declares both `id` and `shareId` optional, and only the prose says
"either the UUID or the urlId is acceptable". Without that line, `required: false` everywhere reads as
"nothing has to be sent". 23 of the 29 carry prose that resolves it; the remaining 6 are as loose
upstream as they look here, and `describe` does not invent a constraint the spec never stated.

Both obey the usual dual-state rule: a terminal gets a readable rendering, a pipe or `--json` gets
JSON. (Before 0.2, `otl api list` printed its tab-separated form into pipes as well, `--json` included.
That was a bug, and `otl api` output is not covered by semver — a script that parsed those columns
should read the JSON array now.)

`list` deliberately stops at what is needed to *choose* an operation. A partial contract — parameter
names with no types, no facets and no response shape — reads like the answer while being a fragment of
one, so the contract is published in exactly one place and in full.

`list` and `describe` are reserved words in the operation namespace, which is safe because every real
operation name is `resource.method`. If a spec you sync ever declares an operation named `list` or
`describe`, the reserved word still wins and `otl` says so on stderr rather than shadowing it silently.

## Signing in

An API key in the environment works everywhere and needs no setup, which is why the quick start uses it.
For interactive use there is a browser flow, and for keys there is a place to put them that is not your
shell history:

```sh
otl auth login                        # browser consent, OAuth 2.0 authorization code + PKCE
otl auth login --client-id <id>       # when an admin pre-registered the application
otl auth set-key < key.txt            # store an API key in the credential file (0600)
otl auth info                         # which credential is in use, and where it lives
otl auth logout                       # revoke and forget this profile's credentials
otl auth logout --purge               # also delete the application otl registered for itself
otl auth logout --force               # discard them even if the server could not be told
```

`otl auth login` discovers the instance's OAuth endpoints, registers `otl` as a public client if the
instance allows it (otherwise it tells you exactly what an admin has to create), and catches the redirect
on a loopback port. It opens the page with the platform's URL handler, or with `$BROWSER` when that is
set, and prints the URL either way — a machine with no browser is a "copy this link", not a failure. Access tokens are then renewed inside the request channel, so no command ever fails
because a token aged out. Outline rotates the refresh token on every use, so renewal takes an advisory
file lock: concurrent `otl` processes refresh once between them rather than invalidating each other.

`otl auth logout` needs no `OUTLINE_URL`: every server it contacts comes out of the credential file,
anchored to the origin each credential recorded for itself. It exits non-zero if a server-side step did
not happen, and — when a retry could still succeed — keeps the credentials rather than leaving you with a
token that is live on the server and unrevocable from here. `--force` overrides that.

When several credentials exist for a profile, the order is OAuth session, then the credential file's API
key, then `OUTLINE_API_KEY`. Using the environment variable prints a one-time note about where plaintext
in the environment tends to end up; `OUTLINE_NO_KEY_WARNING=1` silences it.

Setting `auth = "oauth"` on a profile removes the last step: an environment variable cannot hold a
renewable session, so a profile configured for browser login never quietly authenticates as whatever
`OUTLINE_API_KEY` happens to be exported. A session stored by `otl auth login` is used either way — `auth`
names the login flow, not a filter on what is already stored — and `otl auth info` reports what it shadows.

Every `otl auth` subcommand resolves its instance and profile exactly as `otl api` does, so `--profile`,
`--url`, `--config` and `default_profile` all apply to signing in and out as well. `otl auth logout` is
the one exception, and only in one direction: it honours `--profile` but needs no URL at all.

Four rules the OAuth flow will not bend on, because each protects a credential in flight and a warning
after the fact protects nothing:

- **TLS** for every command, not just sign-in, unless the host is a loopback IP literal. `http://` to
  anything else is refused, and `localhost` does not count as loopback — it is a name, and a resolver can
  point it elsewhere. Endpoints read back out of the credential file are re-checked before they are used.
- **No redirects on credential-bearing requests.** A 307 or 308 replays the request body, and reqwest's
  cross-origin header stripping does not cover bodies, so following one could post an authorization code
  or refresh token to whoever the `Location` names.
- **Discovered endpoints must be the instance's own**, matching its origin and its RFC 8414 `issuer`.
- **Credentials are bound to the instance that issued them.** Pointing `OUTLINE_URL` at a different
  instance without switching profile sends nothing at all, and adding a second instance's credentials to
  the same profile is refused rather than merged. Use one profile per instance.
## Everyday commands

Polished commands cover the day-to-day work. Unlike `otl api`, their flags and
output are a stable (semver) contract. Each command's `--help` ends in two
sections: `API contract(s):` names the underlying operation(s) and the
`otl api describe <operation> --json` that prints their full request and
response shape, and `JSON shape:` states what *that command* writes to stdout —
which is not always the operation's own object.

```sh
otl collections list                          # name / id / document count, every page fetched
otl docs search deploy                        # title / collection / updated / matching snippet
otl docs list                                 # recently updated documents
otl docs list deploy --collection <id>        # unified list/search workflow
otl docs search deploy --json | jq '.[].document.id'

otl docs view <doc-id>                        # markdown; $PAGER on a terminal, plain in a pipe
otl docs view <doc-id> --raw                  # never paged
otl docs view <doc-id> --web                  # prints the URL and opens a browser

cat notes.md | otl docs create --title Notes --collection <collection-id>
otl docs create --title Notes --collection <collection-id> --file notes.md   # equivalent
otl docs update <doc-id> --title "New title"
cat revised.md | otl docs update <doc-id>
printf '\nMore notes\n' | otl docs update <doc-id> --mode append
printf 'new wording' | otl docs update <doc-id> --mode patch --find-text 'old wording'
otl docs move <doc-id> --parent <parent-id> --index 0
otl docs delete <doc-id>                      # trash
otl docs delete <doc-id> --archive            # archive

otl docs export --collection <collection-id> --out ./backup

otl fetch document https://outline.example.com/doc/runbook-abc123
otl fetch collection <collection-id>          # metadata plus full document tree
otl fetch user current_user
otl fetch attachment <attachment-id>          # short-lived signed download URL

otl collections create --name Engineering --icon 🛠️ --color '#3366FF'
otl collections update <id> --description 'Team knowledge base'
otl collections delete <id> --archive

otl comments list --document <id> --status unresolved
otl comments create --document <id> --text 'Looks good'
otl comments update <id> --resolve
otl comments delete <id>

otl attachments create --name image.png --content-type image/png --size 12345
otl users list --status active --role member --query jane
```

`attachments create` returns the pre-signed POST/PUT inputs and the stable
attachment URL. It does not upload local bytes; send them directly to the
returned storage URL. `fetch attachment` handles Outline's authenticated 302
response without forwarding the bearer token to storage.

Comment updates accept `--text` for plain text (Markdown punctuation remains
literal) or `--data FILE` for a complete ProseMirror JSON document. Resolve and
unresolve use Outline's dedicated application API routes. Those two are separate
requests: when the content lands and the resolve does not, the updated comment is
printed and the exit code is **9**, so a retry cannot apply the text twice. The
server's error text for a rejected body is withheld unless
`--show-server-message` is given, exactly as for `otl api --body`.

Notes worth knowing:

- **`docs view` is markdown-first.** A pipe gets the document body, not JSON — the body *is* the data
  here. Ask for `--json` explicitly to get the document object. Every other command follows the usual
  rule (JSON whenever stdout is not a terminal).
- **`docs list` returns two different shapes**, because it dispatches to two operations. Without a
  query it lists `documents.list` rows (`.[].id`); with one it returns `documents.search` hits, where
  the document is nested (`.[].document.id`). A `jq` path written for one yields `null` against the
  other, so reach for `docs search` whenever there is a query and keep `docs list` for the query-less
  listing. Both spellings issue the same `documents.search` request.
- **`collections list --json` carries no document count.** The count in the table is computed here by
  walking `collections.documents` once per collection; the API never states it, so it is not put into
  a payload a script would treat as data. `--no-counts` accordingly changes nothing in JSON mode — the
  extra requests are not made either way. To count in a script, walk the tree:
  `otl fetch collection <id> --json | jq '[.documents | .. | .id? // empty] | length'`.
- **Four commands compose an object** rather than returning the operation's own: `fetch collection`
  (`{collection, documents}`), `fetch attachment` (`{id, signedUrl}`), `docs view --web --json`
  (`{id, title, url}`) and `comments update` when it both edits and resolves (`{comment, status}`).
  Delete commands answer `{"success": true}`, and their `--archive` variants answer with the archived
  entity. Everything else is the operation's own object or array, verbatim. Each command's
  `JSON shape:` section says which it is, and `crates/otl/tests/help_coverage.rs` fails the build if
  a data-printing command stops saying.
- **`docs create` and `docs update` answer with a receipt, not the document.** Outline replies to a
  write with the whole stored document, body included, so appending one line to a 46 KB page used to
  return 46 KB — a cost paid in full by the agent that then has to hold it. Both commands report the
  identity fields instead: `{id, collectionId, parentDocumentId, title, url, urlId, revision,
  createdAt, updatedAt, publishedAt}`, with absent fields omitted rather than sent as null. `.text`
  is never among them. The verbatim response stays one command away, through `docs view <id> --json`
  or `otl api documents.update id=<id> ...` — the curated command offers a chosen shape, `otl api`
  offers the server's.
- **`docs create` publishes** when you give it a `--collection` or `--parent`, because a draft is
  invisible to everyone else; `--draft` opts out. Without a destination Outline cannot publish at all,
  and the command says so.
- **A blank body means "no body".** `otl docs update <id> --title X` from a script (where stdin is
  `/dev/null`) can never be read as "replace the body with nothing". Clearing a body is possible, but
  only by spelling it out: `otl api documents.update id=<id> text=`.
- **`docs export`** rebuilds the document hierarchy as directories, sanitizes every file name (path
  traversal, Windows device names, case- and normalization-insensitive collisions, length limits),
  refuses a non-empty output directory unless `--overwrite` is given, and keeps going when one
  document fails — the failures are summarized at the end and the exit code is 9 (partial failure).
  Each file is written to a temporary file, flushed, and only then given its real name, so an
  interrupted export never leaves a half-written or empty document and a failed `--overwrite` never
  destroys the previous backup. `--json` reports `"complete"`, `"durable"` (`true`/`false`/`null` —
  `null` where the platform cannot flush a directory, so test with
  `complete == true && durable != false` rather than `if (durable)`) and `"stray"` alongside the
  exported paths.
- **Pagination never truncates silently, and never lies about it either.** `--limit N` caps the total
  rows, warns on stderr and exits 0 — you asked for it. But when the CLI's own page cap stops a fetch
  before the server ran out of rows, the result is incomplete through no choice of yours, so
  `docs search`, `collections list` and `docs export` all exit **9**. `docs export --json` also reports
  `"complete": false`, because an output directory cannot show what was never fetched.
- **`docs view` on a terminal is a display, in a pipe it is data.** Piped or `--raw` output is the
  document byte-for-byte — not even a trailing newline is added. On a terminal the text is prepared for
  display: control sequences are replaced (a document body must not be able to set your clipboard or
  forge a hyperlink), and `$PAGER` takes over when the content does not fit on one screen, counting
  wrapped rows rather than lines.
## Keeping the spec current

The spec is vendored into the binary, so a fresh install needs no network for anything but your own
requests. When upstream adds an endpoint you need before the next release:

```sh
otl spec sync                          # fetch upstream, compile once, cache the IR
otl spec sync --spec ./spec3.json      # or compile a local document (development override)
otl api list                           # the new operations are here immediately
otl api describe things.new            # and `describe` answers from the synced table, not the built-in one
otl spec reset                         # go back to the spec built into this binary
```

Nothing checks for spec updates on its own: `otl` never contacts the network unless a command you ran
requires it.

An `otl` upgrade can change the shape of the compiled operation table, and when it does, a cache written
by the previous version is **discarded, not migrated**: interpreting an old table with new rules is a
worse risk than rebuilding a file that is regenerable by definition. That is not an error — commands keep
working on the spec built into the binary, and one stderr line says the cache was outdated and names
`otl spec sync` as the fix. `otl doctor` reports it too. (0.2 does exactly this: the table now carries
each parameter's description.)

## Checking your environment

```sh
otl doctor                             # credentials, instance, spec drift - one report
otl doctor --offline                   # local state only; contacts nothing
otl doctor --json | jq '.checks[]'     # machine-readable, one object per check
otl doctor --spec-url <url>            # compare against a mirror instead of upstream
```

Eight checks in dependency order: which config file and profile are in effect, the instance URL, the
credential file (where it is, whether its permissions are sound, which kinds each profile holds — never
the credentials themselves), the credential a request would actually send, whether the instance answers,
which operation table this binary dispatches from, how that table differs from the online API
description — operations you are missing, operations upstream has withdrawn, and operations upstream has
deprecated while this build still offers them — and last, whether the agent skill installed on this
machine still matches this binary.

`otl doctor` invents no exit code. It prints the whole report, then exits with the code the first
blocking finding would have produced in any other command: **0** when nothing is blocking, **2** for
something to fix locally, **4** when the instance rejected the credential, and **1**/**3**/**5**/**6**/**7**/**8**
for whatever the instance did to the probe. Warnings — a discarded spec cache, a table behind upstream, an
unreachable spec host, a credential directory other users can write to around a sound `0600` file, an
agent skill installed at an older version — are
reported and leave the exit code at 0, because none of them stops `otl` from working. Both requests happen
only because you typed the command, and `--offline` skips them.

## Design

**Two crates.** `engine` is a generic OpenAPI RPC client with no knowledge of Outline whatsoever — the
Outline conventions (the `/api` path prefix, the `data`/`pagination` envelope) live entirely in the `otl`
layer and reach the engine as data. The boundary is enforced in review, not just by convention.

**One request channel.** Every API request goes through a single private `send()`: local validation,
rate-limit backoff, global throttling, pagination, error mapping, and credential redaction are each
implemented exactly once. `otl spec sync` adds the one other outbound call in the codebase — a plain
unauthenticated GET of a public document, deliberately kept apart from the channel that carries your
token — and a test confines both to their modules. `otl doctor` adds no third: it asks the request
channel for one `auth.info` and calls `spec sync`'s own fetcher for the document comparison.

**No runtime spec parsing.** `build.rs` compiles the vendored spec into a static IR table. The binary
contains neither the spec file nor its path, which a test asserts against the built artifact. The single
exception is the spec lifecycle module: `otl spec sync` parses a document once on the command you typed
and stores the compiled IR as a binary cache, and `otl doctor` reuses that same entry point to compile a
document in memory for its comparison (writing nothing). Every other command only deserializes the cache,
so `otl --help` stays a few milliseconds. A cache that is damaged or was written by another version is discarded with a warning and
the built-in spec takes over — it can never make the CLI unusable.

**Server text is never printed verbatim — on the human-readable paths.** Document titles, operation
summaries, profile names and paths all reach a terminal, and control characters are only the obvious half
of the problem: an unterminated `U+202E` reverses the visual order of everything after it, and zero-width
characters hide inside a value. One classification (`otl::text`) covers control, bidi, invisible and
joiner characters, and each surface decides what to do with each category: a diagnostic replaces all of
them with a visible marker, a table cell turns controls into spaces, marks what has scope, drops what is
invisible, and keeps the zero-width joiner that emoji ligatures and Persian spelling depend on. `--json`
is a deliberate exemption — it is the payload, and its contract is to round-trip what the server sent, so
it is emitted unchanged.

**Output is two-state.** Data goes to stdout, diagnostics to stderr, always. On a terminal you get a
table whose columns come from the operation's response schema — one generic policy over the schema's own
facets (identity, writable label, timestamps), so the same operation always renders the same columns and
no endpoint has rendering code of its own — with widths measured in grapheme clusters; piped or with
`--json`, you get raw JSON for `jq`. A reader that closes the pipe early (`otl ... | head -1`) is normal
completion, not a crash.

**Pagination never truncates silently.** List operations page automatically; any cap — `--limit`, a page
ceiling, an exhausted offset space — produces an explicit stderr warning, and the wording distinguishes
"definitely truncated" from "may be truncated".

**Exit codes are a public API.** See [Stability and versioning](#stability-and-versioning) below.
Published codes never change meaning.

## Stability and versioning

`otl` follows [semantic versioning](https://semver.org/), and is explicit about which surfaces the
version number is a promise about. While the version is `0.x` the promise is intent, not yet a
guarantee: breaking changes may land in a minor release until `1.0`.

**Covered by semver** — a breaking change here requires a major version:

- The **curated commands** (`otl docs search`, `otl docs get`, …): their names, their flags, and the
  shape of their output, both the human-readable rendering and `--json`. "Shape" means which fields
  exist and with what types — not the order object keys are serialized in, which JSON itself leaves
  unordered. `--json` round-trips what the server sent, key order included.
- The **exit-code table** below. Published codes never change meaning; new error classes get new codes.
- Environment variables (`OUTLINE_URL`, `OUTLINE_API_KEY`, …) and the location and key names of the
  config and credential files.

**Not covered by semver** — these may change in any release:

- **`otl api` output.** The generic escape hatch is explicitly unstable. It prints whatever the Outline
  API returned, so its shape is the *server's* contract, not this CLI's: it changes when your Outline
  instance changes, when the vendored OpenAPI spec is updated, and when `otl spec sync` pulls a newer
  spec on your machine. The set of operations `otl api` exposes shifts for the same reason. Scripts that
  need a stable interface should use a curated command; scripts that use `otl api` should pin a version
  of `otl` and re-check on upgrade.
- Diagnostic wording on stderr, including warning and error message text.
- Which columns the generic table renderer picks, and how it lays them out.
- Anything on disk that is a cache rather than configuration (the spec/IR cache and its format).

### Exit codes

Full detail, including which errors map to which code, is in
[docs/exit-codes.md](docs/exit-codes.md) — the table there is the source of truth and this one is
checked against it by `cargo test`, so the two cannot drift.

<!-- BEGIN GENERATED EXIT CODES: regenerate with `UPDATE_EXIT_CODE_TABLES=1 cargo test -p outline-cli --test exit_code_tables` -->

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Generic failure |
| 2 | Usage or configuration error |
| 3 | API request rejected |
| 4 | Authentication or permission error |
| 5 | Resource not found |
| 6 | Server error |
| 7 | Network error |
| 8 | Rate limited |
| 9 | Partial failure |

<!-- END GENERATED EXIT CODES -->

A closed stdout pipe is not a failure: `otl ... | head -1` exits **0**.

In JSON mode the failure is also an object on **stderr**, while stdout stays empty:

```json
{ "error": { "exit_code": 2, "code": "usage", "message": "OUTLINE_URL is not set.\n..." } }
```

`code` is the name of the same numeric class, not a second taxonomy — renaming one is exactly as
breaking as changing what a number means. Two things deliberately keep the prose form: argument
errors caught by the argument parser itself (an unknown flag, a missing required option), whose
usage synopsis and suggestion are the useful part, and warnings that do not end the command (a
truncation notice, the plaintext-key notice). So a caller must not assume the object is always
there — the exit code is the fact that always is.

## Configuration and profiles

Configuration comes from three layers, resolved **flag > environment > user config file, key by key** —
an `OUTLINE_URL` in the environment does not discard the rest of the selected profile:

```toml
# config.toml in your config directory (~/.config/outline-cli on Linux/macOS,
# %APPDATA%\outline-cli\config on Windows). `otl --config FILE` overrides it.
default_profile = "work"

[profiles.work]
url = "https://outline.example.com"
auth = "api-key"                      # or "oauth", for `otl auth login`

[profiles.personal]
url = "https://notes.example.net"
```

```sh
otl --profile personal api documents.list     # or: OUTLINE_PROFILE=personal
otl --url https://other.example.com api auth.info
OUTLINE_CONFIG= otl api auth.info             # empty value: ignore the config file entirely
```

**Credentials are scoped to their instance.** A profile names a server, so its key is read from that
profile's own variable and from nowhere else:

```sh
export OUTLINE_API_KEY=...                    # used only when no profile is in effect
export OUTLINE_API_KEY_WORK=...               # used by --profile work
export OUTLINE_API_KEY_PERSONAL=...           # used by --profile personal
```

The name is `OUTLINE_API_KEY_` plus the profile name upper-cased, with anything other than an ASCII
letter or digit becoming `_` (`self-hosted` → `OUTLINE_API_KEY_SELF_HOSTED`). A profile never falls back
to the global `OUTLINE_API_KEY`: falling back would send the key that happens to be exported to whichever
instance the selected profile points at, which is one workspace's credential going to another
workspace's server. When a profile is missing its key, `otl` says which variable to set and exits 2
without making a request.

The same rule applies to the *other* half of a request, without bending the precedence model.
Resolution is always flag > env > file, for the base URL as for every other key. What changes is that
the credential is only handed to the request channel once it is bound to the origin that request will
use: with a profile in effect, `otl` releases the key when the base URL came from the profile's own
`url` or from `--url` (stated in the same command, so the redirect is deliberate), and refuses when it
came from `OUTLINE_URL` and names a different instance — or when the profile declares no `url` at all,
leaving nothing to bind to. An ambient variable left over from an earlier shell session must not be able
to point a profile's credential at a server the profile never named, and a warning would not help: a
credential that has been sent cannot be recalled. Origins are compared normalized, so a trailing slash,
host casing or a default port is never a false conflict. Without a profile there is nothing to bind and
`OUTLINE_URL` behaves exactly as before.

The gate is enforced by the type system rather than by convention, and that turns out to be a question of
module layout rather than of the `pub` keyword — a private field in Rust is visible to the declaring
module *and every descendant of it*. So the three pieces of state live in separate leaf modules, none an
ancestor of another: resolved settings can only be produced by the resolver, the credential can only be
read by the source that the gate calls, and the token proving the check ran can only be minted by the
gate. A credential source added later inherits all of it without opting in.

The config file holds no secrets, by construction: an `api_key` or `token` key — at the top level or in a
profile — is a hard error pointing at `credentials.toml`, and any other unrecognized key (including a
deeper table holding one) is rejected as an unknown key. A missing config file is not an error — the
environment-only path works on a fresh machine — but a file named explicitly with
`--config`/`OUTLINE_CONFIG` must exist, and an unknown key, an unknown profile, or malformed TOML fails
with exit code 2 before any request. Parse diagnostics are built from a line number, a description `otl`
owns, and the schema itself — never from the TOML parser's own text, which quotes the offending value —
so a secret wrongly placed in the file is never echoed back.

## Shell completions

```sh
otl completions zsh > ~/.zfunc/_otl          # bash, zsh, fish, powershell, elvish
```

For zsh this file must keep its `#compdef otl` first line — `compinit` reads only that line when it scans
`$fpath` — so the coverage comment below is placed after it rather than above.

Candidates are generated from the same command tree the binary parses with, so subcommands and flags can
never drift from the build; `otl api` operation names come from the compiled IR table. bash, zsh and fish
complete operation names; powershell and elvish get subcommands and flags only, because their upstream
generators emit no candidates for positional arguments. Every generated script states its own coverage in
a header comment, so an installed file never over-claims.

Completion scripts are executable code, so candidate text is constrained rather than trusted: an
operation name must be a plain `resource.method` token (ASCII letters, digits, `.`, `_`, `-`) or it is
not written at all, and the build fails outright if the vendored spec ever contains one that is not.

## The agent skill

An agent driving `otl` has to learn three things no single `--help` page states: which commands are
stable, how to read an operation's contract without calling it, and what to do about the environment when
a command exits 2 or 4. That document ships inside the binary and installs as a skill:

```sh
otl skill install                      # every agent skills directory that exists under your home
otl skill install --dir ~/.claude/skills   # or one you name (OUTLINE_SKILL_DIR does the same)
otl skill show                         # print the document itself
```

Without `--dir`, the targets are the skills directories of the agents already installed here
(`~/.claude`, `~/.codex`, and the agent-agnostic `~/.agents`); a directory for an agent you do not run is
never created. Installing is a local file copy — nothing is fetched, no credential is needed, and the
document is the one compiled into this binary, so its version is the version of the CLI it describes.

The document carries that version in its own frontmatter, which is the single place it is authored, and
`otl doctor`'s `skill` check compares each installed copy against it: `ok` when they match or when none
is installed, `warn` (never blocking) when a copy is behind, was edited, declares no version, is another
skill's document, or sits at a path that cannot hold one, and `skipped` when the machine has no agent
skills directory at all. Each state carries its own `remedy` in the report, because they are not answered
by the same command — a foreign document needs `--force`, and an unusable path needs a look rather than a
reinstall.

Its own document is the only thing an install overwrites. Another skill's `SKILL.md` needs `--force`; a
document path that is not a regular file, and a `<skills dir>/outline-cli` that is a symlink, are refused
whatever the flags say — that directory is one this command creates, so a link there would redirect a
write it believes is local. The skills directory above it may be a symlink: that one is yours.

The document is a contract with something that will not question it: an agent reads it *instead of*
experimenting, so a line that has gone stale is an instruction rather than a typo. Three tests hold it
to the binary, and each of them exists because the corresponding mistake had already been made:

- `crates/otl/tests/skill_surface.rs` extracts every `otl …` line from its fenced code blocks and
  checks the subcommand path, every flag and every value-enum against the real command tree, plus
  every `OUTLINE_*` variable it names against the source. It exists because a hand review found the
  document claiming that `collections list --json` prints a document count, and that every curated
  command returns "the operation's own item shape" — both true when written, and neither noticed by
  anything for as long as they were not.
- `crates/otl/tests/exit_code_tables.rs` generates its exit-code table from `docs/exit-codes.md`,
  which is the same source `README.md`'s copy comes from. Before that, the skill's table was the one
  copy nothing compared to anything.
- `crates/otl/tests/help_coverage.rs` is upstream of both: it requires every argument in the tree to
  carry help text, and every data-printing command to declare its `JSON shape:`. A document can only
  be accurate about a surface that describes itself.

## Credential handling

Credentials live in a dedicated `credentials.toml` under your config directory, kept separate from
`config.toml` so configuration stays shareable while credentials do not travel with it. The file is
created with `0600` at creation time (never created-then-chmod'd), refused if its permissions are ever
widened, and written atomically. On Windows, where POSIX permission bits do not exist, protection comes
from the user profile directory's ACL and the CLI says so rather than claiming otherwise.

Credentials never appear anywhere else — not in logs, error messages, debug output, or `doctor` reports.
That invariant is enforced structurally: server-provided error text is checked against a normalized
skeleton of the token before printing, so interleaving invisible or visible separators into a reflected
credential does not smuggle it to your terminal, and URLs in diagnostics are reduced to their origin so a
secret in a URL path cannot leak. For `--body` requests the server's free-form message is withheld by
default, because it may quote a request body containing secrets; `--show-server-message` opts back in.

Storing secrets in a plaintext file is a deliberate trade-off, chosen for headless, SSH, container, and
scripted use where a system keyring is unavailable or interactive. Its security rests on file permissions
and full-disk encryption.

## Development

```sh
cargo test --workspace                                  # unit, wiremock, golden-file, and CLI tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
bash scripts/win-check.sh                               # lints the retained cfg(windows) source
bash scripts/bench-startup.sh                           # asserts otl --help stays under 10ms
bash scripts/check-binary-size.sh                       # per-target size budget (both Apple triples)
bash scripts/check-all.sh                               # all of the above, with real exit statuses
```

`win-check.sh` needs a word of explanation now that Windows is not shipped. Windows is not built, tested
or published, but the `#[cfg(windows)]` branches and `tests/portability.rs` are deliberately kept so
re-adding the platform is an edit rather than a rewrite — and macOS never compiles those branches, so
without this they would rot unnoticed. A `#[cfg(unix)]` block leaves imports, `mut` bindings and whole
functions unused on Windows, and `cargo test`/`cargo fmt` cannot see any of it; that is how two
`doc_lazy_continuation` violations reached the tree before. It is clippy-only, so it needs no Windows
machine, and it also runs in CI (`windows-source-lint`) rather than depending on anyone remembering the
flag. Run it after splitting or adding a file that carries a `cfg`.

Releasing is `git tag`: [`dist-workspace.toml`](dist-workspace.toml) is the single description of every
distribution channel, and `.github/workflows/release.yml` is generated from it by
[cargo-dist](https://axodotdev.github.io/cargo-dist) — edit the config and run `dist generate`, never the
workflow. (`crates/otl/wix/main.wxs` is the one generated file that is now hand-maintained; see the
comment inside it.)

Two things guard a release, and both are wired so that failing them actually stops one:

- **`release-guards.yml`** runs alongside the build matrix as one of dist's `local-artifacts-jobs`. It
  verifies the cargo-dist installer against a committed checksum before running it, checks that the
  generated workflow is in sync, that every action is pinned to a commit SHA, that all four artifacts are
  planned, that no updater has crept in, and that the Homebrew tap and its token actually exist.
- **The binary-size budget** runs *inside* dist's own build job (injected via
  `.github/build-setup/release-build-setup.yml`), once per published target. `binary-size.yml` runs the
  same script on pull requests for early feedback.

Both are wired to the only two things that stop a release, which is a narrower set than it looks:
`host` accepts a *skipped* dependency and only rejects a *failed* one, so a guard that merely gets
skipped changes nothing. Failing `release-guards` or failing a build job skips `host`, which skips
`announce` — and the GitHub Release is created in `announce`, so nothing is published.
`scripts/check-release-gating.sh` asserts that chain against the generated workflow on every run, so a
cargo-dist upgrade cannot quietly unhook it.

CI builds and tests on macOS — the only shipped platform — guards the startup budget there rather than on
a machine nobody ships, lints the retained Windows source without a Windows runner, and asserts that no
YAML/OpenAPI parser enters the runtime dependency graph of either shipped target (that graph is
platform-specific, so the check names its targets instead of inheriting the runner's). Contract tests
against a real workspace run only on pushes to `main`/`develop`, gated on repository secrets, and are
skipped when those are absent.

`scripts/test_oauth.py` is a stdlib-only probe for the OAuth flow (discovery, dynamic client
registration, PKCE, loopback redirect, refresh, revocation) against a real instance:

```sh
OUTLINE_URL=https://outline.example.com python3 scripts/test_oauth.py
```

The planning documents — [`specs/spec-outline-cli/`](specs/spec-outline-cli/),
[`planning/epics.md`](planning/epics.md), [`project-context.md`](project-context.md) — record the
machine contract, the story breakdown, and the rules that implementations must follow.

## Non-goals

Bidirectional markdown sync, a TUI, event watching, an MCP server mode, OAuth device flow, an offline
write queue, and full typed code generation are all explicitly out of scope.

## License

MIT
