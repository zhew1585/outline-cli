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

Releases are cut from git tags and publish one static binary per platform, plus a Homebrew formula, a
shell installer, and a Windows MSI. There is no published release yet — the commands below are the
contract the release pipeline implements, and they start working with the first tag.

**Homebrew** (macOS and Linux):

```sh
brew install zhew1585/tap/outline-cli
```

**Shell installer** (macOS and Linux; installs into `$CARGO_HOME/bin`):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/zhew1585/outline-cli/releases/latest/download/outline-cli-installer.sh | sh
```

**Windows**: download `outline-cli-x86_64-pc-windows-msvc.msi` from the
[latest release](https://github.com/zhew1585/outline-cli/releases/latest) and run it. The MSI adds
`otl` to `PATH` and uninstalls through Settings → Apps like any other Windows program. It is **not
code-signed** — SmartScreen will warn on first run, and "More info → Run anyway" is the way past it.
Signing needs a purchased certificate; until there is one, verify the download with the attestation
below rather than trusting the publisher prompt.

Prebuilt archives are attached to every release for these targets:

| Platform | Target triple | Notes |
|----------|---------------|-------|
| macOS (Apple Silicon) | `aarch64-apple-darwin` | |
| macOS (Intel) | `x86_64-apple-darwin` | |
| Linux (x86-64) | `x86_64-unknown-linux-musl` | statically linked, no glibc version floor |
| Windows (x86-64) | `x86_64-pc-windows-msvc` | also shipped as an MSI |

**Verifying a download.** Every release artifact — the per-platform archives, the MSI, and also the shell
installer, the Homebrew formula and `sha256.sum` — carries a GitHub build attestation, so you can check
it was produced by this repository's release workflow rather than merely that it matches a checksum
published alongside it:

```sh
gh attestation verify outline-cli-aarch64-apple-darwin.tar.xz --repo zhew1585/outline-cli
```

**`otl` never checks for updates.** No telemetry, no update ping, no background spec fetch — the binary
makes exactly the network requests your command implies. Upgrading is something you do: `brew upgrade`,
re-run the shell installer, or install the newer MSI.

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
  shape of their output, both the human-readable rendering and `--json`.
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

<!-- BEGIN GENERATED EXIT CODES: regenerate with `UPDATE_README_EXIT_CODES=1 cargo test -p outline-cli --test readme_exit_codes` -->

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

<!-- END GENERATED EXIT CODES -->

A closed stdout pipe is not a failure: `otl ... | head -1` exits **0**.

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
bash scripts/check-binary-size.sh                       # asserts the shipped binary stays under 4 MiB
```

Releasing is `git tag`: [`dist-workspace.toml`](dist-workspace.toml) is the single description of every
distribution channel, and `.github/workflows/release.yml` is generated from it by
[cargo-dist](https://axodotdev.github.io/cargo-dist) — edit the config and run `dist generate`, never the
workflow. (`crates/otl/wix/main.wxs` is the one generated file that is now hand-maintained; see the
comment inside it.)

Two things guard a release, and both are wired so that failing them actually stops one:

- **`release-guards.yml`** runs alongside the build matrix as one of dist's `local-artifacts-jobs`. It
  verifies the cargo-dist installer against a committed checksum before running it, checks that the
  generated workflow is in sync and has not drifted from dist's WiX template, that every action is pinned
  to a commit SHA, that all six artifacts are planned, that no updater has crept in, that the version can
  be expressed as an MSI, and that the Homebrew tap and its token actually exist.
- **The binary-size budget** runs *inside* dist's own build job (injected via
  `.github/build-setup/release-build-setup.yml`), once per published target. `binary-size.yml` runs the
  same script on pull requests for early feedback.

Both are wired to the only two things that stop a release, which is a narrower set than it looks:
`host` accepts a *skipped* dependency and only rejects a *failed* one, so a guard that merely gets
skipped changes nothing. Failing `release-guards` or failing a build job skips `host`, which skips
`announce` — and the GitHub Release is created in `announce`, so nothing is published.
`scripts/check-release-gating.sh` asserts that chain against the generated workflow on every run, so a
cargo-dist upgrade cannot quietly unhook it.

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
