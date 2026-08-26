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
on a loopback port. Access tokens are then renewed inside the request channel, so no command ever fails
because a token aged out. Outline rotates the refresh token on every use, so renewal takes an advisory
file lock: concurrent `otl` processes refresh once between them rather than invalidating each other.

`otl auth logout` needs no `OUTLINE_URL`: every server it contacts comes out of the credential file,
anchored to the origin each credential recorded for itself. It exits non-zero if a server-side step did
not happen, and — when a retry could still succeed — keeps the credentials rather than leaving you with a
token that is live on the server and unrevocable from here. `--force` overrides that.

When several credentials exist for a profile, the order is OAuth session, then the credential file's API
key, then `OUTLINE_API_KEY`. Using the environment variable prints a one-time note about where plaintext
in the environment tends to end up; `OUTLINE_NO_KEY_WARNING=1` silences it.

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

**Exit codes are a public API.** See [docs/exit-codes.md](docs/exit-codes.md). Published codes never
change meaning.

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
