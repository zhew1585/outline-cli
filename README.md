# outline-cli (`otl`)

A fast, single-binary CLI for [Outline](https://www.getoutline.com/) knowledge bases.

Outline's API is pure RPC — every endpoint is `POST /api/resource.method` — so the OpenAPI spec is the
contract. `otl` compiles that spec into a static table at build time and interprets it at runtime, which
means **every** endpoint is callable without hand-written per-endpoint code, and cold start stays in
single-digit milliseconds.

> **Status: work in progress.** The engine (Epic 1) is complete: authentication with an API key, the
> generic `otl api` escape hatch, schema validation, dual-state output, auto-pagination, and rate-limit
> backoff. Multi-workspace profiles and shell completions are in place too. OAuth login and the six
> polished day-to-day commands are next. Command surfaces may still change before 1.0.

## Install

No published release yet. Build from source (Rust stable):

```sh
git clone https://github.com/weizhesafeheron/outline-cli
cd outline-cli
cargo build --release
# binary at target/release/otl
```

## Quick start

```sh
export OUTLINE_URL=https://outline.example.com
export OUTLINE_API_KEY=...            # Settings → API in your Outline instance

otl api list                          # every callable operation in the vendored spec
otl api documents.info id=<doc-id>    # call any of them
otl api documents.search query=deploy --json | jq '.[].title'
```

Arguments are `key=value` pairs coerced to the types the spec declares, and validated locally before any
network request is made:

```sh
otl api documents.list limit=25 template=false   # native JSON number and boolean on the wire
otl api documents.info id=not-a-uuid             # exits 2, no request sent
otl api shares.create --body @share.json         # oneOf/anyOf bodies go through --body verbatim
```

## Design

**Two crates.** `engine` is a generic OpenAPI RPC client with no knowledge of Outline whatsoever — the
Outline conventions (the `/api` path prefix, the `data`/`pagination` envelope) live entirely in the `otl`
layer and reach the engine as data. The boundary is enforced in review, not just by convention.

**One request channel.** Every HTTP request goes through a single private `send()`: local validation,
rate-limit backoff, global throttling, pagination, error mapping, and credential redaction are each
implemented exactly once. There is one `.send()` call in the whole crate.

**No runtime spec parsing.** `build.rs` compiles the vendored spec into a static IR table. The binary
contains neither the spec file nor its path, which a test asserts against the built artifact.

**Output is two-state.** Data goes to stdout, diagnostics to stderr, always. On a terminal you get a
table whose columns come from the operation's response schema — one generic policy over the schema's own
facets (identity, writable label, timestamps), so the same operation always renders the same columns and
no endpoint has rendering code of its own — with widths measured in grapheme clusters; piped or with
`--json`, you get raw JSON for `jq`. A reader that closes the pipe early (`otl ... | head -1`) is normal
completion, not a crash.

**Pagination never truncates silently.** List operations page automatically; any cap — `--limit`, a page
ceiling, an exhausted offset space — produces an explicit stderr warning, and the wording distinguishes
"definitely truncated" from "may be truncated".

**Exit codes are a public API.** See [docs/exit-codes.md](docs/exit-codes.md). Published codes never
change meaning.

## Configuration and profiles

Configuration comes from three layers, resolved **flag > environment > user config file, key by key** —
an `OUTLINE_URL` in the environment does not discard the rest of the selected profile:

```toml
# config.toml in your config directory (~/.config/outline-cli on Linux,
# ~/Library/Application Support/outline-cli on macOS,
# %APPDATA%\outline-cli\config on Windows). `otl --config FILE` overrides it.
default_profile = "work"

[profiles.work]
url = "https://outline.example.com"
auth = "api-key"                      # "oauth" arrives with `otl auth login`

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
without making a request. If `OUTLINE_URL` points somewhere other than the selected profile declares,
precedence still applies (env beats the file) but a warning goes to stderr, since the profile's
credential is then travelling to an instance the profile did not name; `--url` is the deliberate way to
redirect a profile and is not warned about.

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

Candidates are generated from the same command tree the binary parses with, so subcommands and flags can
never drift from the build; `otl api` operation names come from the compiled IR table. bash, zsh and fish
complete operation names; powershell and elvish get subcommands and flags only, because their upstream
generators emit no candidates for positional arguments. Every generated script states its own coverage in
a header comment, so an installed file never over-claims.

Completion scripts are executable code, so candidate text is constrained rather than trusted: an
operation name must be a plain `resource.method` token (ASCII letters, digits, `.`, `_`, `-`) or it is
not written at all, and the build fails outright if the vendored spec ever contains one that is not.

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
bash scripts/bench-startup.sh                           # asserts otl --help stays under 10ms
```

CI runs the matrix on macOS, Linux, and Windows, guards the startup budget, and asserts that no
YAML/OpenAPI parser ever enters the runtime dependency graph. Contract tests against a real workspace run
only on pushes to `main`/`develop`, gated on repository secrets, and are skipped when those are absent.

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
