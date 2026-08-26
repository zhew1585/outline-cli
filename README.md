# outline-cli (`otl`)

A fast, single-binary CLI for [Outline](https://www.getoutline.com/) knowledge bases.

Outline's API is pure RPC — every endpoint is `POST /api/resource.method` — so the OpenAPI spec is the
contract. `otl` compiles that spec into a static table at build time and interprets it at runtime, which
means **every** endpoint is callable without hand-written per-endpoint code, and cold start stays in
single-digit milliseconds.

> **Status: work in progress.** The engine (Epic 1) is complete: authentication with an API key, the
> generic `otl api` escape hatch, schema validation, dual-state output, auto-pagination, and rate-limit
> backoff. OAuth login, the six polished day-to-day commands, and multi-workspace profiles are next.
> Command surfaces may still change before 1.0.

## Install

Releases are cut from git tags and publish one static binary per platform, plus a Homebrew formula, a
shell installer, and a Windows MSI. There is no published release yet — the commands below are the
contract the release pipeline implements, and they start working with the first tag.

**Homebrew** (macOS and Linux):

```sh
brew install weizhesafeheron/tap/outline-cli
```

**Shell installer** (macOS and Linux; installs into `$CARGO_HOME/bin`):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/weizhesafeheron/outline-cli/releases/latest/download/outline-cli-installer.sh | sh
```

**Windows**: download `outline-cli-x86_64-pc-windows-msvc.msi` from the
[latest release](https://github.com/weizhesafeheron/outline-cli/releases/latest) and run it. The MSI adds
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

**Verifying a download.** Every release archive carries a GitHub build attestation, so you can check an
artifact was produced by this repository's release workflow rather than merely that it matches a checksum
published alongside it:

```sh
gh attestation verify outline-cli-aarch64-apple-darwin.tar.xz --repo weizhesafeheron/outline-cli
```

**`otl` never checks for updates.** No telemetry, no update ping, no background spec fetch — the binary
makes exactly the network requests your command implies. Upgrading is something you do: `brew upgrade`,
re-run the shell installer, or install the newer MSI.

**From source** (Rust stable, `rust-version` 1.85):

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
table with columns picked from the data and widths measured in grapheme clusters; piped or with
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

- **`release-guards.yml`** runs as cargo-dist's plan-phase job, which every later job depends on. It
  verifies the cargo-dist installer against a committed checksum before running it, checks that the
  generated workflow is in sync, that every action is pinned to a commit SHA, that all six artifacts are
  planned, that no updater has crept in, that the version can be expressed as an MSI, and that the
  Homebrew tap and its token actually exist.
- **The binary-size budget** runs *inside* dist's own build job (injected via
  `.github/build-setup/release-build-setup.yml`), once per published target. Failing it fails that job,
  which skips `host`, which skips `announce` — and the GitHub Release is created in `announce`, so
  nothing is published. `binary-size.yml` runs the same script on pull requests for early feedback.

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
